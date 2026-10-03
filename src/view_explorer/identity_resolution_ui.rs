//! 重複・曖昧の解決UI。レイアウト仕様は docs/features/fingerprint-resolution-ui.md。
//!
//! 文言は「このアプリの管理データ（DB）内」の話だと必ず分かるようにする（ストレージ全体の重複検索と誤解させない）。
//!
//! 一覧（第1ダイアログ）は未解決の記録（`identity_pending`）から作り、第2ダイアログで参照元を選んで確定すると
//! `identity_resolve::apply` で解決する。解決後は、★・評価帯・サムネ・見開き等の表示を引き直す。

use egui::{Align, Color32, Layout, RichText};

use super::NekoviewApp;
use crate::file_identity::{path_of_key, probe_record, FileRecord, Presence};
use crate::i18n;
use crate::identity_pending::{self as pending, PendingKind};
use crate::identity_resolve::{self, Choice, ResolveError};

const DAY: i64 = 86_400;

/// ファイルの表示用の情報。
pub(super) struct FileInfo {
    path: String,
    size: u64,
    mtime: i64,
}

pub(super) struct CandidateRow {
    id: u64,
    info: FileInfo,
    state: Presence,
    rating_half: u8,
    tag_count: usize,
    favorite: bool,
    /// 実体が見つからないファイルの、最後に確認された日時（現存は None）。
    last_seen: Option<i64>,
}

pub(super) struct ResolveItem {
    pending_id: u64,
    kind: PendingKind,
    /// 対象（引き継ぎ元の選択・移動元の選択は新しいファイル、お気に入りの引き継ぎは実体の無いお気に入り）。
    subject: FileInfo,
    /// 対象が実体の無いお気に入りのときの、最後に確認された日時。
    subject_last_seen: Option<i64>,
    detected_at: i64,
    candidates: Vec<CandidateRow>,
}

pub(super) struct ResolveUiState {
    open: bool,
    filter: [bool; 3],
    selected: Option<usize>,
    second: Option<Second>,
    items: Vec<ResolveItem>,
}

struct Second {
    item: usize,
    chosen: Option<usize>,
}

impl Default for ResolveUiState {
    fn default() -> Self {
        Self { open: false, filter: [true; 3], selected: None, second: None, items: Vec::new() }
    }
}

// ---- 表示用の整形 ----

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 「ファイル名（…/親フォルダ末尾）」。
fn short_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(path);
    match p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) {
        Some(parent) => format!("{name}  (…/{parent})"),
        None => name.to_owned(),
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1e6)
    } else {
        format!("{}KB", bytes / 1000)
    }
}

fn local_time(secs: i64) -> Option<time::OffsetDateTime> {
    let utc = time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
    Some(
        time::UtcOffset::current_local_offset()
            .map(|o| utc.to_offset(o))
            .unwrap_or(utc),
    )
}

fn format_md(secs: i64) -> String {
    local_time(secs).map_or("-".into(), |t| format!("{:02}/{:02}", u8::from(t.month()), t.day()))
}

fn format_ymd_hm(secs: i64) -> String {
    local_time(secs).map_or("-".into(), |t| {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute())
    })
}

fn format_rating(half: u8) -> String {
    match half {
        0 => "-".to_owned(),
        h if h % 2 == 0 => format!("★{}", h / 2),
        h => format!("★{}.5", h / 2),
    }
}

/// 削除まで残り何日か。実体が見つからないまま保持日数を過ぎると、日数による削除の対象になる。
/// `last_seen` が None（現存ファイル）は期限なし。
fn days_left(last_seen: Option<i64>, retention_days: u32, now: i64) -> Option<i64> {
    let since = last_seen?;
    let elapsed_days = (now - since).max(0) / DAY;
    Some(i64::from(retention_days) - elapsed_days)
}

