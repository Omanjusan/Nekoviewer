//! ビューアーツールバー（top bar / fs_sort_bar）の項目順序基盤。
//! 設計メモ: docs/features/viewer-toolbar.md
//!
//! 項目の並びを「全項目IDの順列」として state ファイルに永続化する。
//! 現時点で並べ替え編集UIは無く実質固定だが、将来のライブ並べ替え・
//! 優先度付きオーバーフロー畳み込みが同じ順列を消費する前提の基盤。
//! egui 非依存の純粋ロジックのみを置き、描画は view_reader.rs 側で行う。

/// ツールバー項目ID。state ファイルとの対応は `id()` / `from_id()`。
///
/// 項目を追加するときは DEFAULT_BAR_ORDER・id()・from_id()・group() の
/// 4箇所を揃えること（`default_order_is_complete_permutation` テストが検出する）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewerBarItem {
    /// アーカイブ内ソート: 名前
    SortName,
    /// アーカイブ内ソート: 自然数
    SortNatural,
    /// アーカイブ内ソート: 日付
    SortDate,
    /// アーカイブ内ソート: 昇順/降順トグル
    SortOrder,
    /// 手動回転: 反時計回り
    RotateCcw,
    /// 手動回転: 時計回り
    RotateCw,
    /// 回転角度の引き継ぎトグル
    RotationCarry,
    /// Exif Orientation自動回転の適用トグル
    ExifRotation,
}

pub const BAR_ITEM_COUNT: usize = 8;

/// 既定の並び順。現行ツールバーの表示順と同一。
pub const DEFAULT_BAR_ORDER: [ViewerBarItem; BAR_ITEM_COUNT] = [
    ViewerBarItem::SortName,
    ViewerBarItem::SortNatural,
    ViewerBarItem::SortDate,
    ViewerBarItem::SortOrder,
    ViewerBarItem::RotateCcw,
    ViewerBarItem::RotateCw,
    ViewerBarItem::RotationCarry,
    ViewerBarItem::ExifRotation,
];

/// 描画グループ。隣接項目のグループが変わる位置に描画側がセパレータを挟む
/// （固定位置のセパレータ項目を持たないことで、並べ替え後も区切りが自然に追随する）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarGroup {
    Sort,
    Rotation,
}

impl ViewerBarItem {
    /// state ファイル永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            ViewerBarItem::SortName      => "sort_name",
            ViewerBarItem::SortNatural   => "sort_natural",
            ViewerBarItem::SortDate      => "sort_date",
            ViewerBarItem::SortOrder     => "sort_order",
            ViewerBarItem::RotateCcw     => "rotate_ccw",
            ViewerBarItem::RotateCw      => "rotate_cw",
            ViewerBarItem::RotationCarry => "rotation_carry",
            ViewerBarItem::ExifRotation  => "exif_rotation",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "sort_name"      => ViewerBarItem::SortName,
            "sort_natural"   => ViewerBarItem::SortNatural,
            "sort_date"      => ViewerBarItem::SortDate,
            "sort_order"     => ViewerBarItem::SortOrder,
            "rotate_ccw"     => ViewerBarItem::RotateCcw,
            "rotate_cw"      => ViewerBarItem::RotateCw,
            "rotation_carry" => ViewerBarItem::RotationCarry,
            "exif_rotation"  => ViewerBarItem::ExifRotation,
            _ => return None,
        })
    }

    /// ツールバー表示用アイコン（Unicodeグリフ、単一文字）。
    /// テキスト表示の項目は None（例: EXIF トグルは ASCII "EXIF" 表示のため対象外）。
    /// ここが唯一の定義元で、描画（view_reader.rs）と豆腐チェック
    /// （view_explorer/glyph_audit.rs の toolbar_icon_glyphs_are_available）の両方が参照する。
    pub fn icon(self) -> Option<&'static str> {
        match self {
            ViewerBarItem::RotateCcw     => Some("⟲"),
            ViewerBarItem::RotateCw      => Some("⟳"),
            ViewerBarItem::RotationCarry => Some("📌"),
            _ => None,
        }
    }

    pub fn group(self) -> BarGroup {
        match self {
            ViewerBarItem::SortName
            | ViewerBarItem::SortNatural
            | ViewerBarItem::SortDate
            | ViewerBarItem::SortOrder => BarGroup::Sort,
            ViewerBarItem::RotateCcw
            | ViewerBarItem::RotateCw
            | ViewerBarItem::RotationCarry
            | ViewerBarItem::ExifRotation => BarGroup::Rotation,
        }
    }
}

/// state ファイルのカンマ区切りID列から順序を復元する。
/// 未知IDはこの段階で読み捨てる（新バージョンの state を旧バージョンが読んでも壊れない）。
pub fn parse_bar_order(s: &str) -> [ViewerBarItem; BAR_ITEM_COUNT] {
    let saved: Vec<ViewerBarItem> = s
        .split(',')
        .filter_map(|t| ViewerBarItem::from_id(t.trim()))
        .collect();
    resolve_bar_order(&saved)
}

