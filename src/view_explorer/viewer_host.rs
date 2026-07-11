use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::cache::{LoadResult, EntryThumbRequest, EntryThumbResult};
use crate::gui_config::ThumbbarPos;
use crate::gui_config::WindowSlot;
use crate::controller::{self, ViewerNav};
use crate::i18n;
use crate::view_reader::{PageMode, ViewerState};
use super::*;
use super::workers::dispatch_thumb_request;

impl NekoviewApp {
    /// ビューアー窓が開いているか（winit_app が窓の生成/破棄判定に使う）。
    pub fn viewer_is_open(&self) -> bool {
        self.viewer.lock().unwrap().is_some()
    }

    /// ビューアー窓を生成する winit 側が、初回フラッシュを避けるために参照する
    /// 解決済み既定スロット（conf default_slot × 現在の viewer_slots）。
    /// None のとき winit は従来の OS既定位置・800x600 で生成する。
    pub fn resolved_default_viewer_slot(&self) -> Option<WindowSlot> {
        crate::controller::resolve_default_slot(self.config.default_slot, &self.viewer_slots)
    }

    /// ビューアー窓のフォーカス前面化要求を取り出す（取り出したら false に戻す）。
    pub fn take_viewer_focus_request(&mut self) -> bool {
        let f = self.viewer_focus_requested;
        self.viewer_focus_requested = false;
        f
    }

    /// ビューアーを閉じる（OS のクローズボタン等から winit_app が呼ぶ）。
    pub fn close_viewer(&mut self) {
        *self.viewer.lock().unwrap() = None;
    }

    /// ビューアー独立窓の 1 フレーム描画。winit_app がビューアー窓の egui パスから呼ぶ。
    /// 旧 `draw_viewer_viewport` の deferred callback 相当（ページ供給 → show → nav/close 処理）。
    pub fn render_viewer(&mut self, ui: &mut egui::Ui) {
        // フェーズ6-E: poll_resize_redecode()はエクスプローラー窓のlogic()からしか
        // 呼ばれていなかったため、ビューアー窓だけを操作している間はエクスプローラー窓が
        // 再描画されずデバウンスが発火しないバグがあった。ビューアー窓自身の毎フレームでも
        // 呼ぶことで、エクスプローラー窓の再描画タイミングに依存しないようにする。
        self.poll_resize_redecode(ui.ctx());

        // 設定ダイアログ表示中は、エクスプローラー窓と合わせてビューアー窓側の操作も
        // ブロックする。独立 OS 窓（別 egui::Context）なので winit_app 側の入力横取りは
        // 使わない。egui::Modal はレイヤー順で「後に出した方が最前面」になるため、通常の
        // viewer.show() より後に出しても既に処理済みのウィジェットの入力は防げない
        // （同一フレーム内で先に走った側が先に入力を消費してしまう）。そのため
        // viewer.show() 自体を呼ばず、Modal だけを描いて操作を完全に止める。
        if self.settings_is_open() {
            egui::Modal::new(egui::Id::new("viewer_settings_blocked")).show(ui.ctx(), |ui| {
                ui.label(i18n::t().settings_viewer_blocked());
            });
            return;
        }

        // エクスプローラー窓が起きていなくてもページ送りが進むよう、
        // ここでワーカー結果回収（res_rx drain）と先読みを回す。
        self.pump_viewer_pages();
        // 窓ごとに独立した egui::Context を持つ構成のため、サムネイルバー用テクスチャは
        // 必ずビューアー窓自身の ctx(=ui.ctx()) で load_texture する。self.egui_ctx は
        // エクスプローラー窓側の Context であり、そちらで作ったテクスチャはビューアー窓の
        // Painter からは見えない（テクスチャがデコードはできても描画されない原因だった）。
        self.pump_thumbbar_entries(ui.ctx());

        let output = {
            let mut viewer_guard = self.viewer.lock().unwrap();
            let page_cache_guard = self.page_cache.lock().unwrap();
            let mut cfg_guard = self.viewer_cfg.lock().unwrap();
            match viewer_guard.as_mut() {
                Some(viewer) => viewer.show(ui, &*page_cache_guard, &mut *cfg_guard),
                None => return,
            }
        };

        self.draw_translate_overlay(ui.ctx());

        if let Some(slots) = output.save_slots {
            self.viewer_slots = slots;
            self.persist_state();
        }

        if let Some(action) = output.spread_save_action {
            self.handle_spread_save_action(action);
        }

        if output.open_favorite_dialog {
            self.open_favorite_detail_dialog();
        }
        // 描画自体はエクスプローラー窓の ui() 側でのみ行う（memory_warning_open 等と同じ
        // 「状態はどちらの窓のアクションからでもセットできるが、モーダル描画は単一窓に一本化する」
        // 既存パターンに合わせる。ビューアー窓側でも呼ぶと、複数選択からの起動時にビューアー窓・
        // エクスプローラー窓の両方でダイアログが二重に描画されてしまう）。

        let had_nav = output.nav != ViewerNav::None;
        if output.close_requested {
            *self.viewer.lock().unwrap() = None;
            controller::request_status_update(&self.status_update_requested);
            self.egui_ctx.request_repaint();
        } else if had_nav {
            self.handle_viewer_nav(output.nav);
            controller::request_status_update(&self.status_update_requested);
            self.egui_ctx.request_repaint();
        }
    }