/// 項目の「削除まで」。お気に入りの引き継ぎは、対象（実体の無いお気に入り）自身の期限。
/// それ以外は、消えている候補のうち、最も早く削除されるものの残り日数（候補が全て現存なら期限なし）。
fn item_days_left(item: &ResolveItem, retention_days: u32, now: i64) -> Option<i64> {
    if item.kind == PendingKind::FavoriteHandover {
        return days_left(item.subject_last_seen, retention_days, now);
    }
    item.candidates
        .iter()
        .filter_map(|c| days_left(c.last_seen, retention_days, now))
        .min()
}

fn days_left_text(days: Option<i64>) -> RichText {
    let t = i18n::t();
    match days {
        None => RichText::new(t.idres_until_none()),
        Some(d) if d <= 0 => RichText::new(t.idres_until_due()).color(Color32::from_rgb(220, 90, 60)),
        Some(d) if d <= 7 => RichText::new(t.idres_until_days(d)).color(Color32::from_rgb(220, 150, 40)),
        Some(d) => RichText::new(t.idres_until_days(d)),
    }
}

fn state_text(state: Presence) -> &'static str {
    let t = i18n::t();
    match state {
        Presence::Present => t.idres_state_present(),
        Presence::Missing => t.idres_state_missing(),
        Presence::Offline => t.idres_state_offline(),
    }
}

fn error_code(e: &ResolveError) -> &'static str {
    match e {
        ResolveError::NotFound => "not_found",
        ResolveError::NotACandidate => "not_candidate",
        ResolveError::SubjectGone => "subject_gone",
        ResolveError::SourceGone => "source_gone",
        ResolveError::SubjectHasData => "subject_has_data",
        ResolveError::NoFavorite => "no_favorite",
        ResolveError::Db => "db",
    }
}

// ---- 一覧の組み立て ----

fn file_info(rec: &FileRecord) -> FileInfo {
    FileInfo { path: path_of_key(&rec.path_key).to_string_lossy().into_owned(), size: rec.size, mtime: rec.mtime }
}

/// 未解決の記録から、表示用の一覧を作る。対象のIDが消えた記録は除く（候補が消えた分は候補から除く）。
fn build_items(db: &std::sync::Arc<std::sync::Mutex<redb::Database>>) -> Vec<ResolveItem> {
    use redb::ReadableDatabase;
    let record = |id: u64| {
        let guard = db.lock().ok()?;
        let tx = guard.begin_read().ok()?;
        crate::file_identity::record_by_id_tx(&tx, id)
    };
    pending::list(db)
        .into_iter()
        .filter_map(|rec| {
            let subject = record(rec.subject)?;
            let candidates: Vec<CandidateRow> = rec
                .candidates
                .iter()
                .filter_map(|id| record(*id))
                .map(|c| {
                    let state = probe_record(&c);
                    let summary = crate::spread_state::record_summary(db, &c);
                    CandidateRow {
                        id: c.id,
                        info: file_info(&c),
                        state,
                        rating_half: summary.rating_half,
                        tag_count: summary.tag_count,
                        favorite: summary.favorite,
                        last_seen: (state != Presence::Present).then_some(c.last_seen),
                    }
                })
                .collect();
            (!candidates.is_empty()).then(|| ResolveItem {
                pending_id: rec.id,
                kind: rec.kind,
                subject_last_seen: (rec.kind == PendingKind::FavoriteHandover).then_some(subject.last_seen),
                subject: file_info(&subject),
                detected_at: rec.detected_at,
                candidates,
            })
        })
        .collect()
}

/// 固定幅のセルを並べた、1行ぶんの選択可能な行。いずれかのセルのクリックで true。
fn selectable_row(ui: &mut egui::Ui, selected: bool, cells: &[(f32, RichText)]) -> egui::Response {
    const ROW_H: f32 = 22.0;
    let mut response: Option<egui::Response> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (width, text) in cells {
            let r = ui.add_sized(
                [*width, ROW_H],
                egui::Button::selectable(selected, text.clone()).truncate(),
            );
            response = Some(match response.take() {
                Some(prev) => prev.union(r),
                None => r,
            });
        }
    });
    response.expect("row has cells")
}