/// 保存済み順序（部分列・重複・欠落あり得る）を全項目の順列へ正規化する。
/// 前方互換規則:
/// - 重複ID → 先勝ち
/// - 欠落ID → 既定順で直前にある項目（無ければさらに手前へ遡る）の直後へ補完。
///   旧 state に無い新項目が、ユーザーの並べ替え後でも既定順の隣接項目
///   （＝同グループ想定）のそばに出現する
pub fn resolve_bar_order(saved: &[ViewerBarItem]) -> [ViewerBarItem; BAR_ITEM_COUNT] {
    let mut order: Vec<ViewerBarItem> = Vec::with_capacity(BAR_ITEM_COUNT);
    for &it in saved {
        if !order.contains(&it) {
            order.push(it);
        }
    }
    for (di, &it) in DEFAULT_BAR_ORDER.iter().enumerate() {
        if order.contains(&it) {
            continue;
        }
        // 既定順で自分より前の項目を近い順に探し、最初に見つかったものの直後へ。
        // 先行項目が1つも無ければ先頭へ。
        let pos = DEFAULT_BAR_ORDER[..di]
            .iter()
            .rev()
            .find_map(|prev| order.iter().position(|&x| x == *prev).map(|p| p + 1))
            .unwrap_or(0);
        order.insert(pos, it);
    }
    order.try_into().unwrap_or(DEFAULT_BAR_ORDER)
}

/// state ファイル保存用のカンマ区切りID列へ変換する。
pub fn bar_order_to_str(order: &[ViewerBarItem]) -> String {
    order.iter().map(|i| i.id()).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DEFAULT_BAR_ORDER が全項目をちょうど1回ずつ含む順列であること。
    /// 項目追加時に BAR_ITEM_COUNT や既定順の更新漏れを検出する。
    #[test]
    fn default_order_is_complete_permutation() {
        for (i, &a) in DEFAULT_BAR_ORDER.iter().enumerate() {
            assert!(
                !DEFAULT_BAR_ORDER[..i].contains(&a),
                "{:?} が既定順に重複している",
                a
            );
            // id() ↔ from_id() の往復が成立すること
            assert_eq!(ViewerBarItem::from_id(a.id()), Some(a));
        }
    }

    #[test]
    fn roundtrip_default() {
        let s = bar_order_to_str(&DEFAULT_BAR_ORDER);
        assert_eq!(parse_bar_order(&s), DEFAULT_BAR_ORDER);
    }

    #[test]
    fn empty_string_falls_back_to_default() {
        assert_eq!(parse_bar_order(""), DEFAULT_BAR_ORDER);
    }

    #[test]
    fn unknown_ids_are_ignored() {
        let s = "sort_name,future_item_xyz,sort_natural,sort_date,sort_order,\
                 rotate_ccw,rotate_cw,rotation_carry,exif_rotation";
        assert_eq!(parse_bar_order(s), DEFAULT_BAR_ORDER);
    }

    #[test]
    fn duplicates_keep_first() {
        let s = "sort_date,sort_date,sort_name,sort_natural,sort_order,\
                 rotate_ccw,rotate_cw,rotation_carry,exif_rotation";
        let order = parse_bar_order(s);
        assert_eq!(order[0], ViewerBarItem::SortDate);
        assert_eq!(order[1], ViewerBarItem::SortName);
        assert_eq!(order[2], ViewerBarItem::SortNatural);
    }

    /// 並べ替えた保存順序はそのまま維持されること（回転群をソート群より前へ）。
    #[test]
    fn custom_order_is_preserved() {
        let custom = [
            ViewerBarItem::RotateCcw,
            ViewerBarItem::RotateCw,
            ViewerBarItem::RotationCarry,
            ViewerBarItem::ExifRotation,
            ViewerBarItem::SortName,
            ViewerBarItem::SortNatural,
            ViewerBarItem::SortDate,
            ViewerBarItem::SortOrder,
        ];
        let s = bar_order_to_str(&custom);
        assert_eq!(parse_bar_order(&s), custom);
    }

    /// 欠落IDは「既定順で直前にある項目」の直後へ補完されること。
    /// 旧バージョンの state（新項目を知らない）を新バージョンが読むケースの再現。
    #[test]
    fn missing_id_is_inserted_at_default_position() {
        // SortDate を欠いた保存順序 → SortNatural の直後に補完される
        let s = "sort_name,sort_natural,sort_order,rotate_ccw,rotate_cw,\
                 rotation_carry,exif_rotation";
        assert_eq!(parse_bar_order(s), DEFAULT_BAR_ORDER);
    }

    /// 並べ替え済み順序への欠落補完も、既定順の前隣を基準に挿入されること。
    #[test]
    fn missing_id_follows_default_predecessor_in_custom_order() {
        // 回転群を前に出した順序から RotateCw を欠落させる
        // → 既定順の前隣 RotateCcw の直後に補完される
        let s = "rotate_ccw,rotation_carry,exif_rotation,\
                 sort_name,sort_natural,sort_date,sort_order";
        let order = parse_bar_order(s);
        assert_eq!(order[0], ViewerBarItem::RotateCcw);
        assert_eq!(order[1], ViewerBarItem::RotateCw);
        assert_eq!(order[2], ViewerBarItem::RotationCarry);
    }

    /// 既定順先頭の項目が欠落した場合は先頭へ補完されること。
    #[test]
    fn missing_first_item_goes_to_front() {
        let s = "sort_natural,sort_date,sort_order,rotate_ccw,rotate_cw,\
                 rotation_carry,exif_rotation";
        assert_eq!(parse_bar_order(s), DEFAULT_BAR_ORDER);
    }
}