    /// アーカイブ内サムネイルバー用: ワーカー結果の回収と、未取得エントリの要求送出。
    /// サムネイルバー配置が「表示なし」、またはアーカイブが1件以下の場合は要求を出さない
    /// （設定タブの表示条件と揃える）。
    fn pump_thumbbar_entries(&mut self, viewer_ctx: &egui::Context) {
        let results: Vec<EntryThumbResult> =
            std::iter::from_fn(|| self.entry_thumb_res_rx.try_recv().ok()).collect();

        let mut viewer_guard = self.viewer.lock().unwrap();
        let Some(viewer) = viewer_guard.as_mut() else { return; };

        for result in results {
            if result.archive_path == *viewer.archive_path() {
                viewer.set_thumb_result(viewer_ctx, result.original_index, result.rgba);
            }
        }

        let (thumbbar_pos, edge) = {
            let cfg = self.viewer_cfg.lock().unwrap();
            (cfg.thumbbar_pos, cfg.thumbbar_thumb_size)
        };
        if thumbbar_pos == ThumbbarPos::None || viewer.entries().len() <= 1 {
            return;
        }

        let archive_path = viewer.archive_path().clone();
        let is_raw_file = viewer.is_raw_file();
        for original_index in viewer.thumbbar_missing_indices() {
            let Some(entry_name) = viewer.entry_name_for(original_index) else { continue; };
            let req = EntryThumbRequest {
                archive_path: archive_path.clone(),
                entry_name: entry_name.to_string(),
                original_index,
                is_raw_file,
                edge,
                file_cache_entry: None,
            };
            dispatch_thumb_request(
                &self.file_cache,
                &self.file_cache_pending,
                &mut self.deferred_archive_requests,
                &self.entry_thumb_req_tx,
                req,
                viewer,
            );
        }
    }

    /// ビューアー独立窓パスからのページワーカー結果回収（res_rx drain）と先読み。
    /// エクスプローラー窓の poll_workers / prefetch_pages が起きていない間でもページを進める。
    fn pump_viewer_pages(&mut self) {
        let results: Vec<LoadResult> =
            std::iter::from_fn(|| self.res_rx.lock().unwrap().try_recv().ok()).collect();
        if !results.is_empty() {
            let (cur_path, cur_idx) = self.viewer.lock().unwrap().as_ref()
                .map(|v| {
                    let lo = v.spread_lo().max(0) as usize;
                    let orig = v.entries().get(lo).map(|e| e.original_index).unwrap_or(0);
                    (v.archive_path().clone(), orig)
                })
                .unwrap_or_default();
            for result in results {
                self.pending_loads.lock().unwrap()
                    .remove(&(result.archive_path.clone(), result.index));
                self.page_cache.lock().unwrap().insert(
                    result.archive_path,
                    result.index,
                    result.content,
                    &cur_path,
                    cur_idx,
                );
            }
        }
        self.prefetch_pages();
    }