fn header_row(ui: &mut egui::Ui, cells: &[(f32, &str)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (width, label) in cells {
            ui.add_sized([*width, 20.0], egui::Label::new(RichText::new(*label).strong()));
        }
    });
}

impl NekoviewApp {
    /// 未解決の記録から一覧を作り直す。掃除（意味を失った記録の除去）とお気に入りの引き継ぎ候補の検出もここで行う。
    /// 一覧を開く時と、解決の適用後に呼ぶ。
    pub(super) fn reload_identity_resolve_items(&mut self, detect_favorites: bool) {
        let Some(db) = self.spread_db.clone() else {
            self.identity_resolve_ui.items.clear();
            self.identity_pending_count = 0;
            return;
        };
        if detect_favorites {
            pending::detect_favorite_handovers(&db, crate::file_identity::now_unix());
        }
        pending::prune_real(&db);
        self.identity_resolve_ui.items = build_items(&db);
        self.identity_pending_count = pending::count(&db);
        // 選択が指していた項目が無くなっていたら外す。
        let n = self.identity_resolve_ui.items.len();
        if self.identity_resolve_ui.selected.is_some_and(|s| s >= n) {
            self.identity_resolve_ui.selected = None;
        }
    }

    pub(super) fn open_identity_resolve(&mut self) {
        self.reload_identity_resolve_items(true);
        let ui = &mut self.identity_resolve_ui;
        ui.open = true;
        ui.selected = None;
        ui.second = None;
    }

    /// メニューバーの「DB内の重複の解決（n）」ボタンの表示と、押せるか（未解決が無ければ押せない）。
    pub(super) fn identity_resolve_button(&self) -> (String, bool) {
        (i18n::t().idres_button(self.identity_pending_count), self.identity_pending_count > 0)
    }

    fn visible_resolve_items(&self) -> Vec<usize> {
        let ui = &self.identity_resolve_ui;
        ui.items
            .iter()
            .enumerate()
            .filter(|(_, item)| ui.filter[item.kind.index()])
            .map(|(i, _)| i)
            .collect()
    }

    pub(super) fn draw_identity_resolve_dialogs(&mut self, ctx: &egui::Context) {
        if !self.identity_resolve_ui.open {
            return;
        }
        self.draw_resolve_list_dialog(ctx);
        if self.identity_resolve_ui.second.is_some() {
            self.draw_resolve_detail_dialog(ctx);
        }
    }

