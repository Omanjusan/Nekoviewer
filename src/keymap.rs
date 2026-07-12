//! キーアサイン機能(TODO項目J)の型定義。フェーズ0: 型とconfig.ini永続化のみ。
//!
//! 実際のキー判定への組み込み（既存ハードコードキーの置き換え）はフェーズ1(view_reader)/
//! フェーズ2(view_explorer)で行う。ここでは以下のみを扱う:
//!   - KeyCombo / MouseCombo: 1つの入力を表す値（どちらも修飾キー+本体という対称構造）
//!   - ActionBinding: 1アクションにつき「既定キーボード/既定マウス/ユーザー設定キーボード/
//!     ユーザー設定マウス」の4スロット（UI上は「既定・マウス・キーボード」の3項目として見せる）
//!   - ReaderAction / ExplorerAction: 画面ごとに分けたアクションenum
//!   - Keymap: 上記をまとめ、config.ini [keymap] セクションとの相互変換を行う
//!
//! ExplorerAction は、フォーカスペインによって意味が変わる文脈依存操作（矢印キー移動・
//! Enter確定）も対象に含める。実処理側（フェーズ2）でペインごとに同じアクション判定を使う。

use egui::Key;

/// キーボード入力1つ分（修飾キー+主キー）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyCombo {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyCombo {
    pub const fn plain(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: false }
    }
    pub const fn shift(key: Key) -> Self {
        Self { key, ctrl: false, shift: true, alt: false }
    }
    pub const fn alt(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: true }
    }

    pub fn to_config_string(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl  { parts.push("ctrl"); }
        if self.shift { parts.push("shift"); }
        if self.alt   { parts.push("alt"); }
        parts.push(key_name(self.key));
        parts.join("+")
    }

    pub fn from_config_str(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key = None;
        for part in s.split('+') {
            match part {
                "ctrl"  => ctrl = true,
                "shift" => shift = true,
                "alt"   => alt = true,
                other   => key = key_from_name(other),
            }
        }
        key.map(|key| Self { key, ctrl, shift, alt })
    }

    /// このフレームで押されたか（修飾キーの有無も完全一致で判定）。
    pub fn pressed(self, i: &egui::InputState) -> bool {
        i.modifiers.ctrl == self.ctrl
            && i.modifiers.shift == self.shift
            && i.modifiers.alt == self.alt
            && i.key_pressed(self.key)
    }
}

fn key_name(k: Key) -> &'static str {
    match k {
        Key::ArrowUp    => "ArrowUp",
        Key::ArrowDown  => "ArrowDown",
        Key::ArrowLeft  => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::Space      => "Space",
        Key::Enter      => "Enter",
        Key::Escape     => "Escape",
        Key::Home       => "Home",
        Key::End        => "End",
        Key::Tab        => "Tab",
        Key::F2         => "F2",
        Key::F5         => "F5",
        Key::F6         => "F6",
        Key::F7         => "F7",
        Key::F8         => "F8",
        Key::Num1       => "Num1",
        Key::Num2       => "Num2",
        Key::Num3       => "Num3",
        Key::Num4       => "Num4",
        Key::Num5       => "Num5",
        Key::A          => "A",
        _               => "Unknown",
    }
}

fn key_from_name(s: &str) -> Option<Key> {
    Some(match s {
        "ArrowUp"    => Key::ArrowUp,
        "ArrowDown"  => Key::ArrowDown,
        "ArrowLeft"  => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "Space"      => Key::Space,
        "Enter"      => Key::Enter,
        "Escape"     => Key::Escape,
        "Home"       => Key::Home,
        "End"        => Key::End,
        "Tab"        => Key::Tab,
        "F2"         => Key::F2,
        "F5"         => Key::F5,
        "F6"         => Key::F6,
        "F7"         => Key::F7,
        "F8"         => Key::F8,
        "Num1"       => Key::Num1,
        "Num2"       => Key::Num2,
        "Num3"       => Key::Num3,
        "Num4"       => Key::Num4,
        "Num5"       => Key::Num5,
        "A"          => Key::A,
        _            => return None,
    })
}

/// マウスの動作種別。ホイール系はページ送り等、既存キーからの移行分のみを対象にする。
/// ドラッグ等(TODO項目C向け)は、C実装時にここへ追加する。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    WheelUp,
    WheelDown,
    MiddleClick,
}

fn mouse_action_name(a: MouseAction) -> &'static str {
    match a {
        MouseAction::WheelUp     => "wheel_up",
        MouseAction::WheelDown   => "wheel_down",
        MouseAction::MiddleClick => "middle_click",
    }
}

