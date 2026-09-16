//! ビューアー内ツールパレット（オーバーレイ、カスタマイザブルなグリッド式ショートカット）。
//! 固定5x2グリッド。各マスは空欄／ワンアクション型（Toggle）／ダイアログ型（Dialog）の
//! いずれかを保持する。座標・LOCK・透過度・可視性・マス内容は ViewerConfig（gui_config.rs の
//! state ファイル、image_filter と同じ key=value 方式）へ永続化する。

pub mod action;
pub mod dialog;
pub mod toggle;

pub use action::{ActionKind, ALL_ACTION_KINDS};
pub use dialog::{create_dialog, DialogKind, ALL_DIALOG_KINDS};
pub use toggle::{execute_toggle, find_toggle_def, ToggleKind, TOGGLE_DEFS};

/// グリッド列数（固定）。
pub const GRID_COLS: usize = 5;
/// グリッド行数（固定）。
pub const GRID_ROWS: usize = 2;
/// マス総数。
pub const SLOT_COUNT: usize = GRID_COLS * GRID_ROWS;

/// パレット背景の透過度下限/上限(%)。
pub const OPACITY_FLOOR_PCT: u8 = 10;
pub const OPACITY_CEILING_PCT: u8 = 100;

/// マス1個の一辺サイズ(px)の段階。ヘッダーのサイズボタンでこの配列を巡回する。
/// マス内容（Toggleラベル・Dialogアイコン等）はビットマップの拡縮ではなく、
/// この値をそのつど描画関数へ渡して都度描き直す（拡縮によるボケ・ギザギザ回避）。
pub const SLOT_SIZE_STEPS_PX: [f32; 5] = [16.0, 24.0, 32.0, 48.0, 64.0];
/// 既定のマスサイズ段階index（48px = 現行サイズ）。
pub const SLOT_SIZE_DEFAULT_IDX: usize = 3;

/// 1マスの内容。
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum PaletteSlotContent {
    /// 未登録
    #[default]
    Empty,
    /// ワンアクション型（即時トグル実行）
    Toggle(ToggleKind),
    /// ダイアログ型（クリックでミニUI展開）
    Dialog(DialogKind),
    /// ワンアクション型（ページ送り/戻り。ViewerStateの状態を直接操作するため
    /// Toggleとは異なりview_reader.rs側で実行本体を持つ）
    Action(ActionKind),
}

/// state ファイルへ確定保存するまでのデバウンス時間(ms)。ドラッグ中の連続した
/// 座標変化のたびにディスク書き込みが走るのを防ぐ（image_filterの即時セーブと同じ考え方）。
pub const PERSIST_DEBOUNCE_MS: u64 = 500;

/// ツールパレット本体の状態。カスタム名称(String)を持つためCopyは実装できない
/// （以前はCopyだった。ViewerConfig側もCloneのみに変更済み）。フレーム毎の変更検知は
/// 値コピーではなくclone()で行う。
#[derive(Clone, PartialEq, Debug)]
pub struct PaletteState {
    /// パレット左上のスクリーン座標
    pub pos: (f32, f32),
    /// true = ドラッグ移動を禁止
    pub locked: bool,
    /// true = 自動ハイドを禁止（常時 (100-透過度)% で表示し続ける）。
    /// false = ポインタがパレット外に出て0.5秒経過すると自動的に無描画状態へ隠れる。
    pub auto_hide_locked: bool,
    /// 背景の透過度(10〜100%)
    pub opacity_pct: u8,
    /// false = パレット全体を非表示（右クリックで復帰）
    pub visible: bool,
    /// マスサイズ段階（SLOT_SIZE_STEPS_PX のindex）。ヘッダーのサイズボタンで巡回。
    pub slot_size_idx: usize,
    /// 各マスの内容。GRID_COLS×GRID_ROWS、行優先（index = row*GRID_COLS+col）
    pub slots: [PaletteSlotContent; SLOT_COUNT],
    /// マス毎のカスタム表示名。Noneならデフォルトラベル（Toggle/Dialogの定義名）を使う。
    /// 空文字での確定は「何も表示しない」を意味し、デフォルトへは戻さない。
    pub custom_labels: [Option<String>; SLOT_COUNT],
}

