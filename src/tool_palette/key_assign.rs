//! ツールボックス機能のキー割当ダイアログ。キーアサイン設定タブとは別導線で、
//! マスの右クリックメニュー「キー割当」から開く。割り当ての実体は keymap.ini の
//! palette セクション（Keymap::assign_palette_keyboard）で、ここでは入力と表示のみを扱う。
//! 確定結果は呼び出し側（view_reader → ViewerOutput → viewer_host）が keymap へ反映・保存する。
//!
//! 修飾キーはダイアログ左側のトグルだけで決める（物理的に押された修飾キーは無視し、
//! キーイベントからは主キー1つだけを拾う）。Esc は割り当て不可で、押しても何もしない。

use egui::Key;

use crate::keymap::{KeyCombo, Keymap, ViewerKeyOwner};

/// ダイアログの状態。開いている間だけ ViewerState が保持する。
pub struct KeyAssignDialog {
    /// 対象機能の永続化ID（slot_content_to_id の値）
    pub id: String,
    /// 見出しに出す機能名（開いたマスのカスタム名）
    pub name: String,
    /// 開いた時点の割り当て。確定時にこれと同じなら何もせず閉じる。
    original: Option<KeyCombo>,
    key: Option<Key>,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

/// 1フレーム分の操作結果。
#[derive(Debug, PartialEq)]
pub enum KeyAssignOutcome {
    /// 継続（まだ開いている）
    Open,
    /// キャンセル、または割り当てを変えずに保存した（keymap には触らない）
    Close,
    /// 確定。None = 割当解除。衝突相手の割り当ては keymap 側で外す。
    Save(Option<KeyCombo>),
}

impl KeyAssignDialog {
    pub fn new(id: String, name: String, current: Option<KeyCombo>) -> Self {
        Self {
            id,
            name,
            original: current,
            key: current.map(|k| k.key),
            shift: current.is_some_and(|k| k.shift),
            ctrl: current.is_some_and(|k| k.ctrl),
            alt: current.is_some_and(|k| k.alt),
        }
    }

    /// 現在ダイアログ上で組み立てている割り当て。主キーが無ければ割当なし。
    pub fn combo(&self) -> Option<KeyCombo> {
        self.key.map(|key| KeyCombo { key, ctrl: self.ctrl, shift: self.shift, alt: self.alt })
    }

    /// 主キーを1つ受け付ける。Esc は割り当て不可のため無視する。
    fn capture(&mut self, key: Key) {
        if key != Key::Escape {
            self.key = Some(key);
        }
    }

    /// 保存ボタン押下時の結果。
    fn confirm(&self) -> KeyAssignOutcome {
        let combo = self.combo();
        if combo == self.original { KeyAssignOutcome::Close } else { KeyAssignOutcome::Save(combo) }
    }
}

/// 表示用のキー文字列（例: "Ctrl + Shift + M"）。
pub fn combo_display(kb: KeyCombo) -> String {
    let mut parts = Vec::new();
    if kb.ctrl { parts.push("Ctrl"); }
    if kb.shift { parts.push("Shift"); }
    if kb.alt { parts.push("Alt"); }
    parts.push(kb.key.name());
    parts.join(" + ")
}

const CONFLICT_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 70, 70);
const KEYCAP_SIZE: egui::Vec2 = egui::vec2(200.0, 64.0);
const MOD_TOGGLE_SIZE: egui::Vec2 = egui::vec2(64.0, 20.0);

