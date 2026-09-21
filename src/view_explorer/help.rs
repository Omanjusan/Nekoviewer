//! エクスプローラーのヘルプ（ツールチップ）。メニューバーの新 [?] ボタンでONにしている間だけ、
//! 対象ウィジェットへ0.5秒ホバーするとタイトル枠つきの説明が出る。ON/OFFは非永続（起動時は常にOFF）。
//!
//! egui 標準の tooltip_delay は全ツールチップ共通なので、ヘルプ専用の遅延はここで自前管理する
//! （既存の常時ツールチップの遅延には影響しない）。

use crate::i18n::HelpDoc;

/// ホバー開始からヘルプが出るまでの秒数
const HELP_DELAY_SECS: f64 = 0.5;
/// 直前にヘルプが出ていた場合、この秒数以内に隣のウィジェットへ移ったら待たずに出す
const HELP_GRACE_SECS: f64 = 0.3;
/// 本文の最大幅
const HELP_MAX_WIDTH: f32 = 380.0;

/// ヘルプが出るまでの残り秒数。0以下なら今すぐ出す。
/// `hover_secs` は現在のウィジェットにポインタが乗ってからの経過秒、
/// `since_last_shown` は直前にヘルプを出してからの経過秒（一度も出していなければ None）。
pub(super) fn help_wait_remaining(hover_secs: f64, since_last_shown: Option<f64>) -> f64 {
    if since_last_shown.is_some_and(|s| s < HELP_GRACE_SECS) {
        return 0.0;
    }
    (HELP_DELAY_SECS - hover_secs).max(0.0)
}

/// `on` のとき、`r` に0.5秒ホバーしたらヘルプを出す。無効状態のウィジェットでも出る。
/// 同じ `r` に対して既存のツールチップ（実パス等）を別に出していても、egui が枠ごと縦に積む。
pub(super) fn help_tip(r: &egui::Response, on: bool, doc: &HelpDoc) {
    if !on {
        return;
    }
    let ctx = &r.ctx;
    let hover_key = egui::Id::new("explorer_help_hover");
    let shown_key = egui::Id::new("explorer_help_last_shown");
    let now = ctx.input(|i| i.time);
    // rect_contains_pointer は無効状態のウィジェットや他レイヤー（ウィンドウ・メニュー）の遮蔽も正しく扱う。
    // ボタン押下・ドラッグ中は出さない。
    let hovering = ctx.rect_contains_pointer(r.layer_id, r.rect) && !ctx.input(|i| i.pointer.any_down());
    if !hovering {
        ctx.data_mut(|d| {
            if d.get_temp::<(egui::Id, f64)>(hover_key).is_some_and(|(id, _)| id == r.id) {
                d.remove::<(egui::Id, f64)>(hover_key);
            }
        });
        return;
    }
    let (_, since) = ctx.data_mut(|d| {
        match d.get_temp::<(egui::Id, f64)>(hover_key).filter(|(id, _)| *id == r.id) {
            Some(v) => v,
            None => {
                let v = (r.id, now);
                d.insert_temp(hover_key, v);
                v
            }
        }
    });
    let since_last_shown = ctx.data(|d| d.get_temp::<f64>(shown_key)).map(|t| now - t);
    let remaining = help_wait_remaining(now - since, since_last_shown);
    if remaining > 0.0 {
        ctx.request_repaint_after_secs(remaining as f32);
        return;
    }
    ctx.data_mut(|d| d.insert_temp(shown_key, now));
    egui::Tooltip::for_widget(r).show(|ui| draw_help_doc(ui, doc));
}

/// ヘルプON/OFFを ctx に置く。引数を持ち回りにくい描画関数（ツリー行・右クリックメニュー）が
/// `help_tip_auto` で参照する。エクスプローラーの描画冒頭で毎フレーム同期する。
pub(super) fn sync_help_flag(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("explorer_help_on"), on));
}

/// `sync_help_flag` で同期されたON/OFFに従う `help_tip`。
pub(super) fn help_tip_auto(r: &egui::Response, doc: &HelpDoc) {
    let on = r.ctx.data(|d| d.get_temp::<bool>(egui::Id::new("explorer_help_on"))).unwrap_or(false);
    help_tip(r, on, doc);
}

/// タイトルを枠で囲み、その下に節（見出し＋本文）を行間を空けて並べる。
fn draw_help_doc(ui: &mut egui::Ui, doc: &HelpDoc) {
    ui.set_max_width(HELP_MAX_WIDTH);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(doc.title).strong());
    });
    for (heading, body) in doc.sections {
        ui.add_space(6.0);
        if !heading.is_empty() {
            ui.label(egui::RichText::new(*heading).strong());
        }
        ui.label(*body);
    }
}

#[cfg(test)]
mod help_delay_tests {
    use super::help_wait_remaining;

    #[test]
    fn waits_half_a_second_from_hover_start() {
        assert_eq!(help_wait_remaining(0.0, None), 0.5);
        assert!((help_wait_remaining(0.2, None) - 0.3).abs() < 1e-9);
    }

    #[test]
    fn shows_once_half_a_second_has_elapsed() {
        assert_eq!(help_wait_remaining(0.5, None), 0.0);
        assert_eq!(help_wait_remaining(5.0, None), 0.0);
    }

    #[test]
    fn moving_to_a_neighbour_right_after_a_help_shows_it_immediately() {
        assert_eq!(help_wait_remaining(0.0, Some(0.05)), 0.0);
    }

    #[test]
    fn an_old_help_does_not_skip_the_wait() {
        assert_eq!(help_wait_remaining(0.0, Some(2.0)), 0.5);
    }
}