fn mouse_action_from_name(s: &str) -> Option<MouseAction> {
    Some(match s {
        "wheel_up"     => MouseAction::WheelUp,
        "wheel_down"   => MouseAction::WheelDown,
        "middle_click" => MouseAction::MiddleClick,
        _ => return None,
    })
}

/// マウス入力1つ分（修飾キー+動作種別）。KeyComboと対称的な構造にしてあり、キーアサインUIで
/// 「修飾キーのチェックボックス＋動作選択」という共通コンポーネントを両方に使い回せる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseCombo {
    pub action: MouseAction,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl MouseCombo {
    pub const fn plain(action: MouseAction) -> Self {
        Self { action, ctrl: false, shift: false, alt: false }
    }
    pub const fn shift(action: MouseAction) -> Self {
        Self { action, ctrl: false, shift: true, alt: false }
    }

    pub fn to_config_string(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl  { parts.push("ctrl"); }
        if self.shift { parts.push("shift"); }
        if self.alt   { parts.push("alt"); }
        parts.push(mouse_action_name(self.action));
        parts.join("+")
    }

    pub fn from_config_str(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut action = None;
        for part in s.split('+') {
            match part {
                "ctrl"  => ctrl = true,
                "shift" => shift = true,
                "alt"   => alt = true,
                other   => action = mouse_action_from_name(other),
            }
        }
        action.map(|action| Self { action, ctrl, shift, alt })
    }

    /// 修飾キーの状態がこのコンボと一致するか（動作自体が起きたかは呼び出し側が判定する）。
    pub fn modifiers_match(self, i: &egui::InputState) -> bool {
        i.modifiers.ctrl == self.ctrl && i.modifiers.shift == self.shift && i.modifiers.alt == self.alt
    }
}

/// 1アクションぶんの割り当て。既定値2種（キーボード/マウス）と、ユーザーがカスタムした
/// 場合の上書き値2種を持つ。UI上は「既定・キーボード・マウス」の3項目として見せる想定
/// （「既定」列は default_keyboard/default_mouse のうち設定されている方を表示する）。
#[derive(Clone, Copy, Default)]
pub struct ActionBinding {
    pub default_keyboard: Option<KeyCombo>,
    pub default_mouse: Option<MouseCombo>,
    pub keyboard: Option<KeyCombo>,
    pub mouse: Option<MouseCombo>,
}

impl ActionBinding {
    const fn keyboard_only(default: KeyCombo) -> Self {
        Self { default_keyboard: Some(default), default_mouse: None, keyboard: None, mouse: None }
    }
    const fn both(kb: KeyCombo, mouse: MouseCombo) -> Self {
        Self { default_keyboard: Some(kb), default_mouse: Some(mouse), keyboard: None, mouse: None }
    }

    /// 現在有効なキーボード割り当て（ユーザー設定 > 既定）
    pub fn effective_keyboard(&self) -> Option<KeyCombo> {
        self.keyboard.or(self.default_keyboard)
    }
    /// 現在有効なマウス割り当て（ユーザー設定 > 既定）
    pub fn effective_mouse(&self) -> Option<MouseCombo> {
        self.mouse.or(self.default_mouse)
    }

    /// このフレームで、割り当てられたキーボード入力が押されたか。
    pub fn key_pressed(&self, i: &egui::InputState) -> bool {
        self.effective_keyboard().is_some_and(|kb| kb.pressed(i))
    }
}

