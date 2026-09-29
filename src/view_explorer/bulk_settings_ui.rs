use std::path::PathBuf;

use crate::i18n;
use crate::types::{PageMode, ReaderSortKey};
use super::*;

/// エクスプローラー部の右クリックから開く「ソート条件」「しおり保存」「見開き設定」
/// 一括変更ダイアログ群。反映時、対象ファイル1件ずつ`spread_state`の対応するAPIを
/// 呼び出し、成否を[[BulkSettingResult]]に集約してトースト表示する。
/// ソート条件/しおり保存/見開き設定の一括反映を対象ファイル1件ずつ実行した結果。
/// 処理順（呼び出し側がループした順）で格納し、トースト生成時に正常/異常でまとめる。
pub(super) struct BulkSettingResult {
    pub target: PathBuf,
    pub ok: bool,
}

/// 一括設定変更の実行結果から、正常件数1行＋異常ファイル名を続けたトースト文字列を組み立てる。
/// 異常行は最大10行、それを超えた分は11行目に「他○○件で異常終了」として集約する。
/// 正常0件（全滅）の場合は正常行を省略し、異常行のみを表示する。
pub(super) fn build_bulk_setting_toast(results: &[BulkSettingResult]) -> String {
    const MAX_FAILURE_LINES: usize = 10;
    let success_count = results.iter().filter(|r| r.ok).count();
    let failures: Vec<&BulkSettingResult> = results.iter().filter(|r| !r.ok).collect();

    let mut lines: Vec<String> = Vec::new();
    if success_count > 0 {
        lines.push(i18n::t().bulk_setting_success_toast(success_count));
    }
    for r in failures.iter().take(MAX_FAILURE_LINES) {
        lines.push(i18n::t().bulk_setting_failure_toast(&super::panels::truncate_filename(&r.target)));
    }
    if failures.len() > MAX_FAILURE_LINES {
        lines.push(i18n::t().bulk_setting_failure_overflow_toast(failures.len() - MAX_FAILURE_LINES));
    }
    lines.join("\n")
}

impl NekoviewApp {
    /// 右クリック対象からソート条件/しおり保存/見開き設定の対象外
    /// （ディレクトリ・単品生画像ファイル・無効アーカイブ）を除外する。
    /// ディレクトリは現状このメニュー自体がアーカイブグリッドのセルにしか
    /// 出ないため理論上混入しないが、要求仕様に明記されているため防御的に含める。
    pub(super) fn filter_bulk_setting_targets(&self, targets: Vec<PathBuf>) -> Vec<PathBuf> {
        targets
            .into_iter()
            .filter(|p| !p.is_dir())
            .filter(|p| !self.raw_image_files.contains(p))
            .filter(|p| !self.invalid_archives.contains(p))
            .collect()
    }

    /// 反映後、対象ファイルのサムネイル上マーカー（L/R/S/T/B）表示キャッシュを
    /// 実DB値へ同期して再描画を要求する。`viewer_host.rs`の
    /// `refresh_saved_archive_settings`（単一ファイル用）のバルク版。
    fn sync_saved_archive_settings(&mut self, paths: &[PathBuf]) {
        let Some(db) = self.spread_db.clone() else { return };
        let refreshed = crate::spread_state::saved_settings_for_paths(&db, paths);
        for path in paths {
            match refreshed.get(path) {
                Some(settings) => { self.saved_archive_settings.insert(path.clone(), *settings); }
                None => { self.saved_archive_settings.remove(path); }
            }
        }
        self.egui_ctx.request_repaint();
    }

    fn dialog_target_label(targets: &[PathBuf]) -> String {
        targets
            .first()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }

    // ── ソート条件 ──────────────────────────────────────────────────────────