/// ダイアログを1フレーム描く。`owner_name` は衝突相手の表示名を返す。
pub fn show(
    ctx: &egui::Context,
    state: &mut KeyAssignDialog,
    keymap: &Keymap,
    lang: crate::i18n::Lang,
    owner_name: impl Fn(&ViewerKeyOwner) -> String,
) -> KeyAssignOutcome {
    // 主キーを拾い、キーイベントはダイアログ内のウィジェットへ渡さない
    // （フォーカス中のボタンが Space/Enter で押されてしまうのを防ぐ）。
    let captured = ctx.input_mut(|i| {
        let key = i.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
            _ => None,
        });
        i.events.retain(|e| !matches!(e, egui::Event::Key { .. }));
        key
    });
    if let Some(key) = captured {
        state.capture(key);
    }

    let mut outcome = KeyAssignOutcome::Open;
    egui::Modal::new(egui::Id::new("tool_palette_key_assign")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.label(egui::RichText::new(lang.tool_palette_key_assign_title()).small().weak());
        ui.heading(&state.name);
        ui.separator();

        ui.horizontal(|ui| {
            // 修飾キー：縦3連トグル
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (label, value) in [("SHIFT", &mut state.shift), ("CTRL", &mut state.ctrl), ("ALT", &mut state.alt)] {
                    if ui.add_sized(MOD_TOGGLE_SIZE, egui::Button::new(label).small().selected(*value)).clicked() {
                        *value = !*value;
                    }
                }
            });
            draw_keycap(ui, state.combo(), lang);
            if ui.button(lang.tool_palette_key_assign_unassign()).clicked() {
                state.key = None;
            }
        });
        ui.label(lang.tool_palette_key_assign_prompt());

        let conflict = state.combo().and_then(|kb| {
            keymap.find_viewer_keyboard_conflict(kb, &ViewerKeyOwner::Palette(state.id.clone()))
        });
        if let Some(owner) = &conflict {
            let name = owner_name(owner);
            ui.add_space(4.0);
            ui.colored_label(CONFLICT_COLOR, lang.tool_palette_key_assign_conflict(&name));
            ui.colored_label(CONFLICT_COLOR, lang.tool_palette_key_assign_overwrite_note(&name));
        }

        ui.separator();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let save_label = if conflict.is_some() {
                lang.tool_palette_key_assign_overwrite()
            } else {
                lang.tool_palette_key_assign_save()
            };
            if ui.button(save_label).clicked() {
                outcome = state.confirm();
            }
            if ui.button(lang.tool_palette_key_assign_cancel()).clicked() {
                outcome = KeyAssignOutcome::Close;
            }
        });
    });
    outcome
}

/// キーボードのキーに見立てた四角枠。割り当てが無ければ「割当なし」。
fn draw_keycap(ui: &mut egui::Ui, combo: Option<KeyCombo>, lang: crate::i18n::Lang) {
    let (rect, _) = ui.allocate_exact_size(KEYCAP_SIZE, egui::Sense::hover());
    let visuals = ui.visuals();
    ui.painter().rect(
        rect,
        6.0,
        visuals.extreme_bg_color,
        egui::Stroke::new(2.0, visuals.widgets.inactive.fg_stroke.color),
        egui::StrokeKind::Inside,
    );
    let (text, font, color) = match combo {
        Some(kb) => (combo_display(kb), egui::FontId::proportional(20.0), visuals.strong_text_color()),
        None => (lang.tool_palette_key_assign_none().to_string(), egui::FontId::proportional(14.0), visuals.weak_text_color()),
    };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_is_not_captured() {
        let mut d = KeyAssignDialog::new("toggle:magnifier".into(), "虫眼鏡".into(), None);
        d.capture(Key::Escape);
        assert_eq!(d.combo(), None);
        d.capture(Key::M);
        assert_eq!(d.combo(), Some(KeyCombo::plain(Key::M)));
    }

    #[test]
    fn modifiers_come_from_toggles() {
        let mut d = KeyAssignDialog::new("toggle:magnifier".into(), "虫眼鏡".into(), None);
        d.ctrl = true;
        d.capture(Key::M);
        assert_eq!(d.combo(), Some(KeyCombo { key: Key::M, ctrl: true, shift: false, alt: false }));
    }

    #[test]
    fn confirm_without_change_closes_and_unassign_saves_none() {
        let cur = KeyCombo::plain(Key::M);
        let mut d = KeyAssignDialog::new("toggle:magnifier".into(), "虫眼鏡".into(), Some(cur));
        assert_eq!(d.confirm(), KeyAssignOutcome::Close);
        d.key = None;
        assert_eq!(d.confirm(), KeyAssignOutcome::Save(None));
    }

    #[test]
    fn combo_display_orders_modifiers() {
        let kb = KeyCombo { key: Key::M, ctrl: true, shift: true, alt: false };
        assert_eq!(combo_display(kb), "Ctrl + Shift + M");
    }
}
