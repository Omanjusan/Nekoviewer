//! エクスプローラーのファイルカードの並び替え比較（純粋ロジック。egui・DB・ファイルI/Oに依存しない）。
//!
//! ソート軸は独立した2セット。
//! - 第1セット（`ExplorerSortKey` ＋昇降）: 名前・日付・サイズ。常に有効
//! - 第2セット（`RatingSort`）: スコア・訪問回数＋昇降。OFF が既定
//!
//! 第2セットが OFF なら第1セットだけで並べる（従来どおり）。
//! ON のときは第2セットが主軸になり、同順位を第1セットで並べる。
//! 未評価・一度も開いていないもの（レコード不在）は、どちらも 0 として扱う。

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::types::ExplorerSortKey;

/// 第2セットの軸
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RatingSortKey {
    Score,
    Visits,
}

impl RatingSortKey {
    pub fn as_state_key(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Visits => "visits",
        }
    }

    /// 未知の値は None（＝OFF 扱い）
    pub fn from_state_key(s: &str) -> Option<Self> {
        match s {
            "score" => Some(Self::Score),
            "visits" => Some(Self::Visits),
            _ => None,
        }
    }
}

/// 第2セットの状態。`key` が None の間は OFF で、`ascending` は次に ON にしたときの向きとして残る。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RatingSort {
    pub key: Option<RatingSortKey>,
    pub ascending: bool,
}

impl Default for RatingSort {
    /// OFF・降順（高い順・多い順）
    fn default() -> Self {
        Self { key: None, ascending: false }
    }
}

impl RatingSort {
    /// 押し下げ中の軸をもう一度押すと OFF。別の軸を押すとそちらへ切り替わる（排他）。
    pub fn toggle(&mut self, key: RatingSortKey) {
        self.key = if self.key == Some(key) { None } else { Some(key) };
    }

    /// state ファイルの値から復元する。キーが未知・欠落なら OFF、向きが壊れていれば降順。
    pub fn from_state(key: Option<&str>, ascending: Option<bool>) -> Self {
        Self {
            key: key.and_then(RatingSortKey::from_state_key),
            ascending: ascending.unwrap_or(false),
        }
    }

    /// state ファイルへ書く `rating_sort_key` の値（OFF は "off"）
    pub fn state_key(&self) -> &'static str {
        self.key.map_or("off", RatingSortKey::as_state_key)
    }
}

/// 並び替え対象1件ぶんの値。使わない軸の値は 0 / None のままでよい（`ExplorerSort::needs_*` 参照）。
#[derive(Clone, Copy, Default)]
pub struct SortRow<'a> {
    pub name: &'a str,
    pub mtime: Option<SystemTime>,
    pub size: u64,
    /// 半星単位（0=未評価 / レコード不在）
    pub rating_half: u8,
    /// 開いた回数（0=未訪問 / レコード不在）
    pub visit_count: u32,
}

/// 2セット分のソート条件
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ExplorerSort {
    pub key: ExplorerSortKey,
    pub ascending: bool,
    pub rating: RatingSort,
}

impl ExplorerSort {
    /// 更新日時を集める必要があるか（不要な stat を避けるための判定）
    pub fn needs_mtime(&self) -> bool {
        self.key == ExplorerSortKey::Date
    }

    pub fn needs_size(&self) -> bool {
        self.key == ExplorerSortKey::Size
    }

    /// 評価・訪問回数を集める必要があるか
    pub fn needs_rating(&self) -> bool {
        self.rating.key.is_some()
    }

    pub fn compare(&self, a: &SortRow, b: &SortRow) -> Ordering {
        let rating = match self.rating.key {
            Some(RatingSortKey::Score) => directed(self.rating.ascending, a.rating_half.cmp(&b.rating_half)),
            Some(RatingSortKey::Visits) => directed(self.rating.ascending, a.visit_count.cmp(&b.visit_count)),
            None => Ordering::Equal,
        };
        rating.then_with(|| self.compare_main(a, b))
    }

    /// 第1セットの比較。日付は「不明 < 既知」（降順では不明が末尾）で、これまでの並びと同じ。
    fn compare_main(&self, a: &SortRow, b: &SortRow) -> Ordering {
        let ord = match self.key {
            ExplorerSortKey::Name => a.name.cmp(b.name),
            ExplorerSortKey::Date => a.mtime.cmp(&b.mtime),
            ExplorerSortKey::Size => a.size.cmp(&b.size),
        };
        directed(self.ascending, ord)
    }
}

