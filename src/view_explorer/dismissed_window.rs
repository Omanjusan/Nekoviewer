//! 「消した組」の一覧ウィンドウ。「正常として消した」重複マーカーと、「別物として消した」似ているペアを
//! 種類ごとの節に並べ、行ごとの「元に戻す」ボタンでやり直せる。
//!
//! - 入口は下段フィルタ行の右端の「↩」ボタン（開閉のトグル）
//! - 開くたびに台帳から読み直し、戻した行はすぐ一覧から外す（ウィンドウを開いたままでも、台帳と食い違わない）
//! - 重複は `duplicate_marker::undismiss`、似ているペアは `thumb_hash_store::undismiss_pair`。戻した後は、
//!   表示中のフォルダのマーカーを読み直し、照合を依頼し直す（次の照合から通常の「？」の相手に戻る）
//! - 行は「ペアの2枚のサムネ」＋パス・類似度・日時＋戻すボタン。サムネの大きさと、ホバーでの拡大表示は、リスト表示の設定
//!   （S/M/L/サムネ無し、ホバー時サムネ表示）に従う。メインの表示モードがカードでも、ここは常にリストの設定を使う
//!   （このウィンドウが開いている間は、カードのボタンを選べなくする）
//! - 行のパスはIDのレコードから引く。ファイルの存在確認は、見えている行だけを描画時に一度ずつ行う
//!   （ネットワークドライブ（gvfs）は確認せず、あるものとして扱う）。見つからないファイルは薄く表示する
//! - サムネの取得は、メインの一覧と同じ描画関数を使う。別フォルダのファイルにはキャッシュDBを渡さない
//!   （間違ったフォルダのDBへ保存しないため）。結果の受け取りは、ウィンドウが開いている間だけ一覧外のパスも通す
//!   レコードが無い（IDが掃除された）行は、台帳側が連動して消えるので出ない

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::duplicate_marker;
use crate::file_identity::{self, FileRecord, FileStatus};
use crate::i18n;
use crate::thumb_hash_store;

/// 重複（正常として消した再取得マーカー）の1行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DupRow {
    /// マーカー持ち（新しい側）のファイルID。「元に戻す」の対象。
    pub id: u64,
    pub owner: PathBuf,
    /// 重複先（古い側）のうち、先頭のパス。
    pub first_target: Option<PathBuf>,
    /// 重複先の数（レコードが引けたもの）。
    pub target_count: usize,
    /// マーカーを検出した日時（unix秒）。
    pub detected_at: i64,
    /// 持ち主のファイルが未確認（実体が見つからなかった）か。
    pub unconfirmed: bool,
}

/// 似ているペア（別物として消した組）の1行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SimRow {
    pub pair: (u64, u64),
    pub a: PathBuf,
    pub b: PathBuf,
    /// 消した日時（unix秒）。不明（旧版の表から読んだ分）は 0。
    pub at: i64,
    /// 2つのサムネのハッシュから求めた類似度(%)。どちらかのハッシュが無ければ None。
    pub pct: Option<u8>,
    /// どちらかのファイルが未確認か。
    pub unconfirmed: bool,
}

/// ウィンドウの状態。開いている間だけ持つ。
#[derive(Default)]
pub(super) struct DismissedWindow {
    pub dup: Vec<DupRow>,
    pub sim: Vec<SimRow>,
    /// パス → ファイルが在るか（描画で見えた行を一度だけ確認して覚える）。
    exists: HashMap<PathBuf, bool>,
}

fn path_of(rec: &FileRecord) -> PathBuf {
    file_identity::path_of_key(&rec.path_key)
}

/// 2つのファイルのサムネのハッシュから類似度(%)を求める。どちらかが無い・作れなかったものは None。
fn similarity_of(db: &Arc<Mutex<Database>>, a: u64, b: u64) -> Option<u8> {
    let hash = |id: u64| {
        thumb_hash_store::get(db, id).filter(|r| r.version == crate::thumb_hash::HASH_VERSION).and_then(|r| r.hash)
    };
    Some(crate::thumb_hash::similarity_pct(crate::thumb_hash::distance(hash(a)?, hash(b)?)))
}

