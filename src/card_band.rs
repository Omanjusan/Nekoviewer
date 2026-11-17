//! サムネカード下部の「帯」をカスタム書式で組み立てるための、純粋なデータモデルとレイアウト計算。
//!
//! egui にも I/O にも依存しない。仕様は memory の project_card_band_custom_plan。
//! - 要素（`Elem`）と寄せ属性（`Align`）を並べた列（`Item`）が書式。改行で行に分かれる
//! - 保存形式は `title:l,space,date:r,nl,...`（要素ID[:寄せ]のカンマ区切り。未設定の寄せは省略）。
//!   未知の要素は読み捨て、未知の寄せは未設定にする（前方互換）
//! - 上限（要素24・行6）と、確定時の正規化（先頭・末尾の改行を削除、連続改行を1つに）
//! - 1行を3ゾーン（左／中央／右）へ配置する計画（`plan_row`）。明示指定を優先し、未設定は左ゾーンの
//!   明示左の後ろへ書式順で続く。幅が足りないときは 左 → 右 → 中央 の優先で溢れた側を切り詰める

/// 要素の合計上限。
pub(crate) const MAX_ITEMS: usize = 24;
/// 行数の上限（改行は最大 MAX_LINES-1 個）。
pub(crate) const MAX_LINES: usize = 6;

/// 帯に置ける要素。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Elem {
    Date,
    Title,
    Size,
    /// 評価点を星の描画で出す
    Stars,
    /// 評価点を数値（3.5）で出す
    Score,
    Visits,
    /// 半角1個ぶんの隙間
    Space,
    Newline,
    /// 行の高さいっぱいの縦線
    Divider,
}

impl Elem {
    /// 要素ボタンの並び順。
    pub(crate) const ALL: [Elem; 9] = [
        Elem::Date,
        Elem::Title,
        Elem::Size,
        Elem::Stars,
        Elem::Score,
        Elem::Visits,
        Elem::Space,
        Elem::Newline,
        Elem::Divider,
    ];

    /// 保存用ID。一度リリースしたIDは変更しない。
    pub(crate) fn id(self) -> &'static str {
        match self {
            Elem::Date => "date",
            Elem::Title => "title",
            Elem::Size => "size",
            Elem::Stars => "stars",
            Elem::Score => "score",
            Elem::Visits => "visits",
            Elem::Space => "space",
            Elem::Newline => "nl",
            Elem::Divider => "bar",
        }
    }

    pub(crate) fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "date" => Elem::Date,
            "title" => Elem::Title,
            "size" => Elem::Size,
            "stars" => Elem::Stars,
            "score" => Elem::Score,
            "visits" => Elem::Visits,
            "space" => Elem::Space,
            "nl" => Elem::Newline,
            "bar" => Elem::Divider,
            _ => return None,
        })
    }
}

/// 寄せ属性。トグル順は 未設定 → 左 → 中央 → 右 → 未設定。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Align {
    Unset,
    Left,
    Center,
    Right,
}

impl Align {
    pub(crate) fn next(self) -> Self {
        match self {
            Align::Unset => Align::Left,
            Align::Left => Align::Center,
            Align::Center => Align::Right,
            Align::Right => Align::Unset,
        }
    }

    /// 保存用の接尾辞。未設定は省略（None）。
    fn suffix(self) -> Option<&'static str> {
        match self {
            Align::Unset => None,
            Align::Left => Some("l"),
            Align::Center => Some("c"),
            Align::Right => Some("r"),
        }
    }

    /// 未知の接尾辞は未設定として読む。
    fn from_suffix(s: &str) -> Self {
        match s {
            "l" => Align::Left,
            "c" => Align::Center,
            "r" => Align::Right,
            _ => Align::Unset,
        }
    }
}

/// 書式の1項目。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Item {
    pub elem: Elem,
    pub align: Align,
}

impl Item {
    pub(crate) fn new(elem: Elem, align: Align) -> Self {
        Self { elem, align }
    }
}

