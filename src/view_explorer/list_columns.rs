//! リストの列幅（幅を変えられる7列。サムネ・エラー・ファイラー・詳細・属性は固定幅なので持たない）。ユーザーが見出しの境界をドラッグして変え、全モード・全タブ共通で保存する。
//!
//! 列の並びは 重複／名前／サイズ／更新日時／評価／訪問回数／ページ数。サムネ列の幅は行の高さ（S/M/L）で決まるので持たない。
//! 保存は `nekoviewer.state` の `explorer_list_cols`（カンマ区切りの7つの整数。6つだけの旧形式は、ページ数だけ既定幅で読む。未知・欠落・壊れた値は既定幅）。

pub(crate) const COL_COUNT: usize = 7;
/// ページ数列を足す前の旧形式の個数。
const LEGACY_COL_COUNT: usize = 6;

/// 既定幅（px）。
pub(crate) const DEFAULT_WIDTHS: [f32; COL_COUNT] = [44.0, 360.0, 90.0, 160.0, 120.0, 80.0, 80.0];
/// 最小幅（px）。見出しの文字と、評価の★が収まる程度。重複列は、アイコン2つ（重複・似ている）が並ぶ幅。
pub(crate) const MIN_WIDTHS: [f32; COL_COUNT] = [40.0, 80.0, 50.0, 80.0, 60.0, 40.0, 50.0];
const MAX_WIDTH: f32 = 1200.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ListColumns {
    widths: [f32; COL_COUNT],
}

impl Default for ListColumns {
    fn default() -> Self {
        Self { widths: DEFAULT_WIDTHS }
    }
}

impl ListColumns {
    pub(crate) fn width(&self, idx: usize) -> f32 {
        self.widths[idx]
    }

    /// 幅を設定する（最小幅〜上限に丸める）。
    pub(crate) fn set(&mut self, idx: usize, width: f32) {
        self.widths[idx] = width.clamp(MIN_WIDTHS[idx], MAX_WIDTH);
    }

    /// 既定幅へ戻す。
    pub(crate) fn reset(&mut self, idx: usize) {
        self.widths[idx] = DEFAULT_WIDTHS[idx];
    }

    pub(crate) fn as_state_str(&self) -> String {
        self.widths.iter().map(|w| format!("{}", w.round() as i32)).collect::<Vec<_>>().join(",")
    }

    /// 7つ（旧形式は6つ）の数値が揃わない・数値でない場合は全て既定幅。範囲外の値は丸める。
    pub(crate) fn from_state_str(s: &str) -> Self {
        let parsed: Vec<f32> = s.split(',').filter_map(|t| t.trim().parse::<f32>().ok()).collect();
        let n = s.split(',').count();
        if (n != COL_COUNT && n != LEGACY_COL_COUNT) || parsed.len() != n {
            return Self::default();
        }
        let mut out = Self::default();
        for (i, w) in parsed.into_iter().enumerate() {
            out.set(i, w);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_clamps_to_min_and_max() {
        let mut c = ListColumns::default();
        c.set(1, 5.0);
        assert_eq!(c.width(1), MIN_WIDTHS[1]);
        c.set(1, 99999.0);
        assert_eq!(c.width(1), MAX_WIDTH);
        c.set(1, 200.0);
        assert_eq!(c.width(1), 200.0);
    }

    #[test]
    fn reset_restores_only_that_column() {
        let mut c = ListColumns::default();
        c.set(1, 200.0);
        c.set(2, 150.0);
        c.reset(1);
        assert_eq!(c.width(1), DEFAULT_WIDTHS[1]);
        assert_eq!(c.width(2), 150.0);
    }

    #[test]
    fn state_string_round_trips() {
        let mut c = ListColumns::default();
        c.set(0, 30.0);
        c.set(3, 222.0);
        assert_eq!(ListColumns::from_state_str(&c.as_state_str()), c);
    }

    #[test]
    fn broken_state_falls_back_to_defaults() {
        let d = ListColumns::default();
        assert_eq!(ListColumns::from_state_str(""), d);
        assert_eq!(ListColumns::from_state_str("1,2,3"), d, "個数が足りない");
        assert_eq!(ListColumns::from_state_str("1,2,3,4,5,6,7,8"), d, "個数が多い");
        assert_eq!(ListColumns::from_state_str("a,b,c,d,e,f,g"), d, "数値でない");
        assert_eq!(ListColumns::from_state_str("24,360,90,160,120,80,x"), d, "一部だけ壊れていたら全て既定");
    }

    #[test]
    fn legacy_six_column_state_keeps_widths_and_defaults_the_pages_column() {
        let c = ListColumns::from_state_str("30,200,100,170,130,90");
        assert_eq!((c.width(0), c.width(1), c.width(5)), (40.0_f32.max(30.0), 200.0, 90.0), "最小幅に丸めた上で旧値を保つ");
        assert_eq!(c.width(6), DEFAULT_WIDTHS[6]);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let c = ListColumns::from_state_str("1,1,1,1,1,1,1");
        for i in 0..COL_COUNT {
            assert_eq!(c.width(i), MIN_WIDTHS[i]);
        }
    }
}