/// 台帳から、消した組を全て読む。重複は検出の新しい順、似ているペアは消した日時の新しい順（日時不明は最後尾）。
/// パスが引けない（レコードが無い）行は含めない。
pub(super) fn load_rows(db: &Arc<Mutex<Database>>) -> DismissedWindow {
    let dup_records = duplicate_marker::list_dismissed(db);
    let pairs = thumb_hash_store::list_dismissed_pairs(db);
    let recs: std::collections::HashMap<u64, FileRecord> = {
        let wanted: std::collections::HashSet<u64> = dup_records
            .iter()
            .flat_map(|r| std::iter::once(r.id).chain(r.targets.iter().copied()))
            .chain(pairs.iter().flat_map(|p| [p.pair.0, p.pair.1]))
            .collect();
        let Ok(guard) = db.lock() else { return DismissedWindow::default() };
        let Ok(tx) = guard.begin_read() else { return DismissedWindow::default() };
        wanted.iter().filter_map(|id| file_identity::record_by_id_tx(&tx, *id).map(|r| (*id, r))).collect()
    };
    let dup = dup_records
        .iter()
        .filter_map(|m| {
            let owner = recs.get(&m.id)?;
            let targets: Vec<&FileRecord> = m.targets.iter().filter_map(|t| recs.get(t)).collect();
            Some(DupRow {
                id: m.id,
                owner: path_of(owner),
                first_target: targets.first().map(|r| path_of(r)),
                target_count: targets.len(),
                detected_at: m.detected_at,
                unconfirmed: owner.status == FileStatus::Unconfirmed,
            })
        })
        .collect();
    let sim = pairs
        .iter()
        .filter_map(|p| {
            let (a, b) = (recs.get(&p.pair.0)?, recs.get(&p.pair.1)?);
            Some(SimRow {
                pair: p.pair,
                a: path_of(a),
                b: path_of(b),
                at: p.at,
                pct: similarity_of(db, p.pair.0, p.pair.1),
                unconfirmed: a.status == FileStatus::Unconfirmed || b.status == FileStatus::Unconfirmed,
            })
        })
        .collect();
    DismissedWindow { dup, sim, exists: HashMap::new() }
}

/// unix秒をローカル時刻の `YYYY-MM-DD HH:MM` にする。0 以下は「日時不明」。
fn format_time(secs: i64) -> String {
    if secs <= 0 {
        return i18n::t().dismissed_window_time_unknown().to_owned();
    }
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(secs) else { return "-".to_owned() };
    let t = time::UtcOffset::current_local_offset().map(|o| utc.to_offset(o)).unwrap_or(utc);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute())
}

impl super::NekoviewApp {
    /// 「消した組」ウィンドウを開く／閉じる（下段フィルタ行の↩ボタン）。開くときは台帳から読み直す。
    pub(super) fn toggle_dismissed_window(&mut self) {
        if self.dismissed_window.is_some() {
            self.dismissed_window = None;
        } else if let Some(db) = &self.spread_db {
            self.dismissed_window = Some(load_rows(db));
        }
    }

    /// 戻した後の後始末: 表示中のフォルダのマーカーを読み直し、照合を依頼し直す。
    fn after_dismissal_restored(&mut self) {
        self.reload_dir_state_maps();
        self.request_thumb_hash();
        self.egui_ctx.request_repaint();
    }