    pub(super) fn open_sort_condition_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.sort_condition_dialog = Some(SortConditionDialogState {
            targets,
            sort_key: ReaderSortKey::Name,
            ascending: true,
        });
    }

    pub(super) fn draw_sort_condition_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.sort_condition_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().sort_condition_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().sort_condition_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Name, i18n::t().sort_name().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Natural, i18n::t().sort_natural().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Date, i18n::t().sort_date().trim_matches(['[', ']']));
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dialog.ascending, true, i18n::t().sort_asc().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.ascending, false, i18n::t().sort_desc().trim_matches(['[', ']']));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel {
            self.sort_condition_dialog = None;
        } else if apply {
            self.commit_sort_condition_dialog();
        }
    }

    fn commit_sort_condition_dialog(&mut self) {
        let Some(dialog) = self.sort_condition_dialog.take() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        let mut results = Vec::with_capacity(dialog.targets.len());
        for path in &dialog.targets {
            let dir = path.parent().map(|p| p.to_path_buf());
            let filename = path.file_name().and_then(|n| n.to_str()).map(str::to_string);
            let ok = match (dir, filename) {
                (Some(dir), Some(filename)) => {
                    let ok = crate::spread_state::write_archive_sort(&db, &dir, &filename, dialog.sort_key, dialog.ascending);
                    if ok && dir == self.current_dir {
                        self.archive_sort_states.insert(filename, (dialog.sort_key, dialog.ascending));
                    }
                    ok
                }
                _ => false,
            };
            results.push(BulkSettingResult { target: path.clone(), ok });
        }
        self.sync_saved_archive_settings(&dialog.targets);
        self.app_toast = Some((build_bulk_setting_toast(&results), std::time::Instant::now()));
    }

    // ── しおり保存 ──────────────────────────────────────────────────────────

    pub(super) fn open_bookmark_setting_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.bookmark_setting_dialog = Some(BookmarkSettingDialogState {
            targets,
            enabled: false,
        });
    }

    pub(super) fn draw_bookmark_setting_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.bookmark_setting_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().bookmark_setting_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().bookmark_setting_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.checkbox(&mut dialog.enabled, i18n::t().bookmark_save_toggle_label());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel {
            self.bookmark_setting_dialog = None;
        } else if apply {
            self.commit_bookmark_setting_dialog();
        }
    }

    fn commit_bookmark_setting_dialog(&mut self) {
        let Some(dialog) = self.bookmark_setting_dialog.take() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        let mut results = Vec::with_capacity(dialog.targets.len());
        for path in &dialog.targets {
            let dir = path.parent();
            let filename = path.file_name().and_then(|n| n.to_str());
            let ok = match (dir, filename) {
                (Some(dir), Some(filename)) => crate::spread_state::write_bookmark_enabled(&db, dir, filename, dialog.enabled),
                _ => false,
            };
            results.push(BulkSettingResult { target: path.clone(), ok });
        }
        self.sync_saved_archive_settings(&dialog.targets);
        self.app_toast = Some((build_bulk_setting_toast(&results), std::time::Instant::now()));
    }

    // ── 見開き設定 ──────────────────────────────────────────────────────────

    pub(super) fn open_spread_setting_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.spread_setting_dialog = Some(SpreadSettingDialogState {
            targets,
            page_mode: PageMode::Single,
            offset: 0,
        });
    }

    pub(super) fn draw_spread_setting_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.spread_setting_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().spread_setting_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().spread_setting_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dialog.page_mode, PageMode::Single, i18n::t().spread_mode_single_label());
                    ui.radio_value(&mut dialog.page_mode, PageMode::SpreadRight, i18n::t().spread_mode_right_label());
                    ui.radio_value(&mut dialog.page_mode, PageMode::SpreadLeft, i18n::t().spread_mode_left_label());
                });
                ui.add_space(8.0);
                ui.add_enabled_ui(dialog.page_mode != PageMode::Single, |ui| {
                    // 保存値は0（先頭仮想なし）/-1（先頭仮想あり）の2値のみ。
                    // 旧データの+1は読込側でnormalize_saved_spread_offsetにより-1と同一視されるため、
                    // 新規保存では-1に統一する（+1は書かない）。
                    ui.radio_value(&mut dialog.offset, 0, i18n::t().spread_offset_no_virtual_label());
                    ui.radio_value(&mut dialog.offset, -1, i18n::t().spread_offset_virtual_first_label());
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel {
            self.spread_setting_dialog = None;
        } else if apply {
            self.commit_spread_setting_dialog();
        }
    }

    fn commit_spread_setting_dialog(&mut self) {
        let Some(dialog) = self.spread_setting_dialog.take() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        // 単ページ選択時はオフセットスライダーが非活性のため、保存値も強制的に0にする
        // （見開きを解いた後もダイアログ内部状態にオフセットの値が残っている場合があるため）。
        let offset = if dialog.page_mode == PageMode::Single { 0 } else { dialog.offset };
        let mut results = Vec::with_capacity(dialog.targets.len());
        for path in &dialog.targets {
            let dir = path.parent().map(|p| p.to_path_buf());
            let filename = path.file_name().and_then(|n| n.to_str()).map(str::to_string);
            let ok = match (dir, filename) {
                (Some(dir), Some(filename)) => {
                    let ok = crate::spread_state::write_spread(&db, &dir, &filename, dialog.page_mode, offset);
                    if ok && dir == self.current_dir {
                        self.spread_states.insert(filename, (dialog.page_mode, offset));
                    }
                    ok
                }
                _ => false,
            };
            results.push(BulkSettingResult { target: path.clone(), ok });
        }
        self.sync_saved_archive_settings(&dialog.targets);
        self.app_toast = Some((build_bulk_setting_toast(&results), std::time::Instant::now()));
    }

    // ── スコアの設定 ──────────────────────────────────────────────────────────

    pub(super) fn open_rating_setting_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        // 単品選択時は現在のスコアを復元し、ラジオの初期チェック＆「変更前のスコア」欄の両方に使う
        // （レコード不在＝一度も評価していないファイルも「未評価」= 0 として扱う）。
        // 複数選択時はスコアの復元ができないため常にNone（どのラジオも未選択）で開く。
        let original_rating = if targets.len() == 1 {
            let half = self.spread_db.as_ref().and_then(|db| {
                let dir = targets[0].parent()?;
                let name = targets[0].file_name()?.to_str()?;
                crate::spread_state::read_archive_rating(db, dir, name)
            }).map(|r| r.rating_half).unwrap_or(0);
            Some(half)
        } else {
            None
        };
        self.rating_setting_dialog = Some(RatingSettingDialogState {
            targets,
            rating_half: original_rating,
            original_rating,
        });
    }

    pub(super) fn draw_rating_setting_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.rating_setting_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        // ファイル名が長くても横に伸ばさず、固定幅の中で折り返して縦に伸ばす
        // （ラジオボタン2行のレイアウトがファイル名の長さに引きずられて崩れるのを防ぐ）。
        egui::Window::new(i18n::t().rating_setting_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .max_width(300.0)
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().rating_setting_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.add(egui::Label::new(label).wrap());
                let before_value = if is_bulk {
                    i18n::t().rating_setting_before_multi().to_string()
                } else {
                    i18n::t().rating_radio_label(dialog.original_rating.unwrap_or(0))
                };
                ui.add(egui::Label::new(format!("{}{}", i18n::t().rating_setting_before_label(), before_value)).wrap());
                ui.add_space(8.0);
                // 11項目（★0.5〜★5.0+未評価）を4+4+3の3行に分ける。
                // 2行(5+6)だと「★2.5 ★3 ★3.5 ★4 ★4.5 ★5」の6項目がmax_width(300)を
                // 超えてはみ出し、ウィンドウが横に広がってしまう（ui.horizontalは折り返さない）。
                // 「未評価」は文字幅が★x.xと異なり行の並びを崩すため、先頭ではなく最後尾（3行目末尾）に置く。
                const ROWS: [&[u8]; 3] = [&[1, 2, 3, 4], &[5, 6, 7, 8], &[9, 10, 0]];
                for row in ROWS {
                    ui.horizontal(|ui| {
                        for &half in row {
                            let text = i18n::t().rating_radio_label(half);
                            if ui.radio(dialog.rating_half == Some(half), text).clicked() {
                                dialog.rating_half = Some(half);
                            }
                        }
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel {
            self.rating_setting_dialog = None;
        } else if apply {
            self.commit_rating_setting_dialog();
        }
    }

    /// OK押下時の反映。ラジオが未選択（`rating_half == None`）のままなら、複数選択時の
    /// 操作ミス・未記入ガードとして何も書き込まずダイアログを閉じるだけにする。
    fn commit_rating_setting_dialog(&mut self) {
        let Some(dialog) = self.rating_setting_dialog.take() else { return };
        let Some(rating_half) = dialog.rating_half else { return };
        let Some(db) = self.spread_db.clone() else { return };
        let mut results = Vec::with_capacity(dialog.targets.len());
        for path in &dialog.targets {
            let dir = path.parent();
            let filename = path.file_name().and_then(|n| n.to_str());
            let ok = match (dir, filename) {
                (Some(dir), Some(filename)) => crate::spread_state::write_archive_rating(&db, dir, filename, rating_half),
                _ => false,
            };
            results.push(BulkSettingResult { target: path.clone(), ok });
        }
        for path in &dialog.targets {
            self.refresh_rating_cache(path);
        }
        self.app_toast = Some((build_bulk_setting_toast(&results), std::time::Instant::now()));
    }
}

#[cfg(test)]
mod bulk_setting_toast_tests {
    use super::*;

    fn result(name: &str, ok: bool) -> BulkSettingResult {
        BulkSettingResult { target: PathBuf::from(name), ok }
    }

    #[test]
    fn empty_results_yield_empty_toast() {
        assert_eq!(build_bulk_setting_toast(&[]), "");
    }

    #[test]
    fn all_success_has_only_success_line() {
        let results = vec![result("a.zip", true), result("b.zip", true)];
        let toast = build_bulk_setting_toast(&results);
        assert_eq!(toast.lines().count(), 1);
        assert!(toast.contains('2'));
    }

    #[test]
    fn all_failure_omits_success_line() {
        let results = vec![result("a.zip", false), result("b.zip", false)];
        let toast = build_bulk_setting_toast(&results);
        assert_eq!(toast.lines().count(), 2);
        assert!(toast.lines().all(|l| l.contains("a.zip") || l.contains("b.zip")));
    }

    #[test]
    fn failure_overflow_collapses_into_11th_line() {
        // 成功2件 + 失敗12件 → 正常1行 + 失敗10行 + 集約1行(2件) = 12行
        let mut results: Vec<BulkSettingResult> = (0..2).map(|i| result(&format!("ok{i}.zip"), true)).collect();
        results.extend((0..12).map(|i| result(&format!("ng{i}.zip"), false)));
        let toast = build_bulk_setting_toast(&results);
        let lines: Vec<&str> = toast.lines().collect();
        assert_eq!(lines.len(), 12);
        assert!(lines[0].contains('2'));
        assert!(lines.last().unwrap().contains('2'));
    }
}