fn directed(ascending: bool, ord: Ordering) -> Ordering {
    if ascending { ord } else { ord.reverse() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn row(name: &str, rating_half: u8, visit_count: u32) -> SortRow<'_> {
        SortRow { name, rating_half, visit_count, ..SortRow::default() }
    }

    fn sort(key: ExplorerSortKey, ascending: bool, rating_key: Option<RatingSortKey>, rating_ascending: bool) -> ExplorerSort {
        ExplorerSort { key, ascending, rating: RatingSort { key: rating_key, ascending: rating_ascending } }
    }

    fn order<'a>(sort: &ExplorerSort, rows: &[SortRow<'a>]) -> Vec<&'a str> {
        let mut v = rows.to_vec();
        v.sort_by(|a, b| sort.compare(a, b));
        v.into_iter().map(|r| r.name).collect()
    }

    fn sample() -> Vec<SortRow<'static>> {
        vec![
            row("d", 0, 0),  // 未評価・未訪問
            row("b", 10, 3), // ★5.0
            row("a", 10, 1),
            row("c", 4, 3),  // ★2.0
        ]
    }

    #[test]
    fn rating_off_orders_by_main_axis_only() {
        let rows = sample();
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, None, false), &rows), ["a", "b", "c", "d"]);
        assert_eq!(order(&sort(ExplorerSortKey::Name, false, None, false), &rows), ["d", "c", "b", "a"]);
    }

    #[test]
    fn main_date_puts_unknown_first_ascending_and_last_descending() {
        let at = |s| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        let rows = [
            SortRow { name: "x", mtime: None, ..SortRow::default() },
            SortRow { name: "a", mtime: at(30), ..SortRow::default() },
            SortRow { name: "b", mtime: at(10), ..SortRow::default() },
        ];
        assert_eq!(order(&sort(ExplorerSortKey::Date, true, None, false), &rows), ["x", "b", "a"]);
        assert_eq!(order(&sort(ExplorerSortKey::Date, false, None, false), &rows), ["a", "b", "x"]);
    }

    #[test]
    fn main_size_follows_direction() {
        let rows = [
            SortRow { name: "a", size: 30, ..SortRow::default() },
            SortRow { name: "b", size: 10, ..SortRow::default() },
            SortRow { name: "c", size: 20, ..SortRow::default() },
        ];
        assert_eq!(order(&sort(ExplorerSortKey::Size, true, None, false), &rows), ["b", "c", "a"]);
        assert_eq!(order(&sort(ExplorerSortKey::Size, false, None, false), &rows), ["a", "c", "b"]);
    }

    #[test]
    fn score_descending_makes_score_primary_and_main_axis_breaks_ties() {
        let rows = sample();
        // ★5.0 の a,b は名前昇順、次に ★2.0 の c、最後に未評価の d
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, Some(RatingSortKey::Score), false), &rows), ["a", "b", "c", "d"]);
        // 主軸の向きだけ変えると同点内だけが逆になる（スコア順は変わらない）
        assert_eq!(order(&sort(ExplorerSortKey::Name, false, Some(RatingSortKey::Score), false), &rows), ["b", "a", "c", "d"]);
    }

    #[test]
    fn score_ascending_puts_unrated_first() {
        let rows = sample();
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, Some(RatingSortKey::Score), true), &rows), ["d", "c", "a", "b"]);
    }

    #[test]
    fn visits_sort_treats_never_opened_as_zero() {
        let rows = sample();
        // 回数 3 の b,c → 1 の a → 0 の d。同点は名前昇順
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, Some(RatingSortKey::Visits), false), &rows), ["b", "c", "a", "d"]);
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, Some(RatingSortKey::Visits), true), &rows), ["d", "a", "b", "c"]);
    }

    #[test]
    fn tie_break_can_use_size_descending() {
        let rows = [
            SortRow { name: "a", rating_half: 6, size: 10, ..SortRow::default() },
            SortRow { name: "b", rating_half: 6, size: 30, ..SortRow::default() },
            SortRow { name: "c", rating_half: 8, size: 20, ..SortRow::default() },
        ];
        let s = sort(ExplorerSortKey::Size, false, Some(RatingSortKey::Score), false);
        assert_eq!(order(&s, &rows), ["c", "b", "a"]);
    }

    #[test]
    fn all_unrated_falls_through_to_main_axis() {
        let rows = [row("c", 0, 0), row("a", 0, 0), row("b", 0, 0)];
        assert_eq!(order(&sort(ExplorerSortKey::Name, true, Some(RatingSortKey::Score), false), &rows), ["a", "b", "c"]);
    }

    #[test]
    fn needs_flags_follow_the_active_axes() {
        let s = sort(ExplorerSortKey::Name, true, None, false);
        assert!(!s.needs_mtime() && !s.needs_size() && !s.needs_rating());
        let s = sort(ExplorerSortKey::Date, true, Some(RatingSortKey::Visits), false);
        assert!(s.needs_mtime() && !s.needs_size() && s.needs_rating());
        let s = sort(ExplorerSortKey::Size, true, None, false);
        assert!(!s.needs_mtime() && s.needs_size());
    }

    #[test]
    fn pressing_the_active_rating_key_turns_it_off_and_other_key_switches() {
        let mut r = RatingSort::default();
        assert_eq!(r.key, None);
        r.toggle(RatingSortKey::Score);
        assert_eq!(r.key, Some(RatingSortKey::Score));
        r.toggle(RatingSortKey::Visits);
        assert_eq!(r.key, Some(RatingSortKey::Visits));
        r.toggle(RatingSortKey::Visits);
        assert_eq!(r.key, None);
    }

    #[test]
    fn off_keeps_the_last_direction_for_the_next_activation() {
        let mut r = RatingSort { key: Some(RatingSortKey::Score), ascending: true };
        r.toggle(RatingSortKey::Score);
        assert_eq!(r.key, None);
        assert!(r.ascending);
    }

    #[test]
    fn default_is_off_and_descending() {
        let r = RatingSort::default();
        assert_eq!(r.key, None);
        assert!(!r.ascending);
    }

    #[test]
    fn state_round_trip_and_fallbacks() {
        for key in [Some(RatingSortKey::Score), Some(RatingSortKey::Visits), None] {
            for ascending in [true, false] {
                let r = RatingSort { key, ascending };
                assert_eq!(RatingSort::from_state(Some(r.state_key()), Some(r.ascending)), r);
            }
        }
        // 旧 state（キーなし）・未知値・壊れた向きは OFF・降順に戻る
        assert_eq!(RatingSort::from_state(None, None), RatingSort::default());
        assert_eq!(RatingSort::from_state(Some("bogus"), Some(true)), RatingSort { key: None, ascending: true });
        assert_eq!(RatingSort::from_state(Some("score"), None), RatingSort { key: Some(RatingSortKey::Score), ascending: false });
    }
}