impl PaletteState {
    /// 現在のマス一辺サイズ(px)。
    pub fn slot_size_px(&self) -> f32 {
        SLOT_SIZE_STEPS_PX[self.slot_size_idx.min(SLOT_SIZE_STEPS_PX.len() - 1)]
    }

    /// サイズボタン押下時: 次の段階へ巡回（末尾まで行ったら先頭へ戻る）。
    pub fn cycle_slot_size(&mut self) {
        self.slot_size_idx = (self.slot_size_idx + 1) % SLOT_SIZE_STEPS_PX.len();
    }
}

impl Default for PaletteState {
    fn default() -> Self {
        Self {
            pos: (32.0, 32.0),
            locked: false,
            auto_hide_locked: true,
            opacity_pct: OPACITY_CEILING_PCT,
            visible: true,
            slot_size_idx: SLOT_SIZE_DEFAULT_IDX,
            slots: [PaletteSlotContent::Empty; SLOT_COUNT],
            custom_labels: [(); SLOT_COUNT].map(|_| None),
        }
    }
}

// ── 永続化（gui_config.rs の state ファイル） ───────────────────────
// PaletteState自体はViewerConfigにそのまま埋め込む。ここではスロット内容⇔文字列の
// 変換のみ提供する（gui_config.rsがカンマ区切りで tool_palette_slots として読み書きする）。

/// スロット内容 → 永続化用ID文字列。
pub fn slot_content_to_id(content: PaletteSlotContent) -> String {
    match content {
        PaletteSlotContent::Empty => "empty".to_string(),
        PaletteSlotContent::Toggle(k) => format!("toggle:{}", k.id()),
        PaletteSlotContent::Dialog(k) => format!("dialog:{}", k.id()),
        PaletteSlotContent::Action(k) => format!("action:{}", k.id()),
    }
}

/// 永続化用ID文字列 → スロット内容。未知IDやパース失敗は Empty 扱い（前方互換）。
pub fn slot_content_from_id(s: &str) -> PaletteSlotContent {
    if let Some(rest) = s.strip_prefix("toggle:")
        && let Some(k) = ToggleKind::from_id(rest)
    {
        return PaletteSlotContent::Toggle(k);
    }
    if let Some(rest) = s.strip_prefix("dialog:")
        && let Some(k) = DialogKind::from_id(rest)
    {
        return PaletteSlotContent::Dialog(k);
    }
    if let Some(rest) = s.strip_prefix("action:")
        && let Some(k) = ActionKind::from_id(rest)
    {
        return PaletteSlotContent::Action(k);
    }
    PaletteSlotContent::Empty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_content_id_roundtrip() {
        let cases = [
            PaletteSlotContent::Empty,
            PaletteSlotContent::Toggle(ToggleKind::BlueLightCut),
            PaletteSlotContent::Dialog(DialogKind::ImageFilter),
            PaletteSlotContent::Action(ActionKind::NextPage),
            PaletteSlotContent::Action(ActionKind::PrevPage),
            PaletteSlotContent::Action(ActionKind::OpenFolder),
        ];
        for c in cases {
            let id = slot_content_to_id(c);
            assert_eq!(slot_content_from_id(&id), c);
        }
    }

    #[test]
    fn slot_content_from_unknown_id_is_empty() {
        assert_eq!(slot_content_from_id("toggle:nonexistent"), PaletteSlotContent::Empty);
        assert_eq!(slot_content_from_id("garbage"), PaletteSlotContent::Empty);
    }

    #[test]
    fn palette_state_default_is_all_empty() {
        let st = PaletteState::default();
        assert!(st.slots.iter().all(|s| *s == PaletteSlotContent::Empty));
        assert_eq!(st.slots.len(), GRID_COLS * GRID_ROWS);
        assert!(st.custom_labels.iter().all(|l| l.is_none()));
    }
}