    /// direction(+1/-1) 方向に from_idx から次の有効ファイルを探す。
    /// キャッシュ済み無効ZIPはスキップ、未判定は ViewerState::new() で確認して無効なら登録しスキップ。
    fn find_next_valid(&mut self, from_idx: usize, direction: i32) -> Option<(usize, ViewerState)> {
        let mut skip_idx = from_idx;
        loop {
            match controller::find_next_file(
                &self.archives,
                &self.raw_image_files,
                &self.invalid_archives,
                skip_idx,
                direction,
            ) {
                None => return None,
                Some((idx, path, true)) => {
                    return Some((idx, ViewerState::new_raw(path, self.viewer_slots, self.config.default_slot)));
                }
                Some((idx, path, false)) => {
                    if !self.check_memory_budget(&path) {
                        // ダイアログ表示フラグは check_memory_budget 内で立つ。
                        // invalid_archives には永続マークせず、現在のファイルに留まる。
                        return None;
                    }
                    match ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot) {
                        Some(state) => return Some((idx, state)),
                        None => {
                            self.mark_archive_invalid(&path);
                            skip_idx = idx;
                        }
                    }
                }
            }
        }
    }

    fn handle_viewer_nav(&mut self, nav: ViewerNav) {
        match nav {
            ViewerNav::None => {}
            ViewerNav::PrevFile => {
                if let Some(from) = self.selected_archive_index {
                    if let Some((idx, state)) = self.find_next_valid(from, -1) {
                        self.selected_archive_index = Some(idx);
                        self.open_viewer(state);
                    } else if !self.memory_warning_open {
                        // メモリ見積もり超過が原因の場合は memory_warning ダイアログの方を
                        // 表示するため、「これ以上開けない」トーストは出さない。
                        if let Some(v) = self.viewer.lock().unwrap().as_mut() {
                            v.set_toast(i18n::t().toast_no_prev().to_string());
                        }
                    }
                }
            }
            ViewerNav::NextFile => {
                if let Some(from) = self.selected_archive_index {
                    if let Some((idx, state)) = self.find_next_valid(from, 1) {
                        self.selected_archive_index = Some(idx);
                        self.open_viewer(state);
                    } else if !self.memory_warning_open {
                        if let Some(v) = self.viewer.lock().unwrap().as_mut() {
                            v.set_toast(i18n::t().toast_no_next().to_string());
                        }
                    }
                }
            }
        }
    }

    /// 右クリックメニューでの見開き設定保存操作を反映する（DB書き込み/削除＋メモリ上のキャッシュ更新）。
    fn handle_spread_save_action(&mut self, action: crate::controller::SpreadSaveAction) {
        let Some(db) = self.spread_db.clone() else { return };
        let mut viewer_guard = self.viewer.lock().unwrap();
        let Some(viewer) = viewer_guard.as_mut() else { return };
        let filename = match viewer.archive_path().file_name().and_then(|n| n.to_str()) {
            Some(f) => f.to_string(),
            None => return,
        };

        let disable = |viewer: &mut ViewerState, spread_states: &mut HashMap<String, (PageMode, i32)>, db: &Arc<Mutex<redb::Database>>, dir: &std::path::Path, filename: &str| {
            crate::spread_state::remove_spread(db, dir, filename);
            viewer.set_saved_spread(None);
            spread_states.remove(filename);
        };

        use crate::controller::SpreadSaveAction;
        match action {
            SpreadSaveAction::Enable => {
                let (mode, offset) = viewer.current_spread_snapshot();
                crate::spread_state::write_spread(&db, &self.current_dir, &filename, mode, offset);
                viewer.set_saved_spread(Some((mode, offset)));
                self.spread_states.insert(filename, (mode, offset));
            }
            SpreadSaveAction::Disable => {
                disable(viewer, &mut self.spread_states, &db, &self.current_dir, &filename);
            }
            SpreadSaveAction::Overwrite => {
                let (mode, offset) = viewer.current_spread_snapshot();
                if mode == PageMode::Single {
                    disable(viewer, &mut self.spread_states, &db, &self.current_dir, &filename);
                } else {
                    crate::spread_state::write_spread(&db, &self.current_dir, &filename, mode, offset);
                    viewer.set_saved_spread(Some((mode, offset)));
                    self.spread_states.insert(filename, (mode, offset));
                }
            }
        }
    }

    /// ビューアを開く（ページキャッシュクリア・ファイルキャッシュ投入・フォーカス要求を一括処理）
    pub(super) fn open_viewer(&mut self, mut state: ViewerState) {
        let path = state.archive_path().clone();
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(&(mode, offset)) = self.spread_states.get(filename) {
            let mut cfg = self.viewer_cfg.lock().unwrap();
            state.restore_saved_spread(mode, offset, &mut cfg);
            state.set_saved_spread(Some((mode, offset)));
        }
        self.pending_loads.lock().unwrap().clear();
        *self.viewer.lock().unwrap() = Some(state);
        self.ensure_file_cached(path);
        self.viewer_focus_requested = true;
    }

    /// 翻訳機能(実験的)の半透明オーバーレイ。URL未設定なら何も描画しない
    /// （既定では非表示のまま、使わないユーザーの画面に余計なUIを出さない）。
    /// 位置(四隅)・横幅は設定タブ側の値のみで決まり、ビューアー上での
    /// ドラッグ・リサイズは行わない（`movable(false)`）。
    fn draw_translate_overlay(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.translate_ocr_rx {
            if let Ok(msg) = rx.try_recv() {
                match msg {
                    crate::translate::OcrMsg::Result(page) => {
                        self.translate_ocr_lines = page.lines.clone();
                        self.translate_ocr_status = if page.raw_fallback {
                            Some(i18n::t().translate_overlay_fallback_notice().to_string())
                        } else {
                            None
                        };
                        // 要求時点のページへ保存する（待機中にページ送りされても取り違えないよう
                        // trigger_translate_ocr側で退避しておいたキーを使う）。
                        if let Some((archive_path, orig)) = self.translate_ocr_inflight_key.take() {
                            self.save_ocr_text_for(&archive_path, orig, &page.lines);
                        }
                    }
                    crate::translate::OcrMsg::Failed(e) => {
                        self.translate_ocr_status = Some(format!("{}: {e}", i18n::t().translate_overlay_failed_prefix()));
                        self.translate_ocr_inflight_key = None;
                    }
                }
                self.translate_ocr_rx = None;
            }
        }

        // ページが切り替わったら、そのページ用の保存済みtxtを読み直す（無ければ空表示に戻す）。
        // OCR実行中(rxが生きている間)は取りこぼし防止のため切り替えを保留する。
        if self.translate_ocr_rx.is_none() {
            let current_key = self.current_page_key();
            if current_key != self.translate_ocr_loaded_key {
                self.translate_ocr_status = None;
                self.translate_ocr_lines = current_key
                    .as_ref()
                    .and_then(|(path, orig)| self.load_ocr_text_for(path, *orig))
                    .unwrap_or_default();
                self.translate_ocr_loaded_key = current_key;
            }
        }

        if self.translate_cfg.base_url.trim().is_empty() {
            return;
        }

        use crate::translate::OverlayCorner;
        let corner = self.translate_cfg.overlay_corner;
        let width = self.translate_cfg.overlay_width as f32;
        let (anchor, offset) = match corner {
            OverlayCorner::TopLeft => (egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0)),
            OverlayCorner::TopRight => (egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0)),
            OverlayCorner::BottomLeft => (egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0)),
            OverlayCorner::BottomRight => (egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0)),
        };

        egui::Area::new(egui::Id::new("translate_overlay"))
            .anchor(anchor, offset)
            .movable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_black_alpha(190))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(i18n::t().translate_overlay_title()).strong());
                            if ui.small_button(i18n::t().translate_overlay_run_button()).clicked() {
                                self.trigger_translate_ocr(ctx);
                            }
                            // オーバーレイ内は自前キーボードナビゲーションと競合し、矢印キーでの
                            // テキスト範囲選択ができないため、範囲選択の代わりに全文コピーボタンを置く。
                            ui.add_enabled_ui(!self.translate_ocr_lines.is_empty(), |ui| {
                                if ui.small_button(i18n::t().translate_overlay_copy_button()).clicked() {
                                    ctx.copy_text(self.translate_ocr_lines.join("\n"));
                                }
                            });
                            // txtは手動でノイズ取り(誤読訂正・整形)した内容を「翻訳の原本」として
                            // 扱いたいという要望から、OSのファイラーで直接開けるようにしている。
                            if ui.small_button(i18n::t().translate_overlay_open_folder_button()).clicked() {
                                self.open_translate_text_folder();
                            }
                        });
                        if let Some(status) = &self.translate_ocr_status {
                            ui.colored_label(egui::Color32::from_rgb(230, 140, 140), status.as_str());
                        }
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .id_salt("translate_overlay_scroll")
                            .show(ui, |ui| {
                                // フレーム内側の余白ぶんを差し引き、折返しをオーバーレイ幅に追従させる。
                                ui.set_width((width - 16.0).max(40.0));
                                if self.translate_ocr_lines.is_empty() {
                                    ui.weak(i18n::t().translate_overlay_empty());
                                } else {
                                    for line in &self.translate_ocr_lines {
                                        ui.label(egui::RichText::new(line.as_str()).size(14.0));
                                        ui.add_space(6.0);
                                    }
                                }
                            });
                    });
            });
    }

    /// 現在ページに対してOCRリクエストを発火する（1ページ単位。アーカイブ一括OCRは
    /// 規模が大きいため今回は見送り、まずページ単位の永続化を優先した）。
    fn trigger_translate_ocr(&mut self, ctx: &egui::Context) {
        if self.translate_cfg.model.trim().is_empty() {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_model_missing().to_string());
            return;
        }
        let Some(key) = self.current_page_key() else {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_no_page().to_string());
            return;
        };
        let Some(rgba) = self.current_page_rgba_for_ocr() else {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_no_page().to_string());
            return;
        };
        let page_mode = self.viewer.lock().unwrap().as_ref()
            .map(|v| v.current_spread_snapshot().0)
            .unwrap_or(crate::types::PageMode::Single);
        let image = image::DynamicImage::ImageRgba8(rgba);
        self.translate_ocr_status = Some(i18n::t().translate_overlay_running().to_string());
        self.translate_ocr_lines.clear();
        self.translate_ocr_inflight_key = Some(key);
        self.translate_ocr_rx = Some(crate::translate::spawn_ocr_request(
            ctx.clone(),
            self.translate_cfg.base_url.clone(),
            self.translate_cfg.model.clone(),
            image,
            page_mode,
        ));
    }

    /// 現在表示中ページ(見開き時は先頭側)を一意に表す(アーカイブパス, original_index)。
    fn current_page_key(&self) -> Option<(std::path::PathBuf, usize)> {
        let viewer_guard = self.viewer.lock().unwrap();
        let viewer = viewer_guard.as_ref()?;
        let orig = *viewer.visible_original_indices().first()?;
        Some((viewer.archive_path().clone(), orig))
    }

    /// OCR送信用の画像を取得する。見開き表示中は「画面に見えている通り」の1枚に
    /// 左右のページを結合する（indices配列はentries順=[lo, lo+1]で画面左右とは
    /// 限らないため、綴じ方向(page_mode)で画面左右へ並べ替えてから結合する）。
    /// 単独ページ判定はentries数(1枚のみ返る)で行う。アニメーションページは非対応。
    fn current_page_rgba_for_ocr(&self) -> Option<image::RgbaImage> {
        let viewer_guard = self.viewer.lock().unwrap();
        let viewer = viewer_guard.as_ref()?;
        let page_mode = viewer.current_spread_snapshot().0;
        let indices = viewer.visible_original_indices();
        let path = viewer.archive_path().clone();
        drop(viewer_guard);

        let cache = self.page_cache.lock().unwrap();
        let get_rgba = |orig: usize| -> Option<image::RgbaImage> {
            match cache.get(&path, orig)? {
                crate::cache::PageContent::Static(rgba) => Some(rgba.clone()),
                crate::cache::PageContent::Animated(_) => None,
            }
        };

        match indices.as_slice() {
            [single] => get_rgba(*single),
            [lo, hi, ..] => {
                let (screen_left, screen_right) = match page_mode {
                    crate::types::PageMode::SpreadLeft => (*lo, *hi),
                    // SpreadRight(一般的な右綴じ): entries順で先=lo が画面右、後=hiが画面左
                    // （render_spreadの呼び出し ` render_spread(tex_hi, tex_lo)` に合わせる）。
                    crate::types::PageMode::SpreadRight => (*hi, *lo),
                    crate::types::PageMode::Single => (*lo, *lo),
                };
                let left = get_rgba(screen_left)?;
                let right = get_rgba(screen_right)?;
                Some(compose_side_by_side(&left, &right))
            }
            [] => None,
        }
    }

    /// アーカイブパスからOCR txt保存先のキャッシュディレクトリを解決する。
    /// キャッシュ無効設定(cache_root未設定)の環境ではNone（永続化自体をスキップ）。
    fn translate_neko_dir_for(&self, archive_path: &std::path::Path) -> Option<std::path::PathBuf> {
        let dir = archive_path.parent()?;
        crate::neko_dir::neko_dir_for(dir, &self.config)
    }

    fn save_ocr_text_for(&self, archive_path: &std::path::Path, original_index: usize, lines: &[String]) {
        let Some(neko_dir) = self.translate_neko_dir_for(archive_path) else { return };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return };
        let _ = crate::translate::save_ocr_text(&neko_dir, filename, original_index, lines);
    }

    fn load_ocr_text_for(&self, archive_path: &std::path::Path, original_index: usize) -> Option<Vec<String>> {
        let neko_dir = self.translate_neko_dir_for(archive_path)?;
        let filename = archive_path.file_name().and_then(|n| n.to_str())?;
        crate::translate::load_ocr_text(&neko_dir, filename, original_index)
    }

    /// 現在のアーカイブに対応するOCR txtフォルダをOSのファイラーで開く
    /// （手動でのノイズ取り・整形をそのまま「翻訳の原本」として使ってもらうための導線）。
    fn open_translate_text_folder(&self) {
        let Some((archive_path, _)) = self.current_page_key() else { return };
        let Some(neko_dir) = self.translate_neko_dir_for(&archive_path) else { return };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return };
        crate::translate::open_in_file_manager(&crate::translate::ocr_text_dir(&neko_dir, filename));
    }
}

/// 見開きOCR用: 画面表示順どおりに左右のページ画像を1枚へ横結合する。
/// 高さが異なる場合は白背景の上に上詰めで配置する（下に余白ができるだけで、
/// テキスト認識への影響はほぼ無い想定）。
fn compose_side_by_side(left: &image::RgbaImage, right: &image::RgbaImage) -> image::RgbaImage {
    let width = left.width() + right.width();
    let height = left.height().max(right.height());
    let mut canvas = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut canvas, left, 0, 0);
    image::imageops::overlay(&mut canvas, right, left.width() as i64, 0);
    canvas
}
