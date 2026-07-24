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

    /// OCR/翻訳子ウィンドウが開いているか（winit_app が窓の生成/破棄判定に使う）。
    pub fn translate_window_is_open(&self) -> bool {
        self.translate_window_open
    }

    /// OCR/翻訳子ウィンドウを閉じる（OS のクローズボタンから winit_app が呼ぶ）。
    pub fn close_translate_window(&mut self) {
        self.translate_window_open = false;
    }

    /// OCR/翻訳子ウィンドウの最前面固定トグルが有効か（winit_app が WindowLevel 反映に使う）。
    pub fn translate_window_always_on_top(&self) -> bool {
        self.translate_window_always_on_top
    }

    /// アーカイブ内に1P分でもOCR txtが残っているか。
    fn archive_has_any_ocr_text(&self, archive_path: &std::path::Path) -> bool {
        let Some(neko_dir) = self.translate_neko_dir_for(archive_path) else { return false };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return false };
        crate::translate::has_any_ocr_text(&neko_dir, filename)
    }

    /// アーカイブ切替時に1度だけ、既存OCR txtの有無を見て子ウィンドウの自動オープンを判定する。
    fn maybe_autoopen_translate_window(&mut self) {
        let Some((archive_path, _)) = self.current_page_keys().into_iter().next() else { return };
        if self.translate_window_autocheck_done_for.as_ref() == Some(&archive_path) {
            return;
        }
        self.translate_window_autocheck_done_for = Some(archive_path.clone());
        if !self.translate_window_open && self.archive_has_any_ocr_text(&archive_path) {
            self.translate_window_open = true;
        }
    }

    /// OCR/翻訳子ウィンドウ用: 見開き内の2ページを子カーソルだけで行き来し、
    /// 両方見終わった時にだけ親を実際に1見開き分進める（親のオフセットには一切触れない）。
    /// シングルページモードは可視集合が常に1件なので、毎回親を直接1P送りする＝完全同期。
    fn step_child_page(&mut self, forward: bool) {
        let keys = self.current_page_keys();
        if keys.is_empty() { return; }
        let cur_idx = self.translate_child_cursor.as_ref()
            .and_then(|cur| keys.iter().position(|k| k == cur))
            .unwrap_or(0);
        let last_idx = keys.len() - 1;
        let need_parent_advance = if forward { cur_idx >= last_idx } else { cur_idx == 0 };

        if need_parent_advance {
            // アーカイブ端では advance_spread_step が境界ガードで何もしないことがある。
            // その場合は「親が実際には動いていない」ので、反対側のページへカーソルを
            // 飛ばしてはいけない（無条件フリップになっていたのが不具合の原因）。
            let moved = {
                let mut viewer_guard = self.viewer.lock().unwrap();
                let Some(viewer) = viewer_guard.as_mut() else { return };
                let before = viewer.spread_lo();
                let total = viewer.entries().len();
                viewer.advance_spread_step(total, forward);
                viewer.spread_lo() != before
            };
            if moved {
                // 独立Contextのビューアー窓を起こして、書き換えた表示ページを即座に反映させる
                // （でないと次にビューアー窓側で入力が起きるまで再描画されない）。
                if let Some(ctx) = &self.viewer_egui_ctx {
                    ctx.request_repaint();
                }
                let new_keys = self.current_page_keys();
                self.translate_child_cursor = if forward { new_keys.first().cloned() } else { new_keys.last().cloned() };
            }
        } else {
            let next_idx = if forward { cur_idx + 1 } else { cur_idx - 1 };
            self.translate_child_cursor = keys.get(next_idx).cloned();
        }
        self.translate_ocr_status = None;
        self.sync_child_panes_display();
    }

    /// `translate_child_cursor`が親の現在の可視集合に含まれなくなっていたら
    /// （＝子ウィンドウの操作以外で親が動いた）先頭ページへ再同期する。
    fn resync_child_cursor_if_needed(&mut self) {
        let keys = self.current_page_keys();
        let still_valid = self.translate_child_cursor.as_ref().is_some_and(|cur| keys.contains(cur));
        if !still_valid {
            self.translate_child_cursor = keys.first().cloned();
            self.sync_child_panes_display();
        }
    }

    /// `translate_child_cursor`が指すページの保存済みOCR txtを読み直し、左ペイン表示を更新する。
    fn sync_child_ocr_display(&mut self) {
        self.translate_child_ocr_lines = match self.translate_child_cursor.clone() {
            Some((path, orig)) => self.load_ocr_text_for(&path, orig).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// `translate_child_cursor`が指すページの保存済み翻訳txtを読み直し、右ペイン表示を更新する。
    /// OCRとは完全に独立した処理単位なので、OCR未実行でも空のまま(未実行扱い)で構わない。
    fn sync_child_translation_display(&mut self) {
        self.translate_child_translation_lines = match self.translate_child_cursor.clone() {
            Some((path, orig)) => self.load_translated_text_for(&path, orig).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// カーソル移動時にOCR・翻訳の両ペインをまとめて読み直す。
    fn sync_child_panes_display(&mut self) {
        self.sync_child_ocr_display();
        self.sync_child_translation_display();
        self.sync_child_lang_from_meta_if_new_archive();
    }

    /// アーカイブが切り替わった時だけ、原文/翻訳先言語を保存済みメタから復元する。
    /// 保存済みメタが無ければ未設定(None)に戻す（アーカイブごとに言語が違う可能性があり、
    /// 前のアーカイブの選択を持ち越すと誤訳の温床になるため）。
    fn sync_child_lang_from_meta_if_new_archive(&mut self) {
        let Some((archive_path, _)) = self.translate_child_cursor.clone() else { return };
        if self.translate_child_lang_synced_for.as_ref() == Some(&archive_path) {
            return;
        }
        self.translate_child_lang_synced_for = Some(archive_path.clone());
        let meta = self.load_translate_lang_meta_for(&archive_path);
        self.translate_child_source_lang = meta.map(|(source, _)| source);
        self.translate_child_target_lang = meta.map(|(_, target)| target);
    }

    /// 子ウィンドウの[再取得]: `translate_child_cursor`が指す1ページのみOCRを再実行する。
    /// 既存のtxt直置きパイプライン(start_ocr_for/translate_ocr_rx)をそのまま流用する。
    fn trigger_child_ocr_retry(&mut self, ctx: &egui::Context) {
        if self.translate_cfg.ocr_model.trim().is_empty() {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_model_missing().to_string());
            return;
        }
        let Some(key) = self.translate_child_cursor.clone() else {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_no_page().to_string());
            return;
        };
        self.translate_ocr_queue.clear();
        self.start_ocr_for(ctx, key);
    }

    /// 子ウィンドウの[再翻訳]: OCR・翻訳は完全に独立したボタン/処理なので、E2Eで自動連鎖は
    /// しない。絶対条件として対象ページのOCR txtが取得済みであることを要求し、未取得なら
    /// フォールバックメッセージを出すだけで実処理は走らせない。
    fn trigger_child_retranslate(&mut self, ctx: &egui::Context) {
        if self.translate_child_ocr_lines.is_empty() {
            self.translate_translate_status = Some(i18n::t().translate_child_ocr_required().to_string());
            return;
        }
        let Some(source) = self.translate_child_source_lang else {
            self.translate_translate_status = Some(i18n::t().translate_child_lang_required().to_string());
            return;
        };
        let Some(target) = self.translate_child_target_lang else {
            self.translate_translate_status = Some(i18n::t().translate_child_lang_required().to_string());
            return;
        };
        if self.translate_cfg.translation_model.trim().is_empty() {
            self.translate_translate_status = Some(i18n::t().translate_overlay_model_missing().to_string());
            return;
        }
        let Some(key) = self.translate_child_cursor.clone() else { return };
        self.translate_translate_status = Some(i18n::t().translate_overlay_running().to_string());
        self.translate_translate_inflight_key = Some(key);
        self.translate_translate_inflight_lang = Some((source, target));
        self.translate_translate_rx = Some(crate::translate::spawn_translate_request(
            ctx.clone(),
            self.translate_cfg.base_url.clone(),
            self.translate_cfg.translation_model.clone(),
            self.translate_child_ocr_lines.clone(),
            source,
            target,
        ));
    }

    /// 翻訳リクエストの完了/失敗をポーリングする。OCRのポーリング(poll_translate_ocr)とは
    /// 別経路・別状態（完全に独立した処理単位のため）。
    fn poll_translate_translation(&mut self) {
        let Some(rx) = &self.translate_translate_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        match msg {
            crate::translate::TranslateMsg::Result(page) => {
                if let Some((archive_path, orig)) = self.translate_translate_inflight_key.take() {
                    self.save_translated_text_for(&archive_path, orig, &page.lines);
                    if let Some((source, target)) = self.translate_translate_inflight_lang.take() {
                        self.save_translate_lang_meta_for(&archive_path, source, target);
                    }
                    if self.translate_child_cursor.as_ref() == Some(&(archive_path, orig)) {
                        self.sync_child_translation_display();
                    }
                }
                self.translate_translate_status = if page.raw_fallback {
                    Some(i18n::t().translate_overlay_fallback_notice().to_string())
                } else {
                    None
                };
            }
            crate::translate::TranslateMsg::Failed(e) => {
                self.translate_translate_status = Some(format!("{}: {e}", i18n::t().translate_overlay_failed_prefix()));
                self.translate_translate_inflight_key = None;
                self.translate_translate_inflight_lang = None;
            }
        }
        self.translate_translate_rx = None;
    }

    /// OCR/翻訳子ウィンドウの1フレーム描画。winit_app が子窓の egui パスから呼ぶ。
    /// 左＝OCR原文、右＝翻訳結果のDIFF風2ペイン。OCRと翻訳は完全に独立したボタン/処理単位。
    pub fn render_translate_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // ビューアー窓側の通常のページ送りでこの独立Contextの窓を起こせるよう、
        // 毎フレーム自身の ctx を保持しておく（render_viewer側の対応処理と対）。
        self.translate_egui_ctx = Some(ctx.clone());
        self.poll_translate_ocr(&ctx);
        self.poll_translate_translation();

        if self.translate_child_cursor.is_none() {
            self.translate_child_cursor = self.current_page_keys().into_iter().next();
            self.sync_child_panes_display();
        }
        self.resync_child_cursor_if_needed();

        ui.horizontal(|ui| {
            if ui.small_button("<").clicked() {
                self.step_child_page(false);
            }
            if ui.small_button(">").clicked() {
                self.step_child_page(true);
            }
            ui.separator();
            if ui.small_button(i18n::t().translate_child_retry_button()).clicked() {
                self.trigger_child_ocr_retry(&ctx);
            }
            egui::ComboBox::from_id_salt("translate_child_source_lang")
                .selected_text(
                    self.translate_child_source_lang
                        .map(|l| i18n::t().translate_lang_label(l))
                        .unwrap_or_else(|| i18n::t().translate_lang_unset_label()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.translate_child_source_lang, None, i18n::t().translate_lang_unset_label());
                    // 翻訳先と同じ言語は選べない（原文=翻訳先を弾く）。
                    for lang in crate::translate::TranslateLang::ALL.into_iter().filter(|l| Some(*l) != self.translate_child_target_lang) {
                        ui.selectable_value(&mut self.translate_child_source_lang, Some(lang), i18n::t().translate_lang_label(lang));
                    }
                });
            ui.label("→");
            egui::ComboBox::from_id_salt("translate_child_target_lang")
                .selected_text(
                    self.translate_child_target_lang
                        .map(|l| i18n::t().translate_lang_label(l))
                        .unwrap_or_else(|| i18n::t().translate_lang_unset_label()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.translate_child_target_lang, None, i18n::t().translate_lang_unset_label());
                    // 原文と同じ言語は選べない（原文=翻訳先を弾く）。
                    for lang in crate::translate::TranslateLang::ALL.into_iter().filter(|l| Some(*l) != self.translate_child_source_lang) {
                        ui.selectable_value(&mut self.translate_child_target_lang, Some(lang), i18n::t().translate_lang_label(lang));
                    }
                });
            if ui.small_button(i18n::t().translate_child_retranslate_button()).clicked() {
                self.trigger_child_retranslate(&ctx);
            }
            // txtは手動でノイズ取り(誤読訂正・整形)した内容を「翻訳の原本」として
            // 扱いたいという要望から、OSのファイラーで直接開けるようにしている。
            if ui.small_button(i18n::t().translate_overlay_open_folder_button()).clicked() {
                self.open_translate_text_folder();
            }
            ui.separator();
            ui.checkbox(&mut self.translate_window_always_on_top, i18n::t().translate_child_always_on_top_toggle());
        });
        if let Some(status) = &self.translate_ocr_status {
            ui.colored_label(egui::Color32::from_rgb(230, 140, 140), status.as_str());
        }
        ui.separator();

        ui.columns(2, |columns| {
            columns[0].vertical(|ui| {
                ui.label(egui::RichText::new(i18n::t().translate_child_ocr_pane_title()).strong());
                egui::ScrollArea::vertical().id_salt("translate_child_ocr_scroll").show(ui, |ui| {
                    if self.translate_child_ocr_lines.is_empty() {
                        ui.weak(i18n::t().translate_overlay_empty());
                    } else {
                        for line in &self.translate_child_ocr_lines {
                            ui.label(line.as_str());
                        }
                    }
                });
            });
            columns[1].vertical(|ui| {
                ui.label(egui::RichText::new(i18n::t().translate_child_translation_pane_title()).strong());
                if let Some(status) = &self.translate_translate_status {
                    ui.colored_label(egui::Color32::from_rgb(230, 140, 140), status.as_str());
                }
                egui::ScrollArea::vertical().id_salt("translate_child_translation_scroll").show(ui, |ui| {
                    if self.translate_child_translation_lines.is_empty() {
                        ui.weak(i18n::t().translate_overlay_empty());
                    } else {
                        for line in &self.translate_child_translation_lines {
                            ui.label(line.as_str());
                        }
                    }
                });
            });
        });

        // ビューアー窓からのcross-context起床(request_repaint)が環境によっては効かない
        // ケースがある（Wayland: フォーカスの無い窓は自然な再描画要因が無く休止しきってしまい、
        // 明示的な起床だけに頼ると反映が遅れる/効かないことがある）ため、ステータス窓と同じ
        // 「開いている間は自前で定期的に起こし続ける」方式を保険として併用する。
        ctx.request_repaint_after(std::time::Duration::from_millis(400));
    }

    /// ビューアー独立窓の 1 フレーム描画。winit_app がビューアー窓の egui パスから呼ぶ。
    /// 旧 `draw_viewer_viewport` の deferred callback 相当（ページ供給 → show → nav/close 処理）。
    pub fn render_viewer(&mut self, ui: &mut egui::Ui) {
        // OCR/翻訳子ウィンドウから共有 ViewerState 経由でページ送りされた際に、独立 Context
        // のこの窓を起こせるよう毎フレーム自身の ctx を保持しておく。
        self.viewer_egui_ctx = Some(ui.ctx().clone());
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
                Some(viewer) => viewer.show(ui, &*page_cache_guard, &mut *cfg_guard, &self.config.keymap),
                None => return,
            }
        };

        self.maybe_autoopen_translate_window();
        self.draw_translate_open_button(ui.ctx());

        // ビューアー窓自身の通常操作（矢印キー等、viewer.show()内部で完結しoutput.navには
        // 出てこない）でページが変わった場合、独立Contextの子ウィンドウには何も伝わらず
        // フォーカスするまで再描画されない。可視ページ集合の変化を検知して明示的に起こす。
        let current_keys_for_translate = self.current_page_keys();
        if current_keys_for_translate != self.translate_last_seen_parent_keys {
            self.translate_last_seen_parent_keys = current_keys_for_translate;
            if let Some(ctx) = &self.translate_egui_ctx {
                ctx.request_repaint();
            }
        }

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

    /// OCRリクエストの完了/失敗をポーリングする。子ウィンドウ（[再取得]ボタン）からの
    /// リクエストのみが対象（Phase5でビューアー側テストUIの手動実行導線は撤去した）。
    /// 子ウィンドウが閉じていて誰も呼ばなくても、次に開いたときのrender_translate_windowで
    /// 追いついて処理される。
    fn poll_translate_ocr(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.translate_ocr_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        match msg {
            crate::translate::OcrMsg::Result(page) => {
                // 要求時点のページへ保存する（待機中にページ送りされても取り違えないよう
                // trigger_child_ocr_retry側で退避しておいたキーを使う）。
                if let Some((archive_path, orig)) = self.translate_ocr_inflight_key.take() {
                    self.save_ocr_text_for(&archive_path, orig, &page.lines);
                    if self.translate_child_cursor.as_ref() == Some(&(archive_path, orig)) {
                        self.sync_child_ocr_display();
                    }
                }
                self.translate_ocr_status = if page.raw_fallback {
                    Some(i18n::t().translate_overlay_fallback_notice().to_string())
                } else {
                    None
                };
                if let Some(next) = self.translate_ocr_queue.pop_front() {
                    self.start_ocr_for(ctx, next);
                }
            }
            crate::translate::OcrMsg::Failed(e) => {
                self.translate_ocr_status = Some(format!("{}: {e}", i18n::t().translate_overlay_failed_prefix()));
                self.translate_ocr_inflight_key = None;
                self.translate_ocr_queue.clear();
            }
        }
        self.translate_ocr_rx = None;
    }

    /// ビューアー窓側の最小限の導線: 既存txtが無いアーカイブでOCR/翻訳子ウィンドウを
    /// ユーザーの意思で開くための[翻訳]ボタンのみ（旧テストUIはPhase5で撤去）。
    /// URL未設定なら何も描画しない。
    fn draw_translate_open_button(&mut self, ctx: &egui::Context) {
        if self.translate_window_open || self.translate_cfg.base_url.trim().is_empty() {
            return;
        }

        use crate::translate::OverlayCorner;
        let corner = self.translate_cfg.overlay_corner;
        let (anchor, offset) = match corner {
            OverlayCorner::TopLeft => (egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0)),
            OverlayCorner::TopRight => (egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0)),
            OverlayCorner::BottomLeft => (egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0)),
            OverlayCorner::BottomRight => (egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0)),
        };

        egui::Area::new(egui::Id::new("translate_open_button"))
            .anchor(anchor, offset)
            .movable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(egui::Color32::from_black_alpha(190)).show(ui, |ui| {
                    if ui.small_button(i18n::t().translate_open_window_button()).clicked() {
                        self.translate_window_open = true;
                    }
                });
            });
    }

    /// キュー内の次の1ページ分のOCRリクエストを実際に発火する。
    fn start_ocr_for(&mut self, ctx: &egui::Context, key: (std::path::PathBuf, usize)) {
        let Some(rgba) = self.full_res_page_rgba_at(&key.0, key.1) else {
            self.translate_ocr_status = Some(i18n::t().translate_overlay_no_page().to_string());
            return;
        };
        let image = image::DynamicImage::ImageRgba8(rgba);
        self.translate_ocr_status = Some(i18n::t().translate_overlay_running().to_string());
        self.translate_ocr_inflight_key = Some(key);
        self.translate_ocr_rx = Some(crate::translate::spawn_ocr_request(
            ctx.clone(),
            self.translate_cfg.base_url.clone(),
            self.translate_cfg.ocr_model.clone(),
            image,
        ));
    }

    /// 現在表示中の全ページ(見開き時は2件、単独ページなら1件)を
    /// (アーカイブパス, original_index)の一覧として、entries順(=読み順どおり)に返す。
    fn current_page_keys(&self) -> Vec<(std::path::PathBuf, usize)> {
        let viewer_guard = self.viewer.lock().unwrap();
        let Some(viewer) = viewer_guard.as_ref() else { return Vec::new() };
        let path = viewer.archive_path().clone();
        viewer.visible_original_indices().into_iter().map(|orig| (path.clone(), orig)).collect()
    }

    /// OCR用: 表示解像度に依存しない原寸デコードで指定ページのRGBA画像を取得する。
    /// 通常表示用のpage_cache（ビューアー窓の実描画サイズへ縮小済み）とは別に、都度
    /// アーカイブから直接デコードする（OCR専用、呼び出し頻度が低いため通常のprefetch/
    /// キャッシュ経路には乗せない）。アニメーションページはOCR対象外。
    fn full_res_page_rgba_at(&self, archive_path: &std::path::Path, original_index: usize) -> Option<image::RgbaImage> {
        let entry_name = {
            let viewer_guard = self.viewer.lock().unwrap();
            let viewer = viewer_guard.as_ref()?;
            if viewer.archive_path().as_path() != archive_path { return None; }
            viewer.entry_name_for(original_index)?.to_string()
        };
        let filter = self.config.viewer_filter.to_image_filter();
        let exif_enabled = self.viewer_cfg.lock().unwrap().exif_orientation_enabled;
        crate::cache::decode_full_res_static_page(
            archive_path,
            &entry_name,
            filter,
            self.cache_budget_bytes,
            self.anim_ring_bounds,
            self.config.anim_frame_hard_limit_mb * 1024 * 1024,
            exif_enabled,
        )
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

    fn save_translated_text_for(&self, archive_path: &std::path::Path, original_index: usize, lines: &[String]) {
        let Some(neko_dir) = self.translate_neko_dir_for(archive_path) else { return };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return };
        let _ = crate::translate::save_translated_text(&neko_dir, filename, original_index, lines);
    }

    fn load_translated_text_for(&self, archive_path: &std::path::Path, original_index: usize) -> Option<Vec<String>> {
        let neko_dir = self.translate_neko_dir_for(archive_path)?;
        let filename = archive_path.file_name().and_then(|n| n.to_str())?;
        crate::translate::load_translated_text(&neko_dir, filename, original_index)
    }

    fn save_translate_lang_meta_for(&self, archive_path: &std::path::Path, source: crate::translate::TranslateLang, target: crate::translate::TranslateLang) {
        let Some(neko_dir) = self.translate_neko_dir_for(archive_path) else { return };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return };
        let _ = crate::translate::save_translate_lang_meta(&neko_dir, filename, source, target);
    }

    fn load_translate_lang_meta_for(&self, archive_path: &std::path::Path) -> Option<(crate::translate::TranslateLang, crate::translate::TranslateLang)> {
        let neko_dir = self.translate_neko_dir_for(archive_path)?;
        let filename = archive_path.file_name().and_then(|n| n.to_str())?;
        crate::translate::load_translate_lang_meta(&neko_dir, filename)
    }

    /// 現在のアーカイブに対応するOCR txtフォルダをOSのファイラーで開く
    /// （手動でのノイズ取り・整形をそのまま「翻訳の原本」として使ってもらうための導線）。
    fn open_translate_text_folder(&self) {
        let Some((archive_path, _)) = self.current_page_keys().into_iter().next() else { return };
        let Some(neko_dir) = self.translate_neko_dir_for(&archive_path) else { return };
        let Some(filename) = archive_path.file_name().and_then(|n| n.to_str()) else { return };
        crate::translate::open_in_file_manager(&crate::translate::ocr_text_dir(&neko_dir, filename));
    }
}