    pub(super) fn draw_dismissed_window(&mut self, ctx: &egui::Context) {
        use super::view_mode::{ListSize, ViewMode};
        let Some(mut state) = self.dismissed_window.take() else { return };
        let t = i18n::t();
        // サムネの大きさとホバーは、リスト表示の設定に従う（メインの表示モードがカードでもリストの設定を使う）。
        let (size, hover_popup) = super::list_view::layout_for(ViewMode::List, self.view_mode.list_size, self.view_mode.hover_popup);
        let text_only = size == ListSize::NoThumb;
        let thumb_h = super::list_view::row_height(size, self.config.thumb_size as f32);
        let thumb_w = (thumb_h / std::f32::consts::SQRT_2).round();
        let item_h = if text_only { thumb_h + 8.0 } else { thumb_h + 6.0 };

        enum Item {
            Heading(String),
            Empty,
            Dup(usize),
            Sim(usize),
        }
        let mut items: Vec<Item> = vec![Item::Heading(t.dismissed_window_dup_heading(state.dup.len()))];
        items.extend(if state.dup.is_empty() { vec![Item::Empty] } else { (0..state.dup.len()).map(Item::Dup).collect() });
        items.push(Item::Heading(t.dismissed_window_sim_heading(state.sim.len())));
        items.extend(if state.sim.is_empty() { vec![Item::Empty] } else { (0..state.sim.len()).map(Item::Sim).collect() });

        let mut open = true;
        let mut restore_dup: Option<u64> = None;
        let mut restore_sim: Option<(u64, u64)> = None;
        egui::Window::new(t.dismissed_window_title())
            .open(&mut open)
            .default_size([880.0, 480.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(t.dismissed_window_explain()).weak());
                ui.separator();
                let DismissedWindow { dup, sim, exists } = &mut state;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, item_h, items.len(), |ui, range| {
                    for item in &items[range] {
                        match item {
                            Item::Heading(text) => {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), item_h), egui::Sense::hover());
                                ui.painter().text(
                                    rect.left_center() + egui::vec2(4.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    egui::FontId::proportional(14.0),
                                    ui.visuals().strong_text_color(),
                                );
                            }
                            Item::Empty => {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), item_h), egui::Sense::hover());
                                ui.painter().text(
                                    rect.left_center() + egui::vec2(24.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    t.dismissed_window_empty(),
                                    egui::FontId::proportional(13.0),
                                    ui.visuals().weak_text_color(),
                                );
                            }
                            Item::Dup(i) => {
                                let row = &dup[*i];
                                let target = row.first_target.clone();
                                let second = match (&target, row.target_count) {
                                    (Some(_), n) if n > 1 => t.dismissed_window_others(n - 1),
                                    (None, _) => t.dismissed_window_no_target().to_owned(),
                                    _ => String::new(),
                                };
                                let meta = format_time(row.detected_at);
                                let paths = [Some(row.owner.clone()), target];
                                if self.dismissed_row(ui, exists, DismissedRow {
                                    id: egui::Id::new(("dismissed_dup", row.id)),
                                    icon: RowIcon::Dup,
                                    paths,
                                    second_note: second,
                                    meta,
                                    unconfirmed: row.unconfirmed,
                                    text_only,
                                    hover_popup,
                                    thumb: (thumb_w, thumb_h),
                                    item_h,
                                }) {
                                    restore_dup = Some(row.id);
                                }
                            }
                            Item::Sim(i) => {
                                let row = &sim[*i];
                                let meta = match row.pct {
                                    Some(p) => format!("{p}%  {}", format_time(row.at)),
                                    None => format_time(row.at),
                                };
                                if self.dismissed_row(ui, exists, DismissedRow {
                                    id: egui::Id::new(("dismissed_sim", row.pair)),
                                    icon: RowIcon::Sim,
                                    paths: [Some(row.a.clone()), Some(row.b.clone())],
                                    second_note: String::new(),
                                    meta,
                                    unconfirmed: row.unconfirmed,
                                    text_only,
                                    hover_popup,
                                    thumb: (thumb_w, thumb_h),
                                    item_h,
                                }) {
                                    restore_sim = Some(row.pair);
                                }
                            }
                        }
                    }
                });
            });
        let mut restored = false;
        if let (Some(id), Some(db)) = (restore_dup, self.spread_db.clone()) {
            if duplicate_marker::undismiss(&db, id) {
                state.dup.retain(|r| r.id != id);
                restored = true;
            }
        }
        if let (Some((a, b)), Some(db)) = (restore_sim, self.spread_db.clone()) {
            if thumb_hash_store::undismiss_pair(&db, a, b) {
                state.sim.retain(|r| r.pair != (a, b));
                restored = true;
            }
        }
        if restored {
            self.set_toast(t.dismissed_window_restored_toast().to_owned());
            self.after_dismissal_restored();
        }
        if open {
            self.dismissed_window = Some(state);
        }
    }

    /// 1行を描く。「元に戻す」が押されたら true。左から、種類のアイコン・戻すボタン・サムネ2枚・パスと日時。
    fn dismissed_row(&mut self, ui: &mut egui::Ui, exists: &mut HashMap<PathBuf, bool>, row: DismissedRow) -> bool {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row.item_h), egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return false;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if pointer.is_some_and(|p| rect.contains(p)) {
            ui.painter().rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
        }
        let mut x = rect.min.x + 4.0;
        let cy = rect.center().y;
        // 種類のアイコン。
        let icon_rect = egui::Rect::from_center_size(egui::pos2(x + 9.0, cy), egui::vec2(18.0, 18.0));
        match row.icon {
            RowIcon::Dup => super::dup_marker_ui::paint_icon(ui.painter(), icon_rect, super::dup_marker_ui::MarkerIcon::Dismissed),
            RowIcon::Sim => super::dup_marker_ui::paint_similar_icon(ui.painter(), icon_rect, true),
        }
        x += 24.0;
        // 戻すボタン。
        let btn_rect = egui::Rect::from_center_size(egui::pos2(x + 11.0, cy), egui::vec2(22.0, 22.0));
        let btn = ui.interact(btn_rect, row.id.with("restore"), egui::Sense::click());
        super::dup_marker_ui::paint_restore_icon(
            ui.painter(),
            egui::Rect::from_center_size(btn_rect.center(), egui::vec2(16.0, 16.0)),
            btn.hovered(),
        );
        let pressed = btn.on_hover_text(i18n::t().dismissed_window_restore_tip()).clicked();
        x += 28.0;
        // サムネ2枚（サムネ無しのときは出さない）。見つからないファイルは薄く覆う。
        let weak = ui.visuals().weak_text_color();
        let mut missing = [false, false];
        for (k, path) in row.paths.iter().enumerate() {
            let Some(path) = path else { continue };
            let present = *exists.entry(path.clone()).or_insert_with(|| {
                crate::fs::dir::is_gvfs_path(path) || std::fs::metadata(path).is_ok()
            });
            missing[k] = !present;
            if row.text_only {
                continue;
            }
            let cell = egui::Rect::from_min_size(egui::pos2(x, cy - row.thumb.1 / 2.0), egui::vec2(row.thumb.0, row.thumb.1));
            self.paint_archive_thumbnail(ui, cell, path);
            if !present {
                ui.painter().rect_filled(cell, 4.0, egui::Color32::from_black_alpha(150));
            }
            if row.hover_popup && pointer.is_some_and(|p| cell.contains(p)) {
                self.show_list_hover_thumbnail(ui, path);
            }
            x += row.thumb.0 + 6.0;
        }
        // パス・補足・日時。
        let text_rect = egui::Rect::from_min_max(egui::pos2(x + 4.0, rect.min.y), egui::pos2(rect.max.x - 6.0, rect.max.y));
        let painter = ui.painter().with_clip_rect(text_rect);
        let strong = ui.visuals().text_color();
        let font = egui::FontId::proportional(13.0);
        let color_of = |k: usize| if missing[k] { weak } else { strong };
        let name = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let a = name(&row.paths[0]);
        let b = if row.second_note.is_empty() {
            name(&row.paths[1])
        } else {
            format!("{}  {}", name(&row.paths[1]), row.second_note)
        };
        let unconfirmed = if row.unconfirmed { format!("  {}", i18n::t().dismissed_window_unconfirmed()) } else { String::new() };
        if row.text_only {
            painter.text(
                egui::pos2(text_rect.min.x, cy),
                egui::Align2::LEFT_CENTER,
                format!("{a}  /  {b}   {}{unconfirmed}", row.meta),
                font,
                color_of(0),
            );
        } else {
            let top = egui::pos2(text_rect.min.x, cy - 10.0);
            let bottom = egui::pos2(text_rect.min.x, cy + 10.0);
            painter.text(top, egui::Align2::LEFT_CENTER, &a, font.clone(), color_of(0));
            painter.text(bottom, egui::Align2::LEFT_CENTER, &b, font.clone(), color_of(1));
            painter.text(
                egui::pos2(text_rect.max.x, cy),
                egui::Align2::RIGHT_CENTER,
                format!("{}{unconfirmed}", row.meta),
                egui::FontId::proportional(12.0),
                weak,
            );
        }
        pressed
    }
}

