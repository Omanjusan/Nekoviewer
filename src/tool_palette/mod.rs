//! ビューアー内ツールパレット（オーバーレイ、カスタマイザブルなグリッド式ショートカット）。
//! 固定5x2グリッド。各マスは空欄／ワンアクション型（Toggle）／ダイアログ型（Dialog）の
//! いずれかを保持する。座標・LOCK・透過度・可視性・マス内容は nekoviewer_spread.redb へ
//! 永続化する（favorites.rs と同様の方式。load/save本体の実装はPhase5）。

use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

pub mod dialog;
pub mod toggle;

pub use dialog::{create_dialog, DialogKind, PaletteDialog};
pub use toggle::{execute_toggle, find_toggle_def, ToggleDef, ToggleKind, TOGGLE_DEFS};

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
/// マス内容（Phase2/3で追加するアイコン等）はビットマップの拡縮ではなく、
/// この値をそのつど描画関数へ渡して都度描き直す前提（拡縮によるボケ・ギザギザ回避）。
pub const SLOT_SIZE_STEPS_PX: [f32; 5] = [16.0, 24.0, 32.0, 48.0, 64.0];
/// 既定のマスサイズ段階index（48px = 現行サイズ）。
pub const SLOT_SIZE_DEFAULT_IDX: usize = 3;

/// 1マスの内容。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PaletteSlotContent {
    /// 未登録
    Empty,
    /// ワンアクション型（即時トグル実行）
    Toggle(ToggleKind),
    /// ダイアログ型（クリックでミニUI展開）
    Dialog(DialogKind),
}

impl Default for PaletteSlotContent {
    fn default() -> Self {
        PaletteSlotContent::Empty
    }
}

/// ツールパレット本体の状態。ViewerConfigとは別に管理する。
#[derive(Clone, Debug)]
pub struct PaletteState {
    /// パレット左上のスクリーン座標
    pub pos: (f32, f32),
    /// true = ドラッグ移動を禁止
    pub locked: bool,
    /// 背景の透過度(10〜100%)
    pub opacity_pct: u8,
    /// false = パレット全体を非表示（右クリックで復帰）
    pub visible: bool,
    /// マスサイズ段階（SLOT_SIZE_STEPS_PX のindex）。ヘッダーのサイズボタンで巡回。
    pub slot_size_idx: usize,
    /// 各マスの内容。GRID_COLS×GRID_ROWS、行優先（index = row*GRID_COLS+col）
    pub slots: [PaletteSlotContent; SLOT_COUNT],
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
            opacity_pct: OPACITY_CEILING_PCT,
            visible: true,
            slot_size_idx: SLOT_SIZE_DEFAULT_IDX,
            slots: [PaletteSlotContent::Empty; SLOT_COUNT],
        }
    }
}

// ── 永続化（nekoviewer_spread.redb） ────────────────────────────────
// スキーマ定義のみここで確定させる。load/save本体・呼び出し元への配線はPhase5で行う。

/// キー固定=0（単一レコード）。値=(x, y, locked, opacity_pct, visible, slot_size_idx)
pub const PALETTE_STATE_TABLE: TableDefinition<u8, (f32, f32, bool, u8, bool, u8)> =
    TableDefinition::new("tool_palette_state");

/// キー=スロットindex(0〜SLOT_COUNT-1)。値=内容ID文字列
/// （"empty" / "toggle:<ToggleKind::id()>" / "dialog:<DialogKind::id()>"）。
/// レコードが無いスロットは Empty 扱い（前方互換：知らないIDも読み捨ててEmpty扱い）。
pub const PALETTE_SLOTS_TABLE: TableDefinition<u8, &str> =
    TableDefinition::new("tool_palette_slots");

/// 既存の spread_state 用 DB（nekoviewer_spread.redb）にツールパレット用テーブルを
/// 追加する。テーブルが無ければ自動作成される（redb の性質上マイグレーション不要）。
pub fn init_palette_tables(db: &Arc<Mutex<Database>>) -> Option<()> {
    let db = db.lock().ok()?;
    let tx = db.begin_write().ok()?;
    tx.open_table(PALETTE_STATE_TABLE).ok()?;
    tx.open_table(PALETTE_SLOTS_TABLE).ok()?;
    tx.commit().ok()?;
    Some(())
}

/// スロット内容 → 永続化用ID文字列。
pub fn slot_content_to_id(content: PaletteSlotContent) -> String {
    match content {
        PaletteSlotContent::Empty => "empty".to_string(),
        PaletteSlotContent::Toggle(k) => format!("toggle:{}", k.id()),
        PaletteSlotContent::Dialog(k) => format!("dialog:{}", k.id()),
    }
}

/// 永続化用ID文字列 → スロット内容。未知IDやパース失敗は Empty 扱い（前方互換）。
pub fn slot_content_from_id(s: &str) -> PaletteSlotContent {
    if let Some(rest) = s.strip_prefix("toggle:") {
        if let Some(k) = ToggleKind::from_id(rest) {
            return PaletteSlotContent::Toggle(k);
        }
    } else if let Some(rest) = s.strip_prefix("dialog:") {
        if let Some(k) = DialogKind::from_id(rest) {
            return PaletteSlotContent::Dialog(k);
        }
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
    }
}