macro_rules! define_action_enum {
    ($name:ident { $($variant:ident => $str:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn key_str(self) -> &'static str {
                match self {
                    $($name::$variant => $str),+
                }
            }

            pub fn from_key_str(s: &str) -> Option<Self> {
                match s {
                    $($str => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

define_action_enum!(ReaderAction {
    PagePrev            => "PagePrev",
    PageNext            => "PageNext",
    PageAdvanceSpace    => "PageAdvanceSpace",
    FileNavPrev         => "FileNavPrev",
    FileNavNext         => "FileNavNext",
    FileNavPrevAlt      => "FileNavPrevAlt",
    FileNavNextAlt      => "FileNavNextAlt",
    JumpFirstPage       => "JumpFirstPage",
    JumpLastPage        => "JumpLastPage",
    ToggleZoomActual    => "ToggleZoomActual",
    ToggleFullscreen    => "ToggleFullscreen",
    CloseOrExitFullscreen => "CloseOrExitFullscreen",
    PageModeSingle      => "PageModeSingle",
    PageModeSpreadLeft  => "PageModeSpreadLeft",
    PageModeSpreadRight => "PageModeSpreadRight",
    SpreadOffsetPrev    => "SpreadOffsetPrev",
    SpreadOffsetNext    => "SpreadOffsetNext",
    ApplySlot1          => "ApplySlot1",
    ApplySlot2          => "ApplySlot2",
    ApplySlot3          => "ApplySlot3",
    ApplySlot4          => "ApplySlot4",
});

impl ReaderAction {
    /// キーアサイン設定タブでの表示名（日本語固定。多言語化は将来対応）。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::PagePrev            => "前のページ",
            Self::PageNext            => "次のページ",
            Self::PageAdvanceSpace    => "次のページ（Space）",
            Self::FileNavPrev         => "前のファイル",
            Self::FileNavNext         => "次のファイル",
            Self::FileNavPrevAlt      => "前のファイル（副）",
            Self::FileNavNextAlt      => "次のファイル（副）",
            Self::JumpFirstPage       => "先頭ページへ",
            Self::JumpLastPage        => "末尾ページへ",
            Self::ToggleZoomActual    => "等倍/fit切替",
            Self::ToggleFullscreen    => "フルスクリーン切替",
            Self::CloseOrExitFullscreen => "閉じる/フルスクリーン解除",
            Self::PageModeSingle      => "シングルページ表示",
            Self::PageModeSpreadLeft  => "見開き表示（左開始）",
            Self::PageModeSpreadRight => "見開き表示（右開始）",
            Self::SpreadOffsetPrev    => "見開きオフセット（前）",
            Self::SpreadOffsetNext    => "見開きオフセット（次）",
            Self::ApplySlot1          => "ウィンドウスロット1適用",
            Self::ApplySlot2          => "ウィンドウスロット2適用",
            Self::ApplySlot3          => "ウィンドウスロット3適用",
            Self::ApplySlot4          => "ウィンドウスロット4適用",
        }
    }

    pub fn default_binding(self) -> ActionBinding {
        match self {
            Self::PagePrev            => ActionBinding::both(KeyCombo::plain(Key::ArrowUp), MouseCombo::plain(MouseAction::WheelUp)),
            Self::PageNext            => ActionBinding::both(KeyCombo::plain(Key::ArrowDown), MouseCombo::plain(MouseAction::WheelDown)),
            Self::PageAdvanceSpace    => ActionBinding::keyboard_only(KeyCombo::plain(Key::Space)),
            Self::FileNavPrev         => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowLeft)),
            Self::FileNavNext         => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowRight)),
            Self::FileNavPrevAlt      => ActionBinding::both(KeyCombo::shift(Key::ArrowUp), MouseCombo::shift(MouseAction::WheelUp)),
            Self::FileNavNextAlt      => ActionBinding::both(KeyCombo::shift(Key::ArrowDown), MouseCombo::shift(MouseAction::WheelDown)),
            Self::JumpFirstPage       => ActionBinding::keyboard_only(KeyCombo::plain(Key::Home)),
            Self::JumpLastPage        => ActionBinding::keyboard_only(KeyCombo::plain(Key::End)),
            Self::ToggleZoomActual    => ActionBinding::keyboard_only(KeyCombo::plain(Key::Enter)),
            Self::ToggleFullscreen    => ActionBinding::both(KeyCombo::alt(Key::Enter), MouseCombo::plain(MouseAction::MiddleClick)),
            Self::CloseOrExitFullscreen => ActionBinding::keyboard_only(KeyCombo::plain(Key::Escape)),
            Self::PageModeSingle      => ActionBinding::keyboard_only(KeyCombo::plain(Key::Num1)),
            Self::PageModeSpreadLeft  => ActionBinding::keyboard_only(KeyCombo::plain(Key::Num2)),
            Self::PageModeSpreadRight => ActionBinding::keyboard_only(KeyCombo::plain(Key::Num3)),
            Self::SpreadOffsetPrev    => ActionBinding::keyboard_only(KeyCombo::plain(Key::Num4)),
            Self::SpreadOffsetNext    => ActionBinding::keyboard_only(KeyCombo::plain(Key::Num5)),
            Self::ApplySlot1          => ActionBinding::keyboard_only(KeyCombo::plain(Key::F5)),
            Self::ApplySlot2          => ActionBinding::keyboard_only(KeyCombo::plain(Key::F6)),
            Self::ApplySlot3          => ActionBinding::keyboard_only(KeyCombo::plain(Key::F7)),
            Self::ApplySlot4          => ActionBinding::keyboard_only(KeyCombo::plain(Key::F8)),
        }
    }
}