    /// 第1ダイアログ: 未解決の項目の一覧。
    fn draw_resolve_list_dialog(&mut self, ctx: &egui::Context) {
        let t = i18n::t();
        let retention = self.config.unconfirmed_retention_days;
        let now = crate::file_identity::now_unix();
        let visible = self.visible_resolve_items();
        // 絞り込みで見えなくなった行の選択は外す。
        if let Some(sel) = self.identity_resolve_ui.selected {
            if !visible.contains(&sel) {
                self.identity_resolve_ui.selected = None;
            }
        }
        let second_open = self.identity_resolve_ui.second.is_some();
        // ↑↓で選択を移動し、Enterで解決へ進む（第2ダイアログが開いている間は受けない）。
        let (mut key_move, mut key_enter) = (0i32, false);
        if !second_open {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowDown) {
                    key_move = 1;
                }
                if i.key_pressed(egui::Key::ArrowUp) {
                    key_move = -1;
                }
                key_enter = i.key_pressed(egui::Key::Enter);
            });
        }
        if key_move != 0 && !visible.is_empty() {
            let pos = self.identity_resolve_ui.selected.and_then(|s| visible.iter().position(|v| *v == s));
            let next = match pos {
                None => 0,
                Some(p) => (p as i32 + key_move).clamp(0, visible.len() as i32 - 1) as usize,
            };
            self.identity_resolve_ui.selected = Some(visible[next]);
        }

        let widths = [150.0, 280.0, 44.0, 64.0, 110.0];
        let mut proceed = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("identity_resolve_list")).show(ctx, |ui| {
            ui.set_width(680.0);
            ui.heading(t.idres_list_title());
            ui.label(t.idres_list_intro1());
            ui.label(t.idres_list_intro2());
            ui.weak(t.idres_list_note());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(t.idres_filter_label());
                for kind in PendingKind::ALL {
                    ui.checkbox(&mut self.identity_resolve_ui.filter[kind.index()], t.idres_kind(kind));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(t.idres_shown(visible.len(), self.identity_resolve_ui.items.len()));
                });
            });
            ui.add_space(4.0);
            header_row(
                ui,
                &[
                    (widths[0], t.idres_col_kind()),
                    (widths[1], t.idres_col_target()),
                    (widths[2], t.idres_col_candidates()),
                    (widths[3], t.idres_col_detected()),
                    (widths[4], t.idres_col_until()),
                ],
            );
            ui.separator();
            egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| ui.weak(t.idres_empty()));
                    ui.add_space(24.0);
                }
                for &i in &visible {
                    let item = &self.identity_resolve_ui.items[i];
                    let selected = self.identity_resolve_ui.selected == Some(i);
                    let resp = selectable_row(
                        ui,
                        selected,
                        &[
                            (widths[0], RichText::new(t.idres_kind(item.kind))),
                            (widths[1], RichText::new(short_path(&item.subject.path))),
                            (widths[2], RichText::new(item.candidates.len().to_string())),
                            (widths[3], RichText::new(format_md(item.detected_at))),
                            (widths[4], days_left_text(item_days_left(item, retention, now))),
                        ],
                    )
                    .on_hover_text(&item.subject.path);
                    if resp.clicked() {
                        self.identity_resolve_ui.selected = Some(i);
                    }
                    if resp.double_clicked() {
                        proceed = true;
                    }
                }
            });
            ui.separator();
            ui.weak(t.idres_until_note());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_proceed = self.identity_resolve_ui.selected.is_some();
                if ui.add_enabled(can_proceed, egui::Button::new(t.idres_btn_proceed())).clicked() {
                    proceed = true;
                }
                if ui.button(t.idres_btn_close()).clicked() {
                    close = true;
                }
            });
        });
        if key_enter && self.identity_resolve_ui.selected.is_some() {
            proceed = true;
        }
        if proceed {
            if let Some(item) = self.identity_resolve_ui.selected {
                self.identity_resolve_ui.second = Some(Second { item, chosen: None });
            }
        }
        if (close || modal.should_close()) && !second_open {
            self.identity_resolve_ui.open = false;
        }
    }

    /// 第2ダイアログ: 参照元の選択。
    fn draw_resolve_detail_dialog(&mut self, ctx: &egui::Context) {
        let t = i18n::t();
        let Some(second) = &self.identity_resolve_ui.second else { return };
        let item_index = second.item;
        let chosen = second.chosen;
        let retention = self.config.unconfirmed_retention_days;
        let now = crate::file_identity::now_unix();
        let Some(item) = self.identity_resolve_ui.items.get(item_index) else {
            self.identity_resolve_ui.second = None;
            return;
        };
        let widths = [48.0, 250.0, 76.0, 64.0, 108.0, 44.0, 44.0, 70.0, 96.0];
        let mut new_choice = chosen;
        let mut confirm = false;
        let mut skip = false;
        let mut back = false;
        let target_name = file_name(&item.subject.path).to_owned();
        let modal = egui::Modal::new(egui::Id::new("identity_resolve_detail")).show(ctx, |ui| {
            ui.set_width(800.0);
            ui.heading(t.idres_detail_title(t.idres_kind(item.kind)));
            ui.add_space(4.0);
            ui.label(RichText::new(t.idres_target_label()).strong());
            ui.label(format!(
                "{}   {}   {}",
                item.subject.path,
                format_size(item.subject.size),
                format_ymd_hm(item.subject.mtime)
            ));
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(t.idres_what_happened()).strong());
                ui.label(t.idres_situation(item.kind, &target_name, item.candidates.len()));
                ui.add_space(4.0);
                ui.label(RichText::new(t.idres_why_manual()).strong());
                ui.label(t.idres_reason(item.kind));
                ui.add_space(4.0);
                ui.label(RichText::new(t.idres_what_to_do()).strong());
                ui.label(t.idres_ask(item.kind));
            });
            ui.add_space(8.0);
            ui.label(RichText::new(t.idres_candidates_heading(item.kind)).strong());
            header_row(
                ui,
                &[
                    (widths[0], t.idres_col_select()),
                    (widths[1], t.idres_col_path()),
                    (widths[2], t.idres_col_state()),
                    (widths[3], t.idres_col_size()),
                    (widths[4], t.idres_col_mtime()),
                    (widths[5], t.idres_col_rating()),
                    (widths[6], t.idres_col_tags()),
                    (widths[7], t.idres_col_favorite()),
                    (widths[8], t.idres_col_until()),
                ],
            );
            ui.separator();
            egui::ScrollArea::vertical().max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                for (ci, c) in item.candidates.iter().enumerate() {
                    let selected = new_choice == Some(ci);
                    let resp = selectable_row(
                        ui,
                        selected,
                        &[
                            (widths[0], RichText::new(if selected { "●" } else { "○" })),
                            (widths[1], RichText::new(&c.info.path)),
                            (widths[2], RichText::new(state_text(c.state))),
                            (widths[3], RichText::new(format_size(c.info.size))),
                            (widths[4], RichText::new(format_ymd_hm(c.info.mtime))),
                            (widths[5], RichText::new(format_rating(c.rating_half))),
                            (widths[6], RichText::new(c.tag_count.to_string())),
                            (widths[7], RichText::new(if c.favorite { t.idres_yes() } else { t.idres_no() })),
                            (widths[8], days_left_text(days_left(c.last_seen, retention, now))),
                        ],
                    )
                    .on_hover_text(&c.info.path);
                    if resp.clicked() {
                        new_choice = Some(ci);
                    }
                }
            });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(t.idres_will_confirm()).strong());
                ui.label(t.idres_on_confirm(item.kind, &target_name));
                ui.add_space(4.0);
                ui.label(RichText::new(t.idres_will_skip()).strong());
                ui.label(t.idres_on_skip(item.kind, &target_name));
                ui.add_space(4.0);
                ui.label(RichText::new(t.idres_will_back()).strong());
                ui.label(t.idres_back_text());
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(new_choice.is_some(), egui::Button::new(t.idres_btn_confirm()))
                    .clicked()
                {
                    confirm = true;
                }
                if ui.button(t.idres_btn_skip()).clicked() {
                    skip = true;
                }
                if ui.button(t.idres_btn_back()).clicked() {
                    back = true;
                }
            });
        });
        if let Some(second) = &mut self.identity_resolve_ui.second {
            second.chosen = new_choice;
        }
        if confirm || skip {
            let choice = match (confirm, new_choice) {
                (true, Some(ci)) => item.candidates.get(ci).map(|c| Choice::Source(c.id)),
                _ => Some(Choice::Skip),
            };
            if let Some(choice) = choice {
                self.apply_identity_resolution(item_index, choice);
            }
        } else if back || modal.should_close() {
            self.identity_resolve_ui.second = None;
        }
    }

    /// 解決を適用する。成功したら第2ダイアログを閉じ、一覧と表示（★・評価帯・サムネ・並び）を引き直す。
    fn apply_identity_resolution(&mut self, item_index: usize, choice: Choice) {
        let Some(db) = self.spread_db.clone() else { return };
        let Some(item) = self.identity_resolve_ui.items.get(item_index) else { return };
        let (pending_id, kind) = (item.pending_id, item.kind);
        let subject_path = std::path::PathBuf::from(&item.subject.path);
        let target_name = file_name(&item.subject.path).to_owned();
        let source_path = match choice {
            Choice::Source(id) => item
                .candidates
                .iter()
                .find(|c| c.id == id)
                .map(|c| std::path::PathBuf::from(&c.info.path)),
            Choice::Skip => None,
        };
        let t = i18n::t();
        match identity_resolve::apply(&db, pending_id, choice) {
            Ok(()) => {
                // サムネ: 移動元・複製元に生成済みのサムネがあれば、対象へ引き継ぐ（既にあれば上書きしない）。
                if let (Some(root), Some(src), true) = (
                    self.config.cache_root(),
                    source_path.as_ref(),
                    matches!(kind, PendingKind::CopySource | PendingKind::MoveTarget),
                ) {
                    if let (Some(src_dir), Some(src_name)) = (src.parent(), src.file_name().and_then(|n| n.to_str())) {
                        let key = crate::spread_state::make_key(src_dir, src_name);
                        super::identity_worker::transplant_thumbnail_for_move(&root, &subject_path, &key);
                    }
                }
                self.set_toast(match choice {
                    Choice::Source(_) => t.idres_done(&target_name),
                    Choice::Skip => t.idres_skipped(&target_name),
                });
                self.refresh_after_identity_resolution(&[Some(subject_path), source_path]);
            }
            Err(e) => self.set_toast(t.idres_error(error_code(&e))),
        }
        self.identity_resolve_ui.second = None;
        self.reload_identity_resolve_items(false);
    }

    /// 解決で記録が動いたファイルの、表示中の★・評価帯・保存設定・並びを引き直す。
    fn refresh_after_identity_resolution(&mut self, paths: &[Option<std::path::PathBuf>]) {
        for p in paths.iter().flatten() {
            if self.archive_rating_cache.contains_key(p) {
                self.refresh_rating_cache(p);
                self.refresh_saved_archive_settings(p);
            }
        }
        if self.viewing_favorites.is_none() {
            self.reload_dir_state_maps();
        }
        self.resort_keeping_selection_always();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: PendingKind, subject_last_seen: Option<i64>, candidate_last_seen: &[Option<i64>]) -> ResolveItem {
        let info = |p: &str| FileInfo { path: p.to_owned(), size: 1, mtime: 1 };
        ResolveItem {
            pending_id: 1,
            kind,
            subject: info("/d/new.zip"),
            subject_last_seen,
            detected_at: 0,
            candidates: candidate_last_seen
                .iter()
                .enumerate()
                .map(|(i, seen)| CandidateRow {
                    id: i as u64 + 1,
                    info: info("/d/c.zip"),
                    state: if seen.is_some() { Presence::Missing } else { Presence::Present },
                    rating_half: 0,
                    tag_count: 0,
                    favorite: false,
                    last_seen: *seen,
                })
                .collect(),
        }
    }

    #[test]
    fn days_left_counts_down_from_retention() {
        let now = 100 * DAY;
        assert_eq!(days_left(None, 60, now), None, "現存ファイルは期限なし");
        assert_eq!(days_left(Some(now), 60, now), Some(60));
        assert_eq!(days_left(Some(now - 19 * DAY), 60, now), Some(41));
        assert_eq!(days_left(Some(now - 60 * DAY), 60, now), Some(0));
        assert_eq!(days_left(Some(now - 70 * DAY), 60, now), Some(-10));
        // 未来の日時（時計ずれ）は経過0日として扱う。
        assert_eq!(days_left(Some(now + 5 * DAY), 60, now), Some(60));
        assert_eq!(days_left(Some(now), 120, now), Some(120));
        assert_eq!(days_left(Some(now), 1, now), Some(1));
    }

    #[test]
    fn item_days_left_uses_the_soonest_missing_candidate_or_own_deadline() {
        let now = 100 * DAY;
        // 引き継ぎ元の選択: 候補は現存のみ＝期限なし。
        let copy = item(PendingKind::CopySource, None, &[None, None]);
        assert_eq!(item_days_left(&copy, 60, now), None);
        // 移動元の選択: 消えている候補のうち、最も早く期限が来るもの（最終確認が58日前）。
        let moved = item(
            PendingKind::MoveTarget,
            None,
            &[Some(now - 19 * DAY), Some(now - 50 * DAY), Some(now - 58 * DAY)],
        );
        assert_eq!(item_days_left(&moved, 60, now), Some(2));
        // お気に入りの引き継ぎ: 対象（実体の無いお気に入り）自身の期限。候補（現存）は関係しない。
        let fav = item(PendingKind::FavoriteHandover, Some(now - 70 * DAY), &[None]);
        assert_eq!(item_days_left(&fav, 60, now), Some(-10));
    }

    #[test]
    fn formatters_are_stable() {
        assert_eq!(format_rating(0), "-");
        assert_eq!(format_rating(8), "★4");
        assert_eq!(format_rating(7), "★3.5");
        assert_eq!(format_size(12_900_000), "12.9MB");
        assert_eq!(format_size(150_400_000), "150.4MB");
        assert_eq!(format_size(2_500_000_000), "2.5GB");
        assert_eq!(format_size(4_200), "4KB");
        assert_eq!(short_path("/home/user/書籍/整理/b.zip"), "b.zip  (…/整理)");
        assert_eq!(file_name("/a/b/c.zip"), "c.zip");
    }

    #[test]
    fn every_resolve_error_maps_to_a_known_message() {
        let t = i18n::t();
        for e in [
            ResolveError::NotFound,
            ResolveError::NotACandidate,
            ResolveError::SubjectGone,
            ResolveError::SourceGone,
            ResolveError::SubjectHasData,
            ResolveError::NoFavorite,
            ResolveError::Db,
        ] {
            let code = error_code(&e);
            assert!(!t.idres_error(code).is_empty(), "{code}");
        }
        // 未知のコードは「保存に失敗」にまとまる（db も同じ）。
        assert_eq!(t.idres_error("db"), t.idres_error("unknown"));
        assert_ne!(t.idres_error("not_found"), t.idres_error("db"));
    }

    #[test]
    fn every_kind_explains_cause_and_outcome_and_names_the_target_in_every_language() {
        for lang in [i18n::Lang::Japanese, i18n::Lang::English, i18n::Lang::Chinese] {
            for kind in PendingKind::ALL {
                assert!(lang.idres_situation(kind, "T.zip", 2).contains("T.zip"), "{lang:?} {kind:?}");
                assert!(lang.idres_situation(kind, "T.zip", 2).contains('2'), "{lang:?} {kind:?}");
                assert!(lang.idres_on_confirm(kind, "T.zip").contains("T.zip"), "{lang:?} {kind:?}");
                assert!(lang.idres_on_skip(kind, "T.zip").contains("T.zip"), "{lang:?} {kind:?}");
                assert!(!lang.idres_reason(kind).is_empty() && !lang.idres_ask(kind).is_empty());
                assert!(!lang.idres_kind(kind).is_empty() && !lang.idres_candidates_heading(kind).is_empty());
            }
            assert!(lang.idres_button(3).contains('3'));
            assert!(lang.idres_shown(1, 2).contains('1') && lang.idres_shown(1, 2).contains('2'));
            assert!(lang.idres_until_days(5).contains('5'));
        }
        // 候補の見出しは、日本語では「管理データ内」の話だと分かる。
        for kind in PendingKind::ALL {
            assert!(i18n::Lang::Japanese.idres_candidates_heading(kind).contains("管理データ内"));
        }
    }
}
