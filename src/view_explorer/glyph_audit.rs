//! マーカー候補グリフの収録・塗りつぶし検証。
//!
//! egui はグリフを単色アルファマスクで描くため、マーカーの色は字形のインク部分に
//! しか乗らない。候補は「塗り（solid）グリフ」であることが必須条件になる。
//! このテストは実行環境のフォントチェーン（egui標準 → PrimaryCJK）を本体と同じ
//! 優先順で解決し、各候補について
//!   1. どのフォントが拾うか（豆腐にならないか）
//!   2. インク塗り率（バウンディングボックス内のカバレッジ比）
//! を機械判定する。Linux は NotoSansCJK、Windows は Meiryo 系が対象になるため、
//! 候補リストを変更したら両OSで `cargo test glyph -- --nocapture` を実行すること。

use ab_glyph::{Font, FontRef, PxScale};

use super::{FAVORITE_MARKER_CANDIDATES, FAVORITE_MARKER_MIGRATION};

/// 塗り判定のインク率しきい値。
/// 実測分布（塗り系はおおむね 0.45 以上、輪郭系は 0.30 以下）の谷間に置く。
const FILLED_THRESHOLD: f32 = 0.35;

/// ラスタライズサイズ。実表示より大きめに取り測定誤差を減らす。
const AUDIT_PX: f32 = 64.0;

/// 本体（main.rs setup_japanese_font）と同じ優先順のフォントチェーンを組む。
/// egui 標準フォント群 → PrimaryCJK（Win: Meiryo等 / Linux: NotoSansCJK）。
fn font_chain() -> Vec<(String, Vec<u8>, u32)> {
    let defs = egui::FontDefinitions::default();
    let order = defs.families[&egui::FontFamily::Proportional].clone();
    let mut chain: Vec<(String, Vec<u8>, u32)> = order
        .iter()
        .filter_map(|name| {
            let fd = defs.font_data.get(name)?;
            Some((name.clone(), fd.font.to_vec(), fd.index))
        })
        .collect();
    if let Some(jp) = crate::japanese_font_data() {
        chain.push(("PrimaryCJK".to_owned(), jp, 0));
    }
    chain
}

struct GlyphReport {
    font_name: Option<String>,
    ink_ratio: f32,
}

/// チェーン先頭から順にグリフを持つフォントを探し、インク塗り率を測る。
/// egui の解決順（先勝ち）を再現する。
fn audit_char(chain: &[(String, Vec<u8>, u32)], ch: char) -> GlyphReport {
    for (name, data, index) in chain {
        let Ok(font) = FontRef::try_from_slice_and_index(data, *index) else {
            continue;
        };
        let gid = font.glyph_id(ch);
        if gid.0 == 0 {
            continue; // notdef: このフォントは当該グリフを持たない
        }
        let glyph = gid.with_scale(PxScale::from(AUDIT_PX));
        let Some(outlined) = font.outline_glyph(glyph) else {
            continue; // アウトラインなし（空白等）
        };
        let bounds = outlined.px_bounds();
        let area = bounds.width() * bounds.height();
        if area <= 0.0 {
            continue;
        }
        let mut ink = 0.0f32;
        outlined.draw(|_, _, coverage| ink += coverage);
        return GlyphReport {
            font_name: Some(name.clone()),
            ink_ratio: ink / area,
        };
    }
    GlyphReport { font_name: None, ink_ratio: 0.0 }
}

/// 塗り率がしきい値未満でも許容する「線画」グリフ。
/// 音符やチェックマークは線が細く bbox 比のインク率は低いが、字形に空洞（囲まれた
/// 未塗り領域）がなくインク全面に色が乗るため、マーカーとしては成立する。
const STROKE_STYLE_ALLOWLIST: &[&str] = &["♪", "♫", "♬", "✔"];

/// 現行候補＋移行先の全グリフを監査し、判定表を出力する。
/// `cargo test glyph_audit_report -- --nocapture` で表を確認する。
#[test]
fn glyph_audit_report() {
    let chain = font_chain();
    assert!(!chain.is_empty(), "フォントチェーンが空（egui標準フォントの取得に失敗）");

    println!();
    println!("{:^4} | {:>8} | {:^18} | 塗り率 | 判定", "字", "コード", "提供フォント");
    println!("{}", "-".repeat(58));
    for s in FAVORITE_MARKER_CANDIDATES {
        let ch = s.chars().next().unwrap();
        let r = audit_char(&chain, ch);
        let (font, verdict) = match &r.font_name {
            None => ("(なし)".to_owned(), "✗ 豆腐"),
            Some(f) if r.ink_ratio >= FILLED_THRESHOLD => (f.clone(), "○ 塗り"),
            Some(f) if STROKE_STYLE_ALLOWLIST.contains(s) => (f.clone(), "○ 線画"),
            Some(f) => (f.clone(), "△ 空洞"),
        };
        println!(
            "{:^4} | U+{:04X} | {:<18} | {:>5.3}  | {}",
            s, ch as u32, font, r.ink_ratio, verdict
        );
    }
}

/// 全マーカー候補が「フォント収録済み かつ 塗り（または線画例外）」であることの回帰テスト。
/// 候補リストを編集した際は Windows / Linux 両方でこのテストを通すこと。
#[test]
fn all_marker_candidates_are_filled_glyphs() {
    let chain = font_chain();
    assert!(!chain.is_empty(), "フォントチェーンが空（egui標準フォントの取得に失敗）");

    for s in FAVORITE_MARKER_CANDIDATES {
        let ch = s.chars().next().unwrap();
        let r = audit_char(&chain, ch);
        assert!(
            r.font_name.is_some(),
            "{s} (U+{:04X}) はフォントチェーンに収録がなく豆腐になる",
            ch as u32
        );
        assert!(
            r.ink_ratio >= FILLED_THRESHOLD || STROKE_STYLE_ALLOWLIST.contains(s),
            "{s} (U+{:04X}) は塗り率 {:.3} < {FILLED_THRESHOLD} の空洞グリフ",
            ch as u32,
            r.ink_ratio
        );
    }
}

/// ビューアーツールバーの全アイコングリフがフォントチェーンに収録されていること。
/// マーカーと違い単色マスク表示のため塗り率は問わず、豆腐（未収録）チェックのみ。
/// 候補は toolbar.rs::ViewerBarItem::icon() が唯一の定義元（追加したら自動で対象になる）。
#[test]
fn toolbar_icon_glyphs_are_available() {
    let chain = font_chain();
    assert!(!chain.is_empty(), "フォントチェーンが空（egui標準フォントの取得に失敗）");

    for item in crate::toolbar::DEFAULT_BAR_ORDER {
        let Some(s) = item.icon() else { continue };
        let ch = s.chars().next().unwrap();
        let r = audit_char(&chain, ch);
        assert!(
            r.font_name.is_some(),
            "{:?} のアイコン {s} (U+{:04X}) はフォントチェーンに収録がなく豆腐になる",
            item,
            ch as u32
        );
    }
}

/// 移行対応表の妥当性: 移行元は候補リストから除外済み、移行先は候補リストに存在すること。
#[test]
fn migration_table_is_consistent() {
    for (from, to) in FAVORITE_MARKER_MIGRATION {
        assert!(
            !FAVORITE_MARKER_CANDIDATES.contains(from),
            "移行元 {from} が候補リストに残っている"
        );
        assert!(
            FAVORITE_MARKER_CANDIDATES.contains(to),
            "移行先 {to} が候補リストにない"
        );
    }
}
