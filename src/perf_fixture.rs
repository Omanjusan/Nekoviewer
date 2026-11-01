//! サムネ周りの速度計測フィクスチャー（開発専用・日本語直書き）。
//!
//! 設定→デバッグの「サムネ周りのリファクタリング用ログ」がONの間だけ、カード右クリックのメニューから
//! 「DBをクリアして計測開始」できる。初回ファイル登録の各段（ID解決・サムネ生成・dHash＋類似照合・
//! マーカー確認）が別スレッドで並走するので、段ごとの開始・終了・件数・累計処理時間を集め、
//! 必要な段が全部終わった時点で標準出力に集計を出す。出力はデバッグログ設定に依存しない（`println!`）。
//!
//! - モード: `ThumbOnly`（サムネ生成だけ）と `Full`（全段）。終了条件は、そのモードで必要な段がすべて完了したこと
//! - 記録はグローバルな1セッション。各ワーカーのスレッドからロック1回で書く（計測OFF時は原子フラグ確認のみ）
//! - 累計処理時間は「その段のワーカーが1件の処理に使った時間」の合計。壁時計の所要時間より小さければ待ち（I/O・
//!   ロック・他段との競合）が大きい、スレッド数ぶん大きければ並列で回っている

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// サムネ生成だけ（他の段は抑止する）。
    ThumbOnly,
    /// 全段を動かす。
    Full,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::ThumbOnly => "サムネのみ",
            Mode::Full => "フルサポート",
        }
    }

    /// このモードで完了を待つ段。
    fn required(self) -> &'static [Stage] {
        match self {
            Mode::ThumbOnly => &[Stage::Thumb],
            Mode::Full => &[Stage::IdResolve, Stage::Thumb, Stage::ThumbHash, Stage::Marker],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    IdResolve,
    Thumb,
    ThumbHash,
    Marker,
}

const ALL_STAGES: [Stage; 4] = [Stage::IdResolve, Stage::Thumb, Stage::ThumbHash, Stage::Marker];

impl Stage {
    fn index(self) -> usize {
        ALL_STAGES.iter().position(|s| *s == self).unwrap_or(0)
    }

    fn label(self) -> &'static str {
        match self {
            Stage::IdResolve => "ID解決(FP)  ",
            Stage::Thumb => "サムネ生成   ",
            Stage::ThumbHash => "dHash+類似照合",
            Stage::Marker => "マーカー確認 ",
        }
    }
}

#[derive(Default, Clone)]
struct StageStat {
    count: usize,
    busy: Duration,
    lock_wait: Duration,
    /// 処理の内訳（ラベルごとの累計時間）。段の累計処理時間の中身を見るためのもの。
    details: Vec<(&'static str, Duration)>,
    first_start: Option<Instant>,
    last_end: Option<Instant>,
    done_at: Option<Instant>,
}

/// 計測1回ぶんの記録。時刻は呼び出し側から渡す（単体テストで固定できるように）。
pub struct Session {
    mode: Mode,
    label: String,
    files: usize,
    started: Instant,
    stages: [StageStat; 4],
}

impl Session {
    pub fn new(mode: Mode, label: String, files: usize, now: Instant) -> Self {
        Self { mode, label, files, started: now, stages: Default::default() }
    }

    /// 1件の処理が終わった。`busy` はその処理に使った時間（終了時刻は `now`）。
    pub fn item(&mut self, stage: Stage, busy: Duration, now: Instant) {
        let s = &mut self.stages[stage.index()];
        s.count += 1;
        s.busy += busy;
        let start = now.checked_sub(busy).unwrap_or(now);
        if s.first_start.map_or(true, |f| start < f) {
            s.first_start = Some(start);
        }
        if s.last_end.map_or(true, |l| now > l) {
            s.last_end = Some(now);
        }
    }

    /// 段の処理時間の内訳を足す（`label` ごとに累計）。
    pub fn detail(&mut self, stage: Stage, label: &'static str, d: Duration) {
        let s = &mut self.stages[stage.index()];
        match s.details.iter_mut().find(|(l, _)| *l == label) {
            Some((_, total)) => *total += d,
            None => s.details.push((label, d)),
        }
    }

    /// 100件ごとの途中経過（止まっているのか遅いのかを見分けるため）。
    pub fn progress_line(&self, stage: Stage) -> Option<String> {
        let s = &self.stages[stage.index()];
        (s.count > 0 && s.count % 100 == 0).then(|| {
            format!(
                "[perf-fixture] 途中経過 {} {} 件完了（+{:.1}s）",
                stage.label().trim(),
                s.count,
                s.last_end.map_or(0.0, |t| t.saturating_duration_since(self.started).as_secs_f64())
            )
        })
    }

