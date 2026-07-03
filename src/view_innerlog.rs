//! model_innerlog の描画。ステータス窓（[?]ボタン）下部に、既存のキャッシュ/debug情報の
//! 下段として表示する。

pub(crate) fn draw(ui: &mut egui::Ui) {
    ui.label("Log:");
    let entries = crate::model_innerlog::snapshot();
    let available_h = ui.available_height().max(60.0).min(160.0);
    egui::ScrollArea::vertical()
        .id_salt("innerlog_scroll")
        .max_height(available_h)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if entries.is_empty() {
                ui.label(
                    egui::RichText::new("(no entries)")
                        .monospace()
                        .color(egui::Color32::DARK_GRAY),
                );
            } else {
                // 複数行同時コピー可能にするため TextEdit を使う。内容は毎フレーム
                // model_innerlog から再構築するため、編集操作をしても表示上残らない
                // （見た目上は読み取り専用のログビューとして機能する）。
                let mut text = entries.join("\n");
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
            }
        });
}