/// どちらの導線を実採用するか。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Active {
    #[default]
    Classic,
    Custom,
}

impl Active {
    pub(crate) fn as_state_str(self) -> &'static str {
        match self {
            Active::Classic => "classic",
            Active::Custom => "custom",
        }
    }

    /// 未知値・空は Classic（従来の挙動）。
    pub(crate) fn from_state_str(s: &str) -> Self {
        match s {
            "custom" => Active::Custom,
            _ => Active::Classic,
        }
    }
}

/// 行数（要素が空なら0。改行1個につき+1）。
pub(crate) fn line_count(items: &[Item]) -> usize {
    if items.is_empty() {
        0
    } else {
        1 + items.iter().filter(|i| i.elem == Elem::Newline).count()
    }
}

/// 要素 `e` を末尾に足せるか（要素数・行数の上限）。
pub(crate) fn can_add(items: &[Item], e: Elem) -> bool {
    items.len() < MAX_ITEMS && !(e == Elem::Newline && line_count(items) >= MAX_LINES)
}

/// 確定時の正規化。先頭・末尾の改行を削除し、連続する改行を1つにする。
/// 上限（要素24・行6）を超える分は後ろから切り捨てる。
pub(crate) fn normalize(items: &[Item]) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len().min(MAX_ITEMS));
    for it in items {
        let prev_is_nl_or_start = out.last().is_none_or(|p| p.elem == Elem::Newline);
        if it.elem == Elem::Newline && prev_is_nl_or_start {
            continue;
        }
        if out.len() >= MAX_ITEMS || (it.elem == Elem::Newline && line_count(&out) >= MAX_LINES) {
            break;
        }
        out.push(*it);
    }
    while out.last().is_some_and(|p| p.elem == Elem::Newline) {
        out.pop();
    }
    out
}

/// 改行で行に分ける（改行そのものは含めない）。要素が空なら行なし。
pub(crate) fn split_rows(items: &[Item]) -> Vec<Vec<Item>> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<Vec<Item>> = vec![Vec::new()];
    for it in items {
        if it.elem == Elem::Newline {
            rows.push(Vec::new());
        } else if let Some(last) = rows.last_mut() {
            last.push(*it);
        }
    }
    rows
}