    /// 必要なのに未完了の段のラベル。
    pub fn unfinished_required(&self) -> Vec<&'static str> {
        self.mode.required().iter().filter(|st| !self.is_done(**st)).map(|st| st.label().trim()).collect()
    }

    pub fn is_done(&self, stage: Stage) -> bool {
        self.stages[stage.index()].done_at.is_some()
    }

    /// DB操作（ロック待ちを含む）に使った時間を足す。
    pub fn lock_wait(&mut self, stage: Stage, waited: Duration) {
        self.stages[stage.index()].lock_wait += waited;
    }

    pub fn stage_done(&mut self, stage: Stage, now: Instant) {
        let s = &mut self.stages[stage.index()];
        if s.done_at.is_none() {
            s.done_at = Some(now);
        }
    }

    /// 必要な段がすべて完了したか。
    pub fn finished(&self) -> bool {
        self.mode.required().iter().all(|st| self.stages[st.index()].done_at.is_some())
    }

    /// 集計の文面（1行1項目）。総時間は、必要な段の完了時刻の最大値。
    pub fn render(&self) -> Vec<String> {
        let rel = |t: Instant| t.saturating_duration_since(self.started).as_secs_f64();
        let total = self
            .mode
            .required()
            .iter()
            .filter_map(|st| self.stages[st.index()].done_at)
            .max()
            .map_or(0.0, rel);
        let mut out = vec![
            format!("[perf-fixture] ==== 計測結果（{}）====", self.mode.label()),
            format!("[perf-fixture] フォルダ: {} / 対象 {} 件", self.label, self.files),
            "[perf-fixture] 注記: 計測中の入力操作・フォルダを開いた直後は並列度が絞られ、結果がぶれる".to_owned(),
            format!("[perf-fixture] 総時間 {total:.3}s"),
        ];
        for st in ALL_STAGES {
            let s = &self.stages[st.index()];
            let required = self.mode.required().contains(&st);
            if s.count == 0 && !required {
                continue;
            }
            let span = match (s.first_start, s.done_at.or(s.last_end)) {
                (Some(a), Some(b)) => format!("+{:.3}s → +{:.3}s", rel(a), rel(b)),
                _ => "（記録なし）".to_owned(),
            };
            let avg_ms = if s.count == 0 { 0.0 } else { s.busy.as_secs_f64() * 1000.0 / s.count as f64 };
            let db_wait = if s.lock_wait.is_zero() {
                String::new()
            } else {
                format!("  DB待ち {:.3}s", s.lock_wait.as_secs_f64())
            };
            out.push(format!(
                "[perf-fixture] {} {}  件数 {}  累計処理 {:.3}s  平均 {:.2}ms/件{}{}",
                st.label(),
                span,
                s.count,
                s.busy.as_secs_f64(),
                avg_ms,
                db_wait,
                if required && s.done_at.is_none() { "  【未完了】" } else { "" },
            ));
            for (label, total) in &s.details {
                let per = if s.count == 0 { 0.0 } else { total.as_secs_f64() * 1000.0 / s.count as f64 };
                out.push(format!(
                    "[perf-fixture]     └ {label}  累計 {:.3}s  平均 {per:.2}ms/件",
                    total.as_secs_f64()
                ));
            }
        }
        out
    }
}

// ── グローバルな1セッション ────────────────────────────────────────────────────
static ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// ゲート（設定のチェックボックス）が有効か。メニューの表示条件。
pub fn enabled() -> bool {
    crate::config::log().thumb_refactor
}

/// 計測中か。ワーカーの記録呼び出しは、まずこれで素通しにする。
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// 今のモード。計測中でなければ `None`（P5のサムネのみ抑止で参照する）。
pub fn mode() -> Option<Mode> {
    if !is_active() {
        return None;
    }
    SESSION.lock().ok()?.as_ref().map(|s| s.mode)
}

/// 計測を始める。前の計測が未完了なら破棄する。
pub fn start(mode: Mode, label: String, files: usize) {
    if let Ok(mut g) = SESSION.lock() {
        if let Some(prev) = g.as_ref() {
            println!("[perf-fixture] 前の計測は未完了のまま破棄（未完了の段: {}）", prev.unfinished_required().join("、"));
        }
        *g = Some(Session::new(mode, label.clone(), files, Instant::now()));
        ACTIVE.store(true, Ordering::Release);
    }
    println!("[perf-fixture] 計測開始（{}）: {} / {} 件", mode.label(), label, files);
}

/// 計測を打ち切る（「戻す」やフォルダ移動などで）。
pub fn cancel() {
    if let Ok(mut g) = SESSION.lock() {
        if g.take().is_some() {
            println!("[perf-fixture] 計測を中止");
        }
        ACTIVE.store(false, Ordering::Release);
    }
}

