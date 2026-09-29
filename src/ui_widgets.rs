//! 画面横断で使う小物ウィジェット。

/// ×マークの半径（ボタン短辺に対する比）。22px のボタンで約5px になる。
const CLOSE_MARK_HALF_RATIO: f32 = 0.23;
const CLOSE_MARK_WIDTH: f32 = 1.5;

/// 閉じるボタン。✕グリフはフォントチェーン（egui標準 → PrimaryCJK）に収録がなく
/// 豆腐化するため、空ボタンの上に線2本で×を描く。
/// `ui.put` / `add_sized` で渡された枠いっぱいに広がった場合も、実際の枠に合わせて描く。
pub fn close_x_button(size: egui::Vec2) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let resp = ui.add(egui::Button::new("").min_size(size));
        let c = resp.rect.center();
        let half = resp.rect.width().min(resp.rect.height()) * CLOSE_MARK_HALF_RATIO;
        let stroke = egui::Stroke::new(CLOSE_MARK_WIDTH, ui.style().interact(&resp).fg_stroke.color);
        let painter = ui.painter();
        painter.line_segment([c + egui::vec2(-half, -half), c + egui::vec2(half, half)], stroke);
        painter.line_segment([c + egui::vec2(-half, half), c + egui::vec2(half, -half)], stroke);
        resp
    }
}