/// state 保存用の文字列へ。例: `title:l,space,date:r,nl,visits`
pub(crate) fn to_state_str(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| match i.align.suffix() {
            Some(a) => format!("{}:{}", i.elem.id(), a),
            None => i.elem.id().to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// state の文字列から復元する。未知の要素は読み捨て、未知の寄せは未設定。
/// 手編集・旧新バージョン間の持ち帰りで上限を超えていても、正規化して収める。
pub(crate) fn from_state_str(s: &str) -> Vec<Item> {
    let items: Vec<Item> = s
        .split(',')
        .filter_map(|tok| {
            let tok = tok.trim();
            let (id, al) = tok.split_once(':').unwrap_or((tok, ""));
            Elem::from_id(id.trim()).map(|e| Item::new(e, Align::from_suffix(al.trim())))
        })
        .collect();
    normalize(&items)
}

// ── 設定（保存対象） ─────────────────────────────────────────────────────

/// 帯の背景の透過度（%）の既定。従来の帯（黒・アルファ150/255）にほぼ等しい。
pub(crate) const DEFAULT_TRANSPARENCY_PCT: u8 = 40;

/// 透過度のトグル順: 0 → 10 → … → 100 → 0。
pub(crate) fn next_transparency(pct: u8) -> u8 {
    let p = normalize_transparency(pct);
    if p >= 100 { 0 } else { p + 10 }
}

/// 透過度を 0..=100 の10%刻みへ丸める（手編集・範囲外の保護）。
pub(crate) fn normalize_transparency(pct: u8) -> u8 {
    (((pct.min(100) as u16 + 5) / 10) * 10) as u8
}

/// 透過度(%) → 帯の黒背景のアルファ(0..=255)。0%=不透明、100%=完全に透明。
pub(crate) fn band_alpha(transparency_pct: u8) -> u8 {
    let p = normalize_transparency(transparency_pct) as u32;
    (((100 - p) * 255 + 50) / 100) as u8
}

/// state に保存する帯の設定。従来方式の選択（`card_info_mode` / `card_rating_mode`）は別キーのまま。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BandSettings {
    /// 実採用する導線
    pub active: Active,
    /// カスタム書式（正規化済み）。従来方式を採用していても保持する
    pub items: Vec<Item>,
    /// 帯の背景の透過度（%、10刻み。従来方式・カスタム共通）
    pub transparency_pct: u8,
}

impl Default for BandSettings {
    fn default() -> Self {
        Self { active: Active::Classic, items: Vec::new(), transparency_pct: DEFAULT_TRANSPARENCY_PCT }
    }
}

impl BandSettings {
    /// state の3キーから復元する。壊れた値・欠落は既定へ。
    pub(crate) fn from_state(active: &str, items: &str, transparency: Option<&str>) -> Self {
        Self {
            active: Active::from_state_str(active.trim()),
            items: from_state_str(items),
            transparency_pct: transparency
                .and_then(|s| s.trim().parse::<u8>().ok())
                .map_or(DEFAULT_TRANSPARENCY_PCT, normalize_transparency),
        }
    }

    /// 実カードに描くカスタム書式。カスタムが有効なときだけ Some（要素が空なら空スライス）。
    /// 空のときは「何も出さない」が正解なので、None（＝従来方式の帯を描く）にはしない。
    pub(crate) fn render_items(&self) -> Option<&[Item]> {
        (self.active == Active::Custom).then_some(self.items.as_slice())
    }
}

// ── 要素 → 表示内容 ──────────────────────────────────────────────────────

/// 1枚のカードぶんの表示データ（呼び出し側が i18n・書式を済ませて渡す）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BandData {
    pub title: String,
    /// 整形済みの日付。未取得なら空
    pub date: String,
    /// 整形済みのサイズ。未取得なら空
    pub size: String,
    /// 評価（半星単位 0..=10）。`None` = 生画像（評価の対象外）
    pub rating_half: Option<u8>,
    /// 「未評価」の表記（言語依存）
    pub unrated: String,
    /// 整形済みの訪問回数の行（「訪問回数：XX回」）
    pub visits_text: String,
}

/// 要素の描画内容。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Content {
    Text(String),
    /// 星の描画（半星値 1..=10）
    Stars(u8),
    /// 半角1個ぶんの隙間
    Space,
    /// 行の高さいっぱいの縦線
    Bar,
}

/// 要素を表示内容へ解決する。データ不在時の表示は現行踏襲:
/// 生画像は「-」、未評価は `unrated`、評価ありは星／小数1桁。日付・サイズが未取得なら空文字。
pub(crate) fn content_of(e: Elem, d: &BandData) -> Content {
    match e {
        Elem::Date => Content::Text(d.date.clone()),
        Elem::Title => Content::Text(d.title.clone()),
        Elem::Size => Content::Text(d.size.clone()),
        Elem::Stars => match d.rating_half {
            None => Content::Text("-".to_string()),
            Some(0) => Content::Text(d.unrated.clone()),
            Some(h) => Content::Stars(h.min(10)),
        },
        Elem::Score => match d.rating_half {
            None => Content::Text("-".to_string()),
            Some(0) => Content::Text(d.unrated.clone()),
            Some(h) => {
                let h = h.min(10);
                Content::Text(format!("{}.{}", h / 2, if h % 2 == 1 { 5 } else { 0 }))
            }
        },
        Elem::Visits => Content::Text(d.visits_text.clone()),
        Elem::Space => Content::Space,
        Elem::Divider => Content::Bar,
        // 改行は行分割で消費済みなので、ここへ来たら何も描かない
        Elem::Newline => Content::Space,
    }
}

