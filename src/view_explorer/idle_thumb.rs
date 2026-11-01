//! 手すき時間のサムネ生成。表示中のフォルダで、画面外（先読みの窓の外）のサムネ未生成のファイルを、
//! 操作が止まっている間だけ、1件ずつ生成してDBキャッシュに貯める。
//!
//! - 条件: 最後の入力から [`IDLE_AFTER`] 以上たち、通常のサムネ処理（先読みキュー・保留）が空で、設定がONのとき。
//!   ビューアー窓が開いている間は動かない（入力はエクスプローラー窓のものだけを見ているので、本を読んでいる間の
//!   ページデコードとディスクを取り合わないため）
//! - 入力があれば新規の送信を止める（送信済みの分は完走させる）。次の手すきで、一覧の先頭から見直す
//! - 対象はID解決済みのファイルだけ（「検証中」はサムネの引っ越しの前提があるため待つ）。ネットワークパスは対象外
//! - 結果はDBに保存されるだけ（テクスチャにはしない。画面に出る時にDBから読む）
//! - 同時実行数は環境変数 `NEKO_IDLE_THUMB_THREADS`（既定1）。計測フィクスチャーの実行中は動かない
//! - 設定→デバッグの「サムネ周りのリファクタリング用ログ」がONの間、開始・100件ごとの進捗・入力による中断・周の完了を
//!   標準出力へ出す（`[idle-thumb]`）。送信が1件も無かった周は出さない
//! - 見直しの1周は、`SCAN_PER_FRAME` 件ずつ状態を確認して進める（UIを重くしない）

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::NekoviewApp;
use crate::cache::ThumbRequest;

/// 最後の入力からこれだけたつと「手すき」とみなす。
const IDLE_AFTER: Duration = Duration::from_secs(3);
/// 1回の呼び出し（1フレーム）で、サムネの有無を確認する最大件数。
const SCAN_PER_FRAME: usize = 16;
/// ID解決待ちで飛ばしたものを見直すまでの間隔。
const RECHECK_AFTER: Duration = Duration::from_secs(1);
const MAX_THREADS: usize = 16;