define_action_enum!(ExplorerAction {
    FocusNext      => "FocusNext",
    FocusPrev      => "FocusPrev",
    Rename         => "Rename",
    SelectAll      => "SelectAll",
    ClearSelection => "ClearSelection",
    NavUp          => "NavUp",
    NavDown        => "NavDown",
    NavLeft        => "NavLeft",
    NavRight       => "NavRight",
    NavHome        => "NavHome",
    NavEnd         => "NavEnd",
    Confirm        => "Confirm",
    ExtendUp       => "ExtendUp",
    ExtendDown     => "ExtendDown",
    ExtendLeft     => "ExtendLeft",
    ExtendRight    => "ExtendRight",
    ExtendHome     => "ExtendHome",
    ExtendEnd      => "ExtendEnd",
});

impl ExplorerAction {
    /// キーアサイン設定タブでの表示名（日本語固定。多言語化は将来対応）。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::FocusNext      => "次のペインへフォーカス移動",
            Self::FocusPrev      => "前のペインへフォーカス移動",
            Self::Rename         => "お気に入りフォルダ名変更",
            Self::SelectAll      => "全選択",
            Self::ClearSelection => "選択解除",
            Self::NavUp          => "上へ移動",
            Self::NavDown        => "下へ移動",
            Self::NavLeft        => "左へ移動",
            Self::NavRight       => "右へ移動",
            Self::NavHome        => "先頭へ移動",
            Self::NavEnd         => "末尾へ移動",
            Self::Confirm        => "確定/開く",
            Self::ExtendUp       => "範囲選択拡張（上）",
            Self::ExtendDown     => "範囲選択拡張（下）",
            Self::ExtendLeft     => "範囲選択拡張（左）",
            Self::ExtendRight    => "範囲選択拡張（右）",
            Self::ExtendHome     => "範囲選択拡張（先頭）",
            Self::ExtendEnd      => "範囲選択拡張（末尾）",
        }
    }

    pub fn default_binding(self) -> ActionBinding {
        match self {
            Self::FocusNext      => ActionBinding::keyboard_only(KeyCombo::plain(Key::Tab)),
            Self::FocusPrev      => ActionBinding::keyboard_only(KeyCombo::shift(Key::Tab)),
            Self::Rename         => ActionBinding::keyboard_only(KeyCombo::plain(Key::F2)),
            Self::SelectAll      => ActionBinding::keyboard_only(KeyCombo { key: Key::A, ctrl: true, shift: false, alt: false }),
            Self::ClearSelection => ActionBinding::keyboard_only(KeyCombo::plain(Key::Escape)),
            Self::NavUp          => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowUp)),
            Self::NavDown        => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowDown)),
            Self::NavLeft        => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowLeft)),
            Self::NavRight       => ActionBinding::keyboard_only(KeyCombo::plain(Key::ArrowRight)),
            Self::NavHome        => ActionBinding::keyboard_only(KeyCombo::plain(Key::Home)),
            Self::NavEnd         => ActionBinding::keyboard_only(KeyCombo::plain(Key::End)),
            Self::Confirm        => ActionBinding::keyboard_only(KeyCombo::plain(Key::Enter)),
            Self::ExtendUp       => ActionBinding::keyboard_only(KeyCombo::shift(Key::ArrowUp)),
            Self::ExtendDown     => ActionBinding::keyboard_only(KeyCombo::shift(Key::ArrowDown)),
            Self::ExtendLeft     => ActionBinding::keyboard_only(KeyCombo::shift(Key::ArrowLeft)),
            Self::ExtendRight    => ActionBinding::keyboard_only(KeyCombo::shift(Key::ArrowRight)),
            Self::ExtendHome     => ActionBinding::keyboard_only(KeyCombo::shift(Key::Home)),
            Self::ExtendEnd      => ActionBinding::keyboard_only(KeyCombo::shift(Key::End)),
        }
    }

    /// キーアサインUIで変更可能か。範囲選択拡張系(Shift+矢印/Home/End)は現時点で固定仕様とし、
    /// キーIDとしては公開するがUI上は編集不可（将来の再検討用にスタンバイさせておく）。
    pub fn is_editable(self) -> bool {
        !matches!(
            self,
            Self::ExtendUp | Self::ExtendDown | Self::ExtendLeft | Self::ExtendRight
                | Self::ExtendHome | Self::ExtendEnd
        )
    }
}

