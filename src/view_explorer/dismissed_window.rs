//! 「消した組」の一覧ウィンドウ。「正常として消した」重複マーカーと、「別物として消した」似ているペアを
//! 種類ごとの節に並べ、行ごとの「元に戻す」ボタンでやり直せる。
//!
//! - 入口は下段フィルタ行の右端の「↩」ボタン（開閉のトグル）
//! - 開くたびに台帳から読み直し、戻した行はすぐ一覧から外す（ウィンドウを開いたままでも、台帳と食い違わない）
//! - 重複は `duplicate_marker::undismiss`、似ているペアは `thumb_hash_store::undismiss_pair`。戻した後は、
//!   表示中のフォルダのマーカーを読み直し、照合を依頼し直す（次の照合から通常の「？」の相手に戻る）
//! - 行のパスはIDのレコードから引く。ファイルの存在確認はしない（ネットワークドライブで止まらないように）。
//!   レコードが無い（IDが掃除された）行は、台帳側が連動して消えるので出ない

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
    pub owner: String,
    /// 重複先（古い側）のうち、先頭のパス。
    pub first_target: Option<String>,
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
    pub a: String,
    pub b: String,
    /// 消した日時（unix秒）。不明（旧版の表から読んだ分）は 0。
    pub at: i64,
    /// どちらかのファイルが未確認か。
    pub unconfirmed: bool,
}

/// ウィンドウの状態。開いている間だけ持つ。
#[derive(Default)]
pub(super) struct DismissedWindow {
    pub dup: Vec<DupRow>,
    pub sim: Vec<SimRow>,
}

fn path_text(rec: &FileRecord) -> String {
    file_identity::path_of_key(&rec.path_key).display().to_string()
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
                owner: path_text(owner),
                first_target: targets.first().map(|r| path_text(r)),
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
                a: path_text(a),
                b: path_text(b),
                at: p.at,
                unconfirmed: a.status == FileStatus::Unconfirmed || b.status == FileStatus::Unconfirmed,
            })
        })
        .collect();
    DismissedWindow { dup, sim }
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

/// 行を描き、押されたら true（元に戻す）。ボタンは行の右端。
fn restore_button(ui: &mut egui::Ui) -> bool {
    let side = 20.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    let hovered = response.hovered();
    super::dup_marker_ui::paint_restore_icon(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), hovered);
    response.on_hover_text(i18n::t().dismissed_window_restore_tip()).clicked()
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
        let Some(mut state) = self.dismissed_window.take() else { return };
        let t = i18n::t();
        let mut open = true;
        let mut restore_dup: Option<u64> = None;
        let mut restore_sim: Option<(u64, u64)> = None;
        egui::Window::new(t.dismissed_window_title())
            .open(&mut open)
            .default_size([760.0, 420.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(t.dismissed_window_explain()).weak());
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(egui::RichText::new(t.dismissed_window_dup_heading(state.dup.len())).strong());
                    if state.dup.is_empty() {
                        ui.label(egui::RichText::new(t.dismissed_window_empty()).weak());
                    }
                    for row in &state.dup {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                            super::dup_marker_ui::paint_icon(ui.painter(), rect, super::dup_marker_ui::MarkerIcon::Dismissed);
                            if restore_button(ui) {
                                restore_dup = Some(row.id);
                            }
                            ui.label(egui::RichText::new(format_time(row.detected_at)).weak());
                            let target = match (&row.first_target, row.target_count) {
                                (Some(first), 1) => first.clone(),
                                (Some(first), n) => format!("{first}  {}", t.dismissed_window_others(n - 1)),
                                (None, _) => t.dismissed_window_no_target().to_owned(),
                            };
                            ui.label(format!("{}  ←  {}", row.owner, target));
                            if row.unconfirmed {
                                ui.label(egui::RichText::new(t.dismissed_window_unconfirmed()).weak());
                            }
                        });
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(t.dismissed_window_sim_heading(state.sim.len())).strong());
                    if state.sim.is_empty() {
                        ui.label(egui::RichText::new(t.dismissed_window_empty()).weak());
                    }
                    for row in &state.sim {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                            super::dup_marker_ui::paint_similar_icon(ui.painter(), rect, true);
                            if restore_button(ui) {
                                restore_sim = Some(row.pair);
                            }
                            ui.label(egui::RichText::new(format_time(row.at)).weak());
                            ui.label(format!("{}  ⇄  {}", row.a, row.b));
                            if row.unconfirmed {
                                ui.label(egui::RichText::new(t.dismissed_window_unconfirmed()).weak());
                            }
                        });
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
    fn unknown_time_is_shown_as_such() {
        assert_eq!(format_time(0), i18n::t().dismissed_window_time_unknown());
        assert_ne!(format_time(1_700_000_000), i18n::t().dismissed_window_time_unknown());
    }
}
