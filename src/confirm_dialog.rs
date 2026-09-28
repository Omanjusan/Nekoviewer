//! 汎用の確認ダイアログ（egui::Window + キャンセル/OKボタン）。
//! favorites_ui.rs / virtual_ui.rs に同型のモーダル（Window設定＋ボタン行＋pending Option
//! 状態のクリア）が複数個別実装されていたため、外枠とボタン行だけをここへ共通化する。
//! 本文（ラベルや警告表示）は呼び出し側の内容ごとに異なるため、クロージャで描画する。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfirmOutcome {
    None,
    Ok,
    Cancel,
}

pub struct ConfirmDialogSpec {
    /// egui::Id用の固定ソルト。タイトル文言の変更でID衝突が起きないよう、
    /// タイトルとは別に呼び出し箇所ごとの固定文字列を渡す。
    pub id_salt: &'static str,
    pub title: &'static str,
    pub ok_label: &'static str,
    pub cancel_label: &'static str,
    /// true = 常に最前面（既存の仮想フォルダ系ダイアログの挙動を維持するためのフラグ）
    pub foreground: bool,
}

/// 確認ダイアログを描画し、このフレームでの結果を返す。ボタンは常に「キャンセル→OK」の順。
pub fn draw_confirm_dialog(
    ctx: &egui::Context,
    spec: ConfirmDialogSpec,
    body: impl FnOnce(&mut egui::Ui),
) -> ConfirmOutcome {
    let mut outcome = ConfirmOutcome::None;
    let mut window = egui::Window::new(spec.title)
        .id(egui::Id::new(spec.id_salt))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0));
    if spec.foreground {
        window = window.order(egui::Order::Foreground);
    }
    window.show(ctx, |ui| {
        body(ui);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(spec.cancel_label).clicked() {
                outcome = ConfirmOutcome::Cancel;
            }
            if ui.button(spec.ok_label).clicked() {
                outcome = ConfirmOutcome::Ok;
            }
        });
    });
    outcome
}
