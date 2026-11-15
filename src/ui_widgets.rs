//! 画面横断で使う小物ウィジェット。

/// 見出しの下の、押せる項目のインデント幅。
const MENU_ITEM_INDENT: i8 = 14;

/// 右クリックメニューで、見出しの下に並べる「押せる項目」をまとめる。左にインデントを入れて、見出し行と区別する。
pub fn menu_theme_items<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE.inner_margin(egui::Margin { left: MENU_ITEM_INDENT, ..Default::default() }).show(ui, add_contents).inner
}

/// 右クリックメニューのテーマ見出し（太字・クリック不可）。直前のテーマとの間には、呼び出し側が
/// `ui.separator()` を置く（先頭のテーマの前には置かない）。
pub fn menu_theme_heading(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).strong()).selectable(false));
}

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