fn with_session(f: impl FnOnce(&mut Session, Instant)) {
    if !is_active() {
        return;
    }
    let Ok(mut g) = SESSION.lock() else { return };
    let Some(s) = g.as_mut() else { return };
    f(s, Instant::now());
    if s.finished() {
        for line in s.render() {
            println!("{line}");
        }
        *g = None;
        ACTIVE.store(false, Ordering::Release);
    }
}

/// その段が完了済みか。計測中でなければ false。
pub fn is_stage_done(stage: Stage) -> bool {
    is_active() && SESSION.lock().ok().and_then(|g| g.as_ref().map(|s| s.is_done(stage))).unwrap_or(false)
}

/// サムネのみモードか（他の段の起動を抑止する判定）。
pub fn is_thumb_only() -> bool {
    mode() == Some(Mode::ThumbOnly)
}

pub fn item(stage: Stage, busy: Duration) {
    with_session(|s, now| {
        s.item(stage, busy, now);
        if let Some(line) = s.progress_line(stage) {
            println!("{line}");
        }
    });
}

/// 段の処理時間の内訳。`stage` の件数あたりに換算して表示される。
pub fn detail(stage: Stage, label: &'static str, d: Duration) {
    with_session(|s, _| s.detail(stage, label, d));
}

pub fn lock_wait(stage: Stage, waited: Duration) {
    with_session(|s, _| s.lock_wait(stage, waited));
}

pub fn stage_done(stage: Stage) {
    with_session(|s, now| s.stage_done(stage, now));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn thumb_only_finishes_when_thumb_done() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::ThumbOnly, "d".into(), 2, t0);
        assert!(!s.finished());
        s.item(Stage::Thumb, ms(100), t0 + ms(150));
        s.item(Stage::Thumb, ms(100), t0 + ms(300));
        assert!(!s.finished(), "stage_done まで完了にしない");
        s.stage_done(Stage::Thumb, t0 + ms(300));
        assert!(s.finished());
    }

    #[test]
    fn details_accumulate_per_label_and_render_under_the_stage() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::Full, "d".into(), 2, t0);
        s.item(Stage::IdResolve, ms(100), t0 + ms(100));
        s.item(Stage::IdResolve, ms(100), t0 + ms(200));
        s.detail(Stage::IdResolve, "FP読み", ms(120));
        s.detail(Stage::IdResolve, "FP読み", ms(60));
        s.detail(Stage::IdResolve, "DB解決", ms(10));
        let text = s.render().join("\n");
        assert!(text.contains("└ FP読み  累計 0.180s  平均 90.00ms/件"), "{text}");
        assert!(text.contains("└ DB解決  累計 0.010s  平均 5.00ms/件"), "{text}");
    }

    #[test]
    fn is_done_reflects_stage_done() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::Full, "d".into(), 1, t0);
        assert!(!s.is_done(Stage::Thumb));
        s.stage_done(Stage::Thumb, t0 + ms(5));
        assert!(s.is_done(Stage::Thumb));
        assert!(!s.is_done(Stage::ThumbHash));
    }

    #[test]
    fn full_needs_every_required_stage() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::Full, "d".into(), 1, t0);
        for st in [Stage::IdResolve, Stage::Thumb, Stage::ThumbHash] {
            s.stage_done(st, t0 + ms(10));
            assert!(!s.finished());
        }
        s.stage_done(Stage::Marker, t0 + ms(20));
        assert!(s.finished());
    }

    #[test]
    fn render_total_is_latest_required_done_and_stats_accumulate() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::Full, "dir".into(), 2, t0);
        s.item(Stage::Thumb, ms(200), t0 + ms(250));
        s.item(Stage::Thumb, ms(400), t0 + ms(700));
        s.lock_wait(Stage::Thumb, ms(30));
        s.stage_done(Stage::IdResolve, t0 + ms(100));
        s.stage_done(Stage::Thumb, t0 + ms(700));
        s.stage_done(Stage::ThumbHash, t0 + ms(1500));
        s.stage_done(Stage::Marker, t0 + ms(900));
        let text = s.render().join("\n");
        assert!(text.contains("総時間 1.500s"), "{text}");
        assert!(text.contains("件数 2  累計処理 0.600s  平均 300.00ms/件  DB待ち 0.030s"), "{text}");
        assert!(text.contains("+0.050s → +0.700s"), "{text}");
    }

    #[test]
    fn render_marks_unfinished_required_stage_and_hides_unused_optional() {
        let t0 = Instant::now();
        let mut s = Session::new(Mode::ThumbOnly, "d".into(), 1, t0);
        s.item(Stage::Thumb, ms(10), t0 + ms(20));
        let text = s.render().join("\n");
        assert!(text.contains("【未完了】"));
        assert!(!text.contains("dHash"), "未使用の任意段は出さない");
    }
}
