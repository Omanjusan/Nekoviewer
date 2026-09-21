//! エクスプローラー下段の評価フィルタ（UI表記は `score filter`。`== ★4.5` `<= ★3.0` `>= ★4.0`）。
//!
//! 値は半星単位の整数（0 = 未評価 / 1..=10 = ★0.5〜★5.0）。純粋な値型で egui・DB に依存しない。
//! 未評価（0）・レコード不在（一度も開いていない）は、どの条件にも一致させない
//! （評価済みだけを対象にする。将来の NEW 絞り込みは別条件として足す）。

/// 半星単位の最大値（★5.0）
const MAX_HALF: u8 = 10;

/// 比較条件
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RatingCmp {
    /// == 一致
    Eq,
    /// <= 以下
    Le,
    /// >= 以上
    Ge,
}

impl RatingCmp {
    /// コンボの並び順
    pub const ALL: [RatingCmp; 3] = [RatingCmp::Eq, RatingCmp::Le, RatingCmp::Ge];

    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Le => "<=",
            Self::Ge => ">=",
        }
    }
}

/// 評価フィルタの状態
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RatingFilter {
    /// 一括ON/OFF（文字列フィルタのチェックボックスと同じ仕組み）
    pub enabled: bool,
    pub cmp: RatingCmp,
    /// 基準の半星値（1..=10）
    pub threshold_half: u8,
}

impl Default for RatingFilter {
    fn default() -> Self {
        Self { enabled: false, cmp: RatingCmp::Eq, threshold_half: MAX_HALF }
    }
}

impl RatingFilter {
    /// この評価が条件に合うか。無効時は常に true（絞り込まない）。
    /// 有効時、未評価（0）・レコード不在（None）は常に false。
    pub fn matches(&self, rating_half: Option<u8>) -> bool {
        if !self.enabled {
            return true;
        }
        let Some(h) = rating_half.filter(|&h| h > 0) else {
            return false;
        };
        match self.cmp {
            RatingCmp::Eq => h == self.threshold_half,
            RatingCmp::Le => h <= self.threshold_half,
            RatingCmp::Ge => h >= self.threshold_half,
        }
    }
}

/// 基準コンボの選択肢（★0.5〜★5.0 の10項目）
pub const THRESHOLD_CHOICES: std::ops::RangeInclusive<u8> = 1..=MAX_HALF;

/// 基準コンボのラベル（例: 7 → "★3.5"、10 → "★5.0"）
pub fn star_label(half: u8) -> String {
    let half = half.min(MAX_HALF);
    format!("★{}.{}", half / 2, if half % 2 == 1 { 5 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(cmp: RatingCmp, threshold_half: u8) -> RatingFilter {
        RatingFilter { enabled: true, cmp, threshold_half }
    }

    #[test]
    fn disabled_filter_lets_everything_through_including_unrated_and_new() {
        let f = RatingFilter::default();
        assert!(!f.enabled);
        assert!(f.matches(None));
        assert!(f.matches(Some(0)));
        assert!(f.matches(Some(7)));
    }

    #[test]
    fn eq_matches_only_the_same_score() {
        let f = filter(RatingCmp::Eq, 7);
        assert!(f.matches(Some(7)));
        assert!(!f.matches(Some(6)));
        assert!(!f.matches(Some(8)));
    }

    #[test]
    fn le_matches_at_or_below_and_ge_matches_at_or_above() {
        let le = filter(RatingCmp::Le, 6);
        assert!(le.matches(Some(1)) && le.matches(Some(6)));
        assert!(!le.matches(Some(7)));

        let ge = filter(RatingCmp::Ge, 6);
        assert!(ge.matches(Some(6)) && ge.matches(Some(10)));
        assert!(!ge.matches(Some(5)));
    }

    #[test]
    fn unrated_and_never_opened_never_match_when_enabled() {
        // ≤ ★5 でも未評価(0)を「以下」に巻き込まない
        for cmp in RatingCmp::ALL {
            let f = filter(cmp, 10);
            assert!(!f.matches(None), "{cmp:?}: レコード不在");
            assert!(!f.matches(Some(0)), "{cmp:?}: 未評価");
        }
        assert!(!filter(RatingCmp::Le, 10).matches(Some(0)));
    }

    #[test]
    fn labels_cover_all_ten_half_steps() {
        assert_eq!(star_label(1), "★0.5");
        assert_eq!(star_label(2), "★1.0");
        assert_eq!(star_label(7), "★3.5");
        assert_eq!(star_label(10), "★5.0");
        assert_eq!(THRESHOLD_CHOICES.count(), 10);
        assert_eq!(RatingCmp::ALL.map(RatingCmp::label), ["==", "<=", ">="]);
    }
}