/// キーアサイン全体。ReaderAction/ExplorerActionそれぞれの割り当てを保持する。
#[derive(Clone)]
pub struct Keymap {
    reader: std::collections::BTreeMap<ReaderAction, ActionBinding>,
    explorer: std::collections::BTreeMap<ExplorerAction, ActionBinding>,
}

impl Keymap {
    pub fn reader_binding(&self, action: ReaderAction) -> ActionBinding {
        self.reader.get(&action).copied().unwrap_or_else(|| action.default_binding())
    }
    pub fn explorer_binding(&self, action: ExplorerAction) -> ActionBinding {
        self.explorer.get(&action).copied().unwrap_or_else(|| action.default_binding())
    }

    pub fn set_reader_keyboard(&mut self, action: ReaderAction, kb: Option<KeyCombo>) {
        self.reader.entry(action).or_insert_with(|| action.default_binding()).keyboard = kb;
    }
    pub fn set_reader_mouse(&mut self, action: ReaderAction, m: Option<MouseCombo>) {
        self.reader.entry(action).or_insert_with(|| action.default_binding()).mouse = m;
    }
    pub fn set_explorer_keyboard(&mut self, action: ExplorerAction, kb: Option<KeyCombo>) {
        self.explorer.entry(action).or_insert_with(|| action.default_binding()).keyboard = kb;
    }
    pub fn set_explorer_mouse(&mut self, action: ExplorerAction, m: Option<MouseCombo>) {
        self.explorer.entry(action).or_insert_with(|| action.default_binding()).mouse = m;
    }

    /// config.ini [keymap] セクションの1行 "reader.PagePrev.keyboard" = "shift+ArrowUp" を適用する。
    /// 未知のアクション名・スロット名・値は無視する（既存パターン踏襲）。
    pub fn apply_ini_entry(&mut self, key: &str, value: &str) {
        let mut parts = key.splitn(3, '.');
        let (scope, action_name, slot) = match (parts.next(), parts.next(), parts.next()) {
            (Some(s), Some(a), Some(sl)) => (s, a, sl),
            _ => return,
        };
        match scope {
            "reader" => {
                if let Some(action) = ReaderAction::from_key_str(action_name) {
                    apply_slot_reader(self, action, slot, value);
                }
            }
            "explorer" => {
                if let Some(action) = ExplorerAction::from_key_str(action_name) {
                    apply_slot_explorer(self, action, slot, value);
                }
            }
            _ => {}
        }
    }

    /// config.ini [keymap] セクションへ書き出す行を生成する（既定から変更されたものだけ出力）。
    pub fn to_ini_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (&action, binding) in &self.reader {
            push_binding_lines(&mut lines, "reader", action.key_str(), binding);
        }
        for (&action, binding) in &self.explorer {
            push_binding_lines(&mut lines, "explorer", action.key_str(), binding);
        }
        lines
    }

    /// 実行ファイルと同じフォルダの keymap.ini を読み込む。無ければコメント付きテンプレートを
    /// 生成して既定値を返す（config.ini の起動時生成パターンを踏襲）。
    pub fn load() -> Self {
        let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
            return Self::default();
        };
        let path = dir.join("keymap.ini");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                let _ = std::fs::write(&path, KEYMAP_INI_HEADER);
                return Self::default();
            }
        };
        let mut km = Self::default();
        let mut section = String::new();
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_string();
                continue;
            }
            if section != "keymap" {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                km.apply_ini_entry(k.trim(), v.trim());
            }
        }
        km
    }

    /// 設定ダイアログの[反映]時に呼ぶ。tmp→renameで安全に書き込み、bakも残す
    /// （config.ini保存(AppConfig::save)と同じパターン）。
    pub fn save(&self) {
        let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
        let path = dir.join("keymap.ini");
        let mut content = String::from(KEYMAP_INI_HEADER);
        for line in self.to_ini_lines() {
            content.push_str(&line);
            content.push('\n');
        }

        let tmp = dir.join("keymap.ini.tmp");
        let bak = dir.join("keymap.ini.bak");
        if std::fs::write(&tmp, &content).is_err() { return; }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = std::fs::write(&bak, &content);
    }
}