/// 日付・サイズの取得が要る書式か（メタデータの遅延取得を省くため）。
pub(crate) fn needs_meta(items: &[Item]) -> bool {
    items.iter().any(|i| matches!(i.elem, Elem::Date | Elem::Size))
}

/// 評価・訪問回数の取得が要る書式か。
pub(crate) fn needs_rating(items: &[Item]) -> bool {
    items.iter().any(|i| matches!(i.elem, Elem::Stars | Elem::Score | Elem::Visits))
}

/// ゾーンの内容が描画範囲を超えた分（ホバー時のマーキー量）。超えていなければ 0。
pub(crate) fn zone_overflow(content_w: f32, clip: (f32, f32)) -> f32 {
    (content_w - (clip.1 - clip.0).max(0.0)).max(0.0)
}

// ── 1行の配置計画 ────────────────────────────────────────────────────────

/// 1ゾーンの配置計画。座標は行の左端を0とした相対値。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ZonePlan {
    /// このゾーンに置く項目の、行内インデックス（左から順に並べる順）
    pub idx: Vec<usize>,
    /// 先頭項目の左端x
    pub start: f32,
    /// 描画を許すx範囲（これを超える分は切り詰め）。幅0なら何も描かない
    pub clip: (f32, f32),
}

/// 1行の配置計画（左・中央・右）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowPlan {
    pub left: ZonePlan,
    pub center: ZonePlan,
    pub right: ZonePlan,
}