/// 行の種類（左端のアイコン）。
#[derive(Clone, Copy)]
enum RowIcon {
    Dup,
    Sim,
}

/// 行の描画に渡す内容。
struct DismissedRow {
    id: egui::Id,
    icon: RowIcon,
    /// 上段・下段のパス（2枚のサムネに対応）。
    paths: [Option<PathBuf>; 2],
    /// 下段のパスに添える文字（「他N件」など）。
    second_note: String,
    /// 右端に出す、類似度や日時。
    meta: String,
    unconfirmed: bool,
    text_only: bool,
    hover_popup: bool,
    /// サムネ1枚の (幅, 高さ)。
    thumb: (f32, f32),
    item_h: f32,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::spread_state::open_spread_db;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_dismissed_window_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn register(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, fill: u8) -> u64 {
        std::fs::write(dir.join(name), vec![fill; 64]).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        file_identity::ensure_record(db, dir, name).unwrap().id
    }

    #[test]
    fn empty_database_has_no_rows_and_creates_no_tables() {
        let t = TempRoot::new("empty");
        let db = open_spread_db(&t.0).unwrap();
        let w = load_rows(&db);
        assert!(w.dup.is_empty() && w.sim.is_empty());
    }

    #[test]
    fn rows_list_dismissed_duplicates_and_pairs_newest_first_with_paths() {
        let t = TempRoot::new("rows");
        let db = open_spread_db(&t.0).unwrap();
        let (old, new1, new2) =
            (register(&db, &t.0, "old.zip", 1), register(&db, &t.0, "new1.zip", 2), register(&db, &t.0, "new2.zip", 3));
        assert!(duplicate_marker::record_reacquired(&db, new1, &[old], false, 100));
        assert!(duplicate_marker::record_reacquired(&db, new2, &[old, new1], false, 200));
        assert!(duplicate_marker::dismiss(&db, new1));
        assert!(duplicate_marker::dismiss(&db, new2));
        assert!(thumb_hash_store::dismiss_pair(&db, old, new1, 500));
        assert!(thumb_hash_store::dismiss_pair(&db, old, new2, 900));
        let w = load_rows(&db);
        assert_eq!(w.dup.iter().map(|r| r.id).collect::<Vec<_>>(), vec![new2, new1], "検出の新しい順");
        assert_eq!(w.dup[0].target_count, 2);
        assert!(w.dup[0].owner.ends_with("new2.zip"));
        assert!(w.dup[1].first_target.as_deref().unwrap().ends_with("old.zip"));
        assert_eq!(w.sim.iter().map(|r| r.at).collect::<Vec<_>>(), vec![900, 500], "消した日時の新しい順");
        assert_eq!(w.sim[0].pair, (old.min(new2), old.max(new2)));
    }