/// 手すき生成の同時実行数を環境変数から読む（未設定・不正は1）。
pub(super) fn limit_from_env() -> usize {
    std::env::var("NEKO_IDLE_THUMB_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_THREADS)
}

/// 手すき生成の進行状態。
pub(super) struct IdleThumb {
    /// 次に確認する `archives` の位置。
    cursor: usize,
    /// この周で、ID解決待ちのため飛ばしたものがあるか（あれば周の終わりに見直す）。
    deferred: bool,
    /// 今の状態で、確認すべきものが尽きたか。
    done: bool,
    /// この状態を作った時の「最後の入力」。入力が新しくなったら、先頭から見直す。
    seen_activity: Option<Instant>,
    /// 見直しを始めてよい時刻（ID解決待ちの再確認を間引く）。
    recheck_at: Option<Instant>,
    /// 同時実行数の上限。
    limit: usize,
    /// 手すき生成で送って、まだ結果が返っていないパス。通常の「保留が空になった」判定（ハッシュ・サマリー更新）から外す。
    pending: HashSet<PathBuf>,
    /// 前回の報告以降に、1件でも送ったか。
    dirty: bool,
    /// 周が終わったので、ハッシュ・サマリーの更新を1回だけ依頼してほしい。
    report_done: bool,
    /// この周（入力による見直しまでの区間）で送った件数と、最初に送った時刻（ログ用）。
    pass_sent: usize,
    pass_started: Option<Instant>,
    /// この周で送らなかった理由の内訳（ログ用）。
    skipped: Skipped,
}

#[derive(Default, Clone, Copy)]
struct Skipped {
    /// 一覧に表示中のテクスチャがある。
    shown: usize,
    /// 通常の生成が保留中。
    pending: usize,
    /// 生成に失敗済み。
    failed: usize,
    /// ネットワークパス。
    network: usize,
    /// ID解決待ち（検証中）。
    verifying: usize,
    /// DBのサムネが未生成ではない（生成済み・処理中・古い）。
    existing: usize,
}

/// ゲート（設定のログ）が有効な時だけ標準出力へ出す。
fn log(msg: impl FnOnce() -> String) {
    if crate::config::log().thumb_refactor {
        println!("[idle-thumb] {}", msg());
    }
}

impl IdleThumb {
    pub(super) fn new() -> Self {
        Self {
            cursor: 0,
            deferred: false,
            done: false,
            seen_activity: None,
            recheck_at: None,
            limit: limit_from_env(),
            pending: HashSet::new(),
            dirty: false,
            report_done: false,
            pass_sent: 0,
            pass_started: None,
            skipped: Skipped::default(),
        }
    }

    fn restart(&mut self, activity: Instant) {
        if self.pass_sent > 0 && !self.done {
            log(|| format!("入力で中断: 送信 {} 件（+{:.1}s）", self.pass_sent, self.elapsed()));
        }
        self.pass_sent = 0;
        self.pass_started = None;
        self.skipped = Skipped::default();
        self.cursor = 0;
        self.deferred = false;
        self.done = false;
        self.recheck_at = None;
        self.seen_activity = Some(activity);
    }
}

impl IdleThumb {
    /// この周の最初の送信からの経過秒。
    fn elapsed(&self) -> f64 {
        self.pass_started.map_or(0.0, |t| t.elapsed().as_secs_f64())
    }

    /// 1件送った。最初の1件で開始を、100件ごとに進捗をログへ出す。
    fn note_sent(&mut self, dir: &std::path::Path, total: usize) {
        self.dirty = true;
        self.pass_sent += 1;
        if self.pass_started.is_none() {
            self.pass_started = Some(Instant::now());
            log(|| format!("開始: {}（一覧 {} 件、同時 {} 件まで）", dir.display(), total, self.limit));
        } else if self.pass_sent % 100 == 0 {
            log(|| format!("送信 {} 件（+{:.1}s）", self.pass_sent, self.elapsed()));
        }
    }
}

/// 手すきとみなせるか（最後の入力からの経過）。
pub(super) fn is_idle(since_input: Duration) -> bool {
    since_input >= IDLE_AFTER
}

impl NekoviewApp {
    /// 手すき生成を1回進める（毎フレーム、通常のサムネ処理の後に呼ぶ）。
    pub(super) fn pump_idle_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.config.idle_thumb_generation
            || crate::perf_fixture::is_active()
            || self.viewer_is_open()
            || !self.folder_pane_tab.shows_real_dir()
            || self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
            || self.viewing_similar.is_some()
            || self.cache_db.is_none()
            || !matches!(self.scan_state, super::ScanState::Done)
            || self.archives.is_empty()
        {
            return;
        }
        let activity = self.thumb_last_user_activity;
        // 入力があったら、その手すきの間に最初から見直す。
        if self.idle_thumb.seen_activity != Some(activity) {
            self.idle_thumb.restart(activity);
        }
        // 結果が返らないまま捨てられた分（フォルダ移動など）を外す。
        self.idle_thumb.pending.retain(|p| self.thumb_pending.contains(p));
        if self.idle_thumb.done {
            return;
        }
        let since_input = activity.elapsed();
        if !is_idle(since_input) {
            // 手すきになる時刻に、入力が無くても再描画して再開する。
            ctx.request_repaint_after(IDLE_AFTER - since_input);
            return;
        }
        // 通常のサムネ処理が空で、手すき生成の同時実行数に空きがあること。
        if !self.thumb_queued.is_empty()
            || !self.thumb_priority_queue.is_empty()
            || self.normal_thumb_pending() > 0
            || self.idle_thumb.pending.len() >= self.idle_thumb.limit
        {
            return;
        }
        let mut checked = 0;
        while checked < SCAN_PER_FRAME {
            if self.idle_thumb.cursor >= self.archives.len() {
                // 周の終わり。ID解決待ちで飛ばしたものがあれば、少し置いて見直す。
                if self.idle_thumb.deferred && !self.identity_verifying.is_empty() {
                    let due = *self.idle_thumb.recheck_at.get_or_insert_with(|| Instant::now() + RECHECK_AFTER);
                    if Instant::now() < due {
                        ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
                        return;
                    }
                    self.idle_thumb.cursor = 0;
                    self.idle_thumb.deferred = false;
                    self.idle_thumb.recheck_at = None;
                    continue;
                }
                self.idle_thumb.done = true;
                // 生成したものがあれば、ハッシュ・サマリーの更新を1回だけ依頼する。
                if std::mem::take(&mut self.idle_thumb.dirty) {
                    self.idle_thumb.report_done = true;
                }
                if self.idle_thumb.pass_sent > 0 {
                    let (s, total) = (self.idle_thumb.skipped, self.archives.len());
                    log(|| {
                        format!(
                            "周が完了: 送信 {} 件 / 一覧 {} 件（+{:.1}s） 送らなかった内訳: 生成済み等 {}・表示中 {}・保留 {}・失敗 {}・検証中 {}・ネット {}",
                            self.idle_thumb.pass_sent,
                            total,
                            self.idle_thumb.elapsed(),
                            s.existing,
                            s.shown,
                            s.pending,
                            s.failed,
                            s.verifying,
                            s.network,
                        )
                    });
                }
                return;
            }
            let path = self.archives[self.idle_thumb.cursor].clone();
            self.idle_thumb.cursor += 1;
            checked += 1;
            let skip = if self.thumbnails.contains_key(&path) {
                Some(&mut self.idle_thumb.skipped.shown)
            } else if self.thumb_pending.contains(&path) {
                Some(&mut self.idle_thumb.skipped.pending)
            } else if self.thumb_failed.contains(&path) {
                Some(&mut self.idle_thumb.skipped.failed)
            } else if crate::fs::dir::is_gvfs_path(&path) {
                Some(&mut self.idle_thumb.skipped.network)
            } else {
                None
            };
            if let Some(counter) = skip {
                *counter += 1;
                continue;
            }
            if self.identity_verifying.contains(&path) {
                self.idle_thumb.deferred = true;
                self.idle_thumb.skipped.verifying += 1;
                continue;
            }
            if !self.is_thumb_missing(&path) {
                self.idle_thumb.skipped.existing += 1;
                continue;
            }
            if self.send_idle_thumbnail(path) {
                let (dir, total) = (self.current_dir.clone(), self.archives.len());
                self.idle_thumb.note_sent(&dir, total);
                // 同時実行数いっぱいなら、完了を待つ（完了の通知で再描画される）。
                if self.idle_thumb.pending.len() >= self.idle_thumb.limit {
                    return;
                }
            } else {
                // 送信口がふさがっている。位置を戻して、次の機会に回す。
                self.idle_thumb.cursor -= 1;
                return;
            }
        }
        // 確認の途中。次のフレームで続ける。
        ctx.request_repaint();
    }

    /// 1件を生成に回す。結果はDBに保存され、画面には出さない（`thumb_display_requested` に入れない）。
    fn send_idle_thumbnail(&mut self, path: PathBuf) -> bool {
        let selection = path.parent().and_then(|dir| {
            let filename = path.file_name()?.to_str()?;
            self.spread_db.as_ref().and_then(|db| crate::spread_state::read_thumbnail_selection(db, dir, filename))
        });
        let request = ThumbRequest {
            archive_path: path.clone(),
            db: self.cache_db.clone(),
            is_raw_file: self.raw_image_files.contains(&path),
            thumbnail_selection: selection,
            requested_edge: self.config.thumb_size,
            requested_filter: self.config.thumb_filter,
            generation_token: None,
            session_id: self.thumb_session.load(Ordering::Acquire),
            is_tag_preview: false,
        };
        if self.thumb_req_tx.try_send(request).is_err() {
            return false;
        }
        self.idle_thumb.pending.insert(path.clone());
        self.thumb_pending.insert(path);
        true
    }

    /// 手すき生成を除いた、サムネ生成の保留件数。「保留が空になった」判定（ハッシュ・サマリー更新の契機）に使う。
    /// 手すき生成の1件ごとに更新が走らないようにするため。
    pub(super) fn normal_thumb_pending(&self) -> usize {
        self.thumb_pending.iter().filter(|p| !self.idle_thumb.pending.contains(*p)).count()
    }

    /// 結果が返った手すき生成の1件を、保留から外す。
    pub(super) fn idle_thumb_completed(&mut self, path: &PathBuf) {
        self.idle_thumb.pending.remove(path);
    }

    /// 手すき生成の周が終わった合図を1回だけ返す（ハッシュ・サマリーの更新の契機）。
    pub(super) fn take_idle_thumb_report(&mut self) -> bool {
        std::mem::take(&mut self.idle_thumb.report_done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_begins_only_after_the_wait() {
        assert!(!is_idle(Duration::from_millis(2999)));
        assert!(is_idle(Duration::from_secs(3)));
        assert!(is_idle(Duration::from_secs(60)));
    }

    #[test]
    fn restart_rewinds_to_the_first_file_on_new_input() {
        let mut s = IdleThumb::new();
        s.cursor = 40;
        s.deferred = true;
        s.done = true;
        let a = Instant::now();
        s.restart(a);
        assert_eq!((s.cursor, s.deferred, s.done, s.seen_activity), (0, false, false, Some(a)));
    }

    #[test]
    fn limit_is_clamped_and_defaults_to_one() {
        // 環境変数は他のテストと共有されるので、既定値（未設定）の範囲だけを確認する。
        let n = limit_from_env();
        assert!((1..=MAX_THREADS).contains(&n));
    }
}