/// 1行を `avail` の幅へ配置する計画を立てる。
/// - `aligns[i]` / `widths[i]` は行内 i 番目の項目の寄せと幅
/// - 左ゾーン = 明示の左（書式順）の後ろに未設定（書式順）。中央・右はそれぞれ明示のみ
/// - 優先は 左 → 右 → 中央。溢れたゾーンは範囲で切り詰める（右は左の残りまで、中央は左右の間まで）
pub(crate) fn plan_row(aligns: &[Align], widths: &[f32], avail: f32) -> RowPlan {
    let n = aligns.len().min(widths.len());
    let pick = |a: Align| -> Vec<usize> { (0..n).filter(|&i| aligns[i] == a).collect() };
    let mut left = pick(Align::Left);
    left.extend(pick(Align::Unset));
    let center = pick(Align::Center);
    let right = pick(Align::Right);
    let sum = |idx: &[usize]| -> f32 { idx.iter().map(|&i| widths[i].max(0.0)).sum() };

    let avail = avail.max(0.0);
    let (lw, cw, rw) = (sum(&left), sum(&center), sum(&right));

    let lw_shown = lw.min(avail);
    let rw_shown = rw.min(avail - lw_shown);
    let right_start = avail - rw_shown;
    let c_l = lw_shown;
    let c_r = right_start;

    let center_start = if c_r > c_l {
        (avail / 2.0 - cw / 2.0).clamp(c_l, (c_r - cw).max(c_l))
    } else {
        c_l
    };

    RowPlan {
        left: ZonePlan { idx: left, start: 0.0, clip: (0.0, lw_shown) },
        center: ZonePlan { idx: center, start: center_start, clip: (c_l, c_r.max(c_l)) },
        right: ZonePlan { idx: right, start: right_start, clip: (right_start, avail) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(e: Elem) -> Item {
        Item::new(e, Align::Unset)
    }
    fn at(e: Elem, a: Align) -> Item {
        Item::new(e, a)
    }

    #[test]
    fn element_and_align_ids_round_trip() {
        for e in Elem::ALL {
            assert_eq!(Elem::from_id(e.id()), Some(e));
        }
        assert_eq!(Elem::from_id("nope"), None);
        for a in [Align::Left, Align::Center, Align::Right] {
            assert_eq!(Align::from_suffix(a.suffix().unwrap()), a);
        }
        assert_eq!(Align::Unset.suffix(), None);
        assert_eq!(Align::from_suffix("x"), Align::Unset);
    }

    #[test]
    fn align_cycles_through_unset_left_center_right() {
        let mut a = Align::Unset;
        let seq: Vec<Align> = (0..5).map(|_| { a = a.next(); a }).collect();
        assert_eq!(seq, vec![Align::Left, Align::Center, Align::Right, Align::Unset, Align::Left]);
    }

    #[test]
    fn active_state_str_round_trips_and_defaults_to_classic() {
        assert_eq!(Active::from_state_str("custom"), Active::Custom);
        assert_eq!(Active::from_state_str("classic"), Active::Classic);
        assert_eq!(Active::from_state_str(""), Active::Classic);
        assert_eq!(Active::from_state_str("???"), Active::Classic);
        assert_eq!(Active::from_state_str(Active::Custom.as_state_str()), Active::Custom);
    }

    #[test]
    fn state_string_round_trips() {
        let items = vec![
            at(Elem::Title, Align::Left), it(Elem::Space), at(Elem::Date, Align::Right),
            it(Elem::Newline), at(Elem::Stars, Align::Center), it(Elem::Divider), it(Elem::Visits),
        ];
        let s = to_state_str(&items);
        assert_eq!(s, "title:l,space,date:r,nl,stars:c,bar,visits");
        assert_eq!(from_state_str(&s), items);
        assert_eq!(from_state_str(""), vec![]);
    }

    #[test]
    fn parse_skips_unknown_elements_and_unknown_align() {
        let items = from_state_str("title:l, future_item:r ,date:zzz,,size");
        assert_eq!(items, vec![at(Elem::Title, Align::Left), it(Elem::Date), it(Elem::Size)]);
    }

    #[test]
    fn parse_normalizes_hand_edited_input_and_clamps_limits() {
        // 先頭・連続・末尾の改行は整える
        assert_eq!(from_state_str("nl,title,nl,nl,date,nl"), vec![it(Elem::Title), it(Elem::Newline), it(Elem::Date)]);
        // 要素数の上限
        let many = vec!["space"; MAX_ITEMS + 10].join(",");
        assert_eq!(from_state_str(&many).len(), MAX_ITEMS);
        // 行数の上限
        let rows = vec!["title"; MAX_LINES + 4].join(",nl,");
        assert_eq!(line_count(&from_state_str(&rows)), MAX_LINES);
    }

    #[test]
    fn normalize_trims_and_collapses_newlines() {
        let items = [
            it(Elem::Newline), it(Elem::Title), it(Elem::Newline), it(Elem::Newline),
            it(Elem::Date), it(Elem::Newline),
        ];
        assert_eq!(normalize(&items), vec![it(Elem::Title), it(Elem::Newline), it(Elem::Date)]);
        assert_eq!(normalize(&[it(Elem::Newline), it(Elem::Newline)]), vec![]);
    }

    #[test]
    fn limits_block_adding() {
        let mut v: Vec<Item> = Vec::new();
        for _ in 0..MAX_LINES - 1 {
            v.push(it(Elem::Title));
            v.push(it(Elem::Newline));
        }
        v.push(it(Elem::Title));
        assert_eq!(line_count(&v), MAX_LINES);
        assert!(!can_add(&v, Elem::Newline), "行上限で改行は追加できない");
        assert!(can_add(&v, Elem::Title));
        while v.len() < MAX_ITEMS {
            v.push(it(Elem::Space));
        }
        assert!(!can_add(&v, Elem::Title), "要素上限で何も追加できない");
    }

    #[test]
    fn split_rows_by_newline() {
        assert_eq!(split_rows(&[]), Vec::<Vec<Item>>::new());
        let rows = split_rows(&[it(Elem::Title), it(Elem::Space), it(Elem::Newline), it(Elem::Date)]);
        assert_eq!(rows, vec![vec![it(Elem::Title), it(Elem::Space)], vec![it(Elem::Date)]]);
    }

    // ── plan_row ──

    #[test]
    fn explicit_zones_are_placed_left_center_right() {
        // 幅100。左10・中央20・右30
        let p = plan_row(&[Align::Left, Align::Center, Align::Right], &[10.0, 20.0, 30.0], 100.0);
        assert_eq!((p.left.idx.clone(), p.left.start), (vec![0], 0.0));
        assert_eq!((p.right.idx.clone(), p.right.start), (vec![2], 70.0));
        assert_eq!((p.center.idx.clone(), p.center.start), (vec![1], 40.0), "中央は行の中央");
    }

    #[test]
    fn unset_follows_explicit_left_in_format_order() {
        // 書式順: [日付(未設定), タイトル(左), サイズ(未設定)] → 左ゾーン = タイトル, 日付, サイズ
        let p = plan_row(&[Align::Unset, Align::Left, Align::Unset], &[10.0, 20.0, 5.0], 100.0);
        assert_eq!(p.left.idx, vec![1, 0, 2]);
        assert!(p.center.idx.is_empty() && p.right.idx.is_empty());
    }

    #[test]
    fn overflow_priority_is_left_then_right_then_center() {
        // 幅100: 左70・右50・中央40 → 左は70そのまま、右は残り30に切り詰め、中央は居場所なし
        let p = plan_row(&[Align::Left, Align::Right, Align::Center], &[70.0, 50.0, 40.0], 100.0);
        assert_eq!(p.left.clip, (0.0, 70.0));
        assert_eq!(p.right.clip, (70.0, 100.0));
        assert_eq!(p.right.start, 70.0);
        assert!(p.center.clip.1 - p.center.clip.0 <= 0.0, "中央に使える幅は無い");
    }

    #[test]
    fn left_wider_than_row_is_clipped_and_pushes_out_the_others() {
        let p = plan_row(&[Align::Left, Align::Right], &[150.0, 20.0], 100.0);
        assert_eq!(p.left.clip, (0.0, 100.0));
        assert_eq!(p.right.clip, (100.0, 100.0), "右は全部押し出される");
    }

    #[test]
    fn center_is_nudged_to_avoid_the_left_zone() {
        // 幅100: 左60・中央40。行中央に置くと30..70 だが左と重なるので60..100へ寄せる
        let p = plan_row(&[Align::Left, Align::Center], &[60.0, 40.0], 100.0);
        assert_eq!(p.center.start, 60.0);
        assert_eq!(p.center.clip, (60.0, 100.0));
    }

    #[test]
    fn empty_row_has_empty_zones() {
        let p = plan_row(&[], &[], 80.0);
        assert!(p.left.idx.is_empty() && p.center.idx.is_empty() && p.right.idx.is_empty());
    }

    // ── content_of ──

    fn data(rating: Option<u8>) -> BandData {
        BandData {
            title: "a.zip".into(),
            date: "2026-01-31".into(),
            size: "1 MB".into(),
            rating_half: rating,
            unrated: "未評価".into(),
            visits_text: "訪問回数：3回".into(),
        }
    }

    #[test]
    fn content_resolves_text_elements() {
        let d = data(Some(7));
        assert_eq!(content_of(Elem::Title, &d), Content::Text("a.zip".into()));
        assert_eq!(content_of(Elem::Date, &d), Content::Text("2026-01-31".into()));
        assert_eq!(content_of(Elem::Size, &d), Content::Text("1 MB".into()));
        assert_eq!(content_of(Elem::Visits, &d), Content::Text("訪問回数：3回".into()));
        assert_eq!(content_of(Elem::Space, &d), Content::Space);
        assert_eq!(content_of(Elem::Divider, &d), Content::Bar);
    }

    #[test]
    fn rating_elements_follow_the_classic_display_rules() {
        // 評価あり: 星 / 小数1桁
        assert_eq!(content_of(Elem::Stars, &data(Some(7))), Content::Stars(7));
        assert_eq!(content_of(Elem::Score, &data(Some(7))), Content::Text("3.5".into()));
        assert_eq!(content_of(Elem::Score, &data(Some(10))), Content::Text("5.0".into()));
        assert_eq!(content_of(Elem::Score, &data(Some(1))), Content::Text("0.5".into()));
        // 未評価
        assert_eq!(content_of(Elem::Stars, &data(Some(0))), Content::Text("未評価".into()));
        assert_eq!(content_of(Elem::Score, &data(Some(0))), Content::Text("未評価".into()));
        // 生画像は「-」
        assert_eq!(content_of(Elem::Stars, &data(None)), Content::Text("-".into()));
        assert_eq!(content_of(Elem::Score, &data(None)), Content::Text("-".into()));
        // 範囲外は丸める
        assert_eq!(content_of(Elem::Stars, &data(Some(99))), Content::Stars(10));
    }

    #[test]
    fn meta_and_rating_needs_are_detected() {
        assert!(needs_meta(&[it(Elem::Title), it(Elem::Date)]));
        assert!(needs_meta(&[it(Elem::Size)]));
        assert!(!needs_meta(&[it(Elem::Title), it(Elem::Stars)]));
        assert!(needs_rating(&[it(Elem::Score)]));
        assert!(needs_rating(&[it(Elem::Visits)]));
        assert!(!needs_rating(&[it(Elem::Title), it(Elem::Date)]));
    }

    #[test]
    fn zone_overflow_is_the_excess_only() {
        assert_eq!(zone_overflow(120.0, (0.0, 100.0)), 20.0);
        assert_eq!(zone_overflow(80.0, (0.0, 100.0)), 0.0);
        assert_eq!(zone_overflow(10.0, (50.0, 50.0)), 10.0, "幅0のゾーンは全部がはみ出し");
    }

    // ── 設定 ──

    #[test]
    fn transparency_toggles_in_ten_percent_steps_and_wraps() {
        assert_eq!(next_transparency(0), 10);
        assert_eq!(next_transparency(40), 50);
        assert_eq!(next_transparency(90), 100);
        assert_eq!(next_transparency(100), 0, "100の次は0へ戻る");
        assert_eq!(next_transparency(43), 50, "10刻みでない値は丸めてから進む");
    }

    #[test]
    fn transparency_maps_to_alpha() {
        assert_eq!(band_alpha(0), 255, "透過度0%は不透明");
        assert_eq!(band_alpha(100), 0, "透過度100%は完全に透明");
        assert_eq!(band_alpha(40), 153);
        assert_eq!(band_alpha(250), 0, "範囲外は100%扱い");
        assert_eq!(normalize_transparency(250), 100);
    }

    #[test]
    fn settings_parse_defaults_and_round_trip() {
        let d = BandSettings::from_state("", "", None);
        assert_eq!(d, BandSettings::default());
        assert_eq!(d.render_items(), None, "従来方式が既定");
        let s = BandSettings::from_state("custom", "title:l,nl,date", Some("70"));
        assert_eq!(s.active, Active::Custom);
        assert_eq!(s.transparency_pct, 70);
        assert_eq!(s.render_items().map(|i| i.len()), Some(3));
        // 壊れた透過度は既定へ
        assert_eq!(BandSettings::from_state("custom", "title", Some("abc")).transparency_pct, DEFAULT_TRANSPARENCY_PCT);
        // カスタム有効で要素が空なら、空スライス（＝何も出さない。従来方式の帯にフォールバックしない）
        assert_eq!(BandSettings::from_state("custom", "", None).render_items(), Some(&[][..]));
        // 従来方式採用中は、カスタムの要素があっても None（従来方式の帯を描く）
        assert_eq!(BandSettings::from_state("classic", "title", None).render_items(), None);
    }
}