    #[test]
    fn restoring_removes_the_row_source_in_the_ledger() {
        let t = TempRoot::new("restore");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        assert!(duplicate_marker::record_reacquired(&db, b, &[a], false, 100));
        assert!(duplicate_marker::dismiss(&db, b));
        assert!(thumb_hash_store::dismiss_pair(&db, a, b, 10));
        assert_eq!((load_rows(&db).dup.len(), load_rows(&db).sim.len()), (1, 1));
        assert!(duplicate_marker::undismiss(&db, b));
        assert!(thumb_hash_store::undismiss_pair(&db, a, b));
        let w = load_rows(&db);
        assert!(w.dup.is_empty() && w.sim.is_empty());
    }

    #[test]
    fn rows_without_a_record_are_left_out_and_deleted_ids_vanish_with_their_rows() {
        let t = TempRoot::new("gone");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        assert!(thumb_hash_store::dismiss_pair(&db, a, b, 10));
        assert!(thumb_hash_store::dismiss_pair(&db, a, 9999, 20), "レコードが無いIDのペア");
        let w = load_rows(&db);
        assert_eq!(w.sim.len(), 1, "レコードの無い相手の行は出さない");
        file_identity::delete_id(&db, b).unwrap();
        assert!(load_rows(&db).sim.is_empty(), "IDの掃除でペアも消える");
    }

    #[test]
    fn similarity_comes_from_the_two_saved_hashes_and_is_none_without_them() {
        use crate::thumb_hash::HASH_VERSION;
        use crate::thumb_hash_store::{put_all, HashRow};
        let t = TempRoot::new("pct");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b, c) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2), register(&db, &t.0, "c.zip", 3));
        let row = |hash: Option<u64>| HashRow { version: HASH_VERSION, thumb_mtime: 1, hash };
        // a と b は距離3（95%）。c はハッシュが作れなかった。
        assert!(put_all(&db, &[(a, row(Some(0))), (b, row(Some(0b111))), (c, row(None))]));
        assert!(thumb_hash_store::dismiss_pair(&db, a, b, 30));
        assert!(thumb_hash_store::dismiss_pair(&db, a, c, 20));
        let w = load_rows(&db);
        assert_eq!(w.sim[0].pct, Some(95));
        assert_eq!(w.sim[1].pct, None, "ハッシュが無いペアは類似度なし");
        // 世代が違う行は使わない。
        assert!(put_all(&db, &[(b, HashRow { version: HASH_VERSION + 1, thumb_mtime: 1, hash: Some(0) })]));
        assert_eq!(load_rows(&db).sim[0].pct, None);
    }

    #[test]
    fn unknown_time_is_shown_as_such() {
        assert_eq!(format_time(0), i18n::t().dismissed_window_time_unknown());
        assert_ne!(format_time(1_700_000_000), i18n::t().dismissed_window_time_unknown());
    }
}
