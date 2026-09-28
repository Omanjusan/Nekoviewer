//! ビューアー内ツールパレット（オーバーレイ、カスタマイザブルなグリッド式ショートカット）。
//! 固定5x2グリッド。各マスは空欄／ワンアクション型（Toggle）／ダイアログ型（Dialog）の
//! いずれかを保持する。座標・LOCK・透過度・可視性・マス内容は ViewerConfig（gui_config.rs の
//! state ファイル、image_filter と同じ key=value 方式）へ永続化する。

pub mod action;
pub mod category;
pub mod dialog;
pub mod key_assign;
pub mod toggle;

pub use action::ActionKind;
pub use category::ALL_CATEGORIES;
pub use dialog::{create_dialog, DialogKind};
pub use key_assign::{KeyAssignDialog, KeyAssignOutcome};
pub use toggle::{execute_toggle, find_toggle_def, ToggleKind};

/// グリッド列数（固定）。
pub const GRID_COLS: usize = 5;
/// グリッド行数の既定値（初回起動時の表示行数）。
pub const GRID_ROWS: usize = 2;
/// グリッド行数の上限。行の＋−ボタンで増減できる範囲は 1..=MAX_GRID_ROWS。
pub const MAX_GRID_ROWS: usize = 5;
/// グリッド行数の下限。
pub const MIN_GRID_ROWS: usize = 1;
/// マス総数。非表示行分も含めて常にこのサイズの配列を確保する
/// （－ボタンは表示行数を減らすだけで、裏のマス内容は破棄しない）。
pub const SLOT_COUNT: usize = GRID_COLS * MAX_GRID_ROWS;

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
    /// 現在表示している行数（MIN_GRID_ROWS..=MAX_GRID_ROWS）。ヘッダー左の行＋−ボタンで増減する。
    pub visible_rows: usize,
    /// true = 行＋−ボタンを無効化する（既存のドラッグ移動用ロックとは別の専用ロック）。
    pub row_edit_locked: bool,
    /// 各マスの内容。GRID_COLS×MAX_GRID_ROWS、行優先（index = row*GRID_COLS+col）で
    /// 非表示行分も含めて常時確保する。－ボタンは visible_rows を減らすだけで、
    /// 非表示になった行のマス内容はここに残り続け、＋ボタンで再度可視化されると復元される。
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

    /// ＋ボタンが押せるか（MAX_GRID_ROWS到達時、またはロック中はfalse）。
    pub fn can_add_row(&self) -> bool {
        !self.row_edit_locked && self.visible_rows < MAX_GRID_ROWS
    }

    /// −ボタンが押せるか（MIN_GRID_ROWS到達時、またはロック中はfalse）。
    pub fn can_remove_row(&self) -> bool {
        !self.row_edit_locked && self.visible_rows > MIN_GRID_ROWS
    }

    /// 行を1行増やす（最下段に追加、または非表示だった最下段を復元）。
    /// 上限到達・ロック中は何もせずfalseを返す。
    pub fn add_row(&mut self) -> bool {
        if !self.can_add_row() {
            return false;
        }
        self.visible_rows += 1;
        true
    }

    /// 行を1行減らす（最下段を非表示にするだけで、そのマス内容は破棄しない）。
    /// 下限到達・ロック中は何もせずfalseを返す。
    pub fn remove_row(&mut self) -> bool {
        if !self.can_remove_row() {
            return false;
        }
        self.visible_rows -= 1;
        true
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
            visible_rows: GRID_ROWS,
            row_edit_locked: false,
            slots: [PaletteSlotContent::Empty; SLOT_COUNT],
            custom_labels: [(); SLOT_COUNT].map(|_| None),
        }
    }
}

/// マス内容の既定の表示名（Toggle/Dialogの定義名、Actionのラベル）。空マスは None。
pub fn default_label(content: PaletteSlotContent, lang: crate::i18n::Lang) -> Option<&'static str> {
    match content {
        PaletteSlotContent::Toggle(kind) => Some((find_toggle_def(kind).label)(lang)),
        PaletteSlotContent::Dialog(kind) => Some(create_dialog(kind).title(lang)),
        PaletteSlotContent::Action(kind) => Some(kind.label(lang)),
        PaletteSlotContent::Empty => None,
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
            PaletteSlotContent::Toggle(ToggleKind::ImageInfo),
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
        assert_eq!(st.slots.len(), GRID_COLS * MAX_GRID_ROWS);
        assert_eq!(st.visible_rows, GRID_ROWS);
        assert!(!st.row_edit_locked);
        assert!(st.custom_labels.iter().all(|l| l.is_none()));
    }

    #[test]
    fn add_row_increments_until_max_then_refuses() {
        let mut st = PaletteState::default();
        st.visible_rows = MAX_GRID_ROWS - 1;
        assert!(st.can_add_row());
        assert!(st.add_row());
        assert_eq!(st.visible_rows, MAX_GRID_ROWS);
        assert!(!st.can_add_row());
        assert!(!st.add_row());
        assert_eq!(st.visible_rows, MAX_GRID_ROWS);
    }

    #[test]
    fn remove_row_decrements_until_min_then_refuses() {
        let mut st = PaletteState::default();
        st.visible_rows = MIN_GRID_ROWS + 1;
        assert!(st.can_remove_row());
        assert!(st.remove_row());
        assert_eq!(st.visible_rows, MIN_GRID_ROWS);
        assert!(!st.can_remove_row());
        assert!(!st.remove_row());
        assert_eq!(st.visible_rows, MIN_GRID_ROWS);
    }

    #[test]
    fn row_edit_locked_blocks_both_add_and_remove() {
        let mut st = PaletteState::default();
        st.row_edit_locked = true;
        assert!(!st.can_add_row());
        assert!(!st.can_remove_row());
        assert!(!st.add_row());
        assert!(!st.remove_row());
        assert_eq!(st.visible_rows, GRID_ROWS);
    }

    #[test]
    fn removed_row_slot_content_survives_and_is_restored_by_add_row() {
        let mut st = PaletteState::default();
        st.visible_rows = 3;
        // 3行目(row index 2)の先頭マス = idx 10
        st.slots[10] = PaletteSlotContent::Action(ActionKind::NextPage);
        assert!(st.remove_row());
        assert_eq!(st.visible_rows, 2);
        // 非表示になっただけで内容は破棄されない
        assert_eq!(st.slots[10], PaletteSlotContent::Action(ActionKind::NextPage));
        assert!(st.add_row());
        assert_eq!(st.visible_rows, 3);
        assert_eq!(st.slots[10], PaletteSlotContent::Action(ActionKind::NextPage));
    }
}