const KEYMAP_INI_HEADER: &str = "\
# ============================================================================
#  Nekoviewer キーアサイン設定 (keymap.ini)
#
#  ・この実行ファイルと同じフォルダに置かれます。
#  ・ファイルを削除すると、次回起動時に既定のキー割り当てに戻ります。
#  ・通常は設定ダイアログの「キーアサイン」タブから変更してください。
#  ・手動編集する場合の形式:
#      reader.<アクション名>.keyboard / .mouse = 値
#      explorer.<アクション名>.keyboard = 値
#    キーボード値の例: ArrowUp / shift+ArrowUp / alt+Enter
#    マウス値の例: wheel_up / shift_wheel_down / middle_click
# ============================================================================

[keymap]
";

impl Default for Keymap {
    fn default() -> Self {
        Self {
            reader: ReaderAction::ALL.iter().map(|&a| (a, a.default_binding())).collect(),
            explorer: ExplorerAction::ALL.iter().map(|&a| (a, a.default_binding())).collect(),
        }
    }
}

fn apply_slot_reader(map: &mut Keymap, action: ReaderAction, slot: &str, value: &str) {
    match slot {
        "keyboard" => map.set_reader_keyboard(action, KeyCombo::from_config_str(value)),
        "mouse"    => map.set_reader_mouse(action, MouseCombo::from_config_str(value)),
        _ => {}
    }
}

fn apply_slot_explorer(map: &mut Keymap, action: ExplorerAction, slot: &str, value: &str) {
    match slot {
        "keyboard" => map.set_explorer_keyboard(action, KeyCombo::from_config_str(value)),
        "mouse"    => map.set_explorer_mouse(action, MouseCombo::from_config_str(value)),
        _ => {}
    }
}

fn push_binding_lines(lines: &mut Vec<String>, scope: &str, action_name: &str, binding: &ActionBinding) {
    if let Some(kb) = binding.keyboard {
        lines.push(format!("{scope}.{action_name}.keyboard = {}", kb.to_config_string()));
    }
    if let Some(m) = binding.mouse {
        lines.push(format!("{scope}.{action_name}.mouse = {}", m.to_config_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_combo_roundtrip() {
        let combo = KeyCombo { key: Key::ArrowUp, ctrl: false, shift: true, alt: false };
        let s = combo.to_config_string();
        assert_eq!(s, "shift+ArrowUp");
        assert_eq!(KeyCombo::from_config_str(&s), Some(combo));
    }

    #[test]
    fn mouse_combo_roundtrip() {
        for m in [
            MouseCombo::plain(MouseAction::WheelUp),
            MouseCombo::plain(MouseAction::WheelDown),
            MouseCombo::shift(MouseAction::WheelUp),
            MouseCombo::shift(MouseAction::WheelDown),
            MouseCombo::plain(MouseAction::MiddleClick),
        ] {
            let s = m.to_config_string();
            assert_eq!(MouseCombo::from_config_str(&s), Some(m));
        }
    }

    #[test]
    fn default_keymap_matches_existing_hardcoded_bindings() {
        let km = Keymap::default();
        let page_prev = km.reader_binding(ReaderAction::PagePrev);
        assert_eq!(page_prev.effective_keyboard(), Some(KeyCombo::plain(Key::ArrowUp)));
        assert_eq!(page_prev.effective_mouse(), Some(MouseCombo::plain(MouseAction::WheelUp)));
    }

    #[test]
    fn user_override_takes_precedence_over_default() {
        let mut km = Keymap::default();
        km.set_reader_keyboard(ReaderAction::PagePrev, Some(KeyCombo::plain(Key::Home)));
        assert_eq!(km.reader_binding(ReaderAction::PagePrev).effective_keyboard(), Some(KeyCombo::plain(Key::Home)));
    }

    #[test]
    fn ini_entry_roundtrip() {
        let mut km = Keymap::default();
        km.apply_ini_entry("reader.PagePrev.keyboard", "shift+ArrowUp");
        assert_eq!(
            km.reader_binding(ReaderAction::PagePrev).effective_keyboard(),
            Some(KeyCombo { key: Key::ArrowUp, ctrl: false, shift: true, alt: false })
        );
        let lines = km.to_ini_lines();
        assert!(lines.iter().any(|l| l == "reader.PagePrev.keyboard = shift+ArrowUp"));
    }

    #[test]
    fn unknown_action_and_slot_are_ignored() {
        let mut km = Keymap::default();
        km.apply_ini_entry("reader.NoSuchAction.keyboard", "ArrowUp");
        km.apply_ini_entry("reader.PagePrev.unknown_slot", "ArrowUp");
        assert!(km.to_ini_lines().is_empty());
    }
}
