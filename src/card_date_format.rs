//! サムネカード情報帯の「更新日時」表示に使う日付フォーマット定義。
//!
//! egui にも I/O にも依存しない純粋な値型。永続化は gui_config.rs が生文字列で
//! 持ち（キー分割）、設定ダイアログ UI は view_gui_config.rs が担当する。ここは
//!   生 state 文字列群 → CardDateFormat → 解決済み具体書式 → 文字列整形
//! の変換ロジックとそのテストだけを扱う。
//!
//! 設定ダイアログのコンボボックス構成（view_gui_config.rs 側で描画）:
//!   1. モード      : 自動 / ソート基準 / カスタム   … [CardDateMode]
//!   2. 自動時の書式: 例文字列4択（モード=自動のとき有効）… [AutoStyle]
//!   3〜6. カスタム : 順序 / 区切り / 年桁 / 月表記（モード=カスタムのとき有効）

use crate::i18n::Lang;

/// コンボボックスの選択肢を例示するときの基準日（2026年1月31日）。
/// 「31」は月になり得ないので DMY と MDY がラベルだけで区別できる。
const SAMPLE_YMD: (i64, i64, i64) = (2026, 1, 31);

/// 英語月名（3文字固定・i18n しない）。月表記=英名 のときだけ使う。
const EN_MONTH_ABBREV: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// 最上位モード。設定ダイアログ1個目のコンボボックスに対応。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum CardDateMode {
    /// 表示言語に応じた既定書式。`auto_style` が Some ならそちらで上書きする。
    #[default]
    Auto,
    /// ソート視認用の固定書式 `20260131`（YMD・区切りなし・4桁・数字）。子コンボは無視。
    Sort,
    /// 子コンボ (order/sep/year/month) で組み立てる。
    Custom,
}

impl CardDateMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sort => "sort",
            Self::Custom => "custom",
        }
    }

    /// 未知値・空文字は Auto へフォールバック。
    fn parse(s: &str) -> Self {
        match s {
            "sort" => Self::Sort,
            "custom" => Self::Custom,
            _ => Self::Auto,
        }
    }
}

/// モード=自動のときの具体スタイル。設定ダイアログ2個目のコンボに対応。
/// 表示ラベルは書式例そのもの（国名は出さない）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AutoStyle {
    /// `2026-01-31`（ISO 8601 / 東アジア / 北欧）
    YmdHyphen,
    /// `2026/01/31`（日本）
    YmdSlash,
    /// `31/01/2026`（英国 / 欧州大陸 / 英連邦 / 南米）
    DmySlash,
    /// `01/31/2026`（米国）
    MdySlash,
}

impl AutoStyle {
    /// 設定ダイアログのコンボ選択肢（表示順もこれに従う）。
    pub(crate) const ALL: [AutoStyle; 4] = [
        Self::YmdHyphen,
        Self::YmdSlash,
        Self::DmySlash,
        Self::MdySlash,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::YmdHyphen => "ymd_hyphen",
            Self::YmdSlash => "ymd_slash",
            Self::DmySlash => "dmy_slash",
            Self::MdySlash => "mdy_slash",
        }
    }

    /// 未知値・空文字は None（＝表示言語に追従）。
    fn parse(s: &str) -> Option<Self> {
        match s {
            "ymd_hyphen" => Some(Self::YmdHyphen),
            "ymd_slash" => Some(Self::YmdSlash),
            "dmy_slash" => Some(Self::DmySlash),
            "mdy_slash" => Some(Self::MdySlash),
            _ => None,
        }
    }

    /// 設定ダイアログのコンボ選択肢に出す書式例（基準日 2026-01-31）。国名は出さない。
    pub(crate) fn example(self) -> &'static str {
        match self {
            Self::YmdHyphen => "2026-01-31",
            Self::YmdSlash => "2026/01/31",
            Self::DmySlash => "31/01/2026",
            Self::MdySlash => "01/31/2026",
        }
    }

    fn to_parts(self) -> (DateOrder, DateSep) {
        match self {
            Self::YmdHyphen => (DateOrder::Ymd, DateSep::Hyphen),
            Self::YmdSlash => (DateOrder::Ymd, DateSep::Slash),
            Self::DmySlash => (DateOrder::Dmy, DateSep::Slash),
            Self::MdySlash => (DateOrder::Mdy, DateSep::Slash),
        }
    }
}

/// カスタム: 表示項目順。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DateOrder {
    /// 年→月→日
    #[default]
    Ymd,
    /// 日→月→年
    Dmy,
    /// 月→日→年
    Mdy,
}

impl DateOrder {
    pub(crate) const ALL: [DateOrder; 3] = [Self::Ymd, Self::Dmy, Self::Mdy];

    fn as_str(self) -> &'static str {
        match self {
            Self::Ymd => "ymd",
            Self::Dmy => "dmy",
            Self::Mdy => "mdy",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "dmy" => Self::Dmy,
            "mdy" => Self::Mdy,
            _ => Self::Ymd,
        }
    }
}

/// カスタム: 区切り文字。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DateSep {
    /// `/`
    #[default]
    Slash,
    /// `-`
    Hyphen,
    /// `.`
    Dot,
    /// 区切りなし
    None,
}

impl DateSep {
    pub(crate) const ALL: [DateSep; 4] = [Self::Slash, Self::Hyphen, Self::Dot, Self::None];

    fn as_str(self) -> &'static str {
        match self {
            Self::Slash => "slash",
            Self::Hyphen => "hyphen",
            Self::Dot => "dot",
            Self::None => "none",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "hyphen" => Self::Hyphen,
            "dot" => Self::Dot,
            "none" => Self::None,
            _ => Self::Slash,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Slash => "/",
            Self::Hyphen => "-",
            Self::Dot => ".",
            Self::None => "",
        }
    }
}

/// カスタム: 年の桁数。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum YearDigits {
    /// 4桁 `2026`
    #[default]
    Four,
    /// 2桁 `26`
    Two,
}

impl YearDigits {
    pub(crate) const ALL: [YearDigits; 2] = [Self::Four, Self::Two];

    fn as_str(self) -> &'static str {
        match self {
            Self::Four => "y4",
            Self::Two => "y2",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "y2" => Self::Two,
            _ => Self::Four,
        }
    }
}

/// カスタム: 月の表記。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum MonthStyle {
    /// 数字 `01`
    #[default]
    Numeric,
    /// 英名 `Jan`（3文字固定・i18n しない）
    EnglishAbbrev,
}

impl MonthStyle {
    pub(crate) const ALL: [MonthStyle; 2] = [Self::Numeric, Self::EnglishAbbrev];

    fn as_str(self) -> &'static str {
        match self {
            Self::Numeric => "num",
            Self::EnglishAbbrev => "en",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "en" => Self::EnglishAbbrev,
            _ => Self::Numeric,
        }
    }
}

/// mode / auto_style / 表示言語 をすべて解決した後の具体書式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Resolved {
    order: DateOrder,
    sep: DateSep,
    year: YearDigits,
    month: MonthStyle,
}

/// カード情報帯の日付書式設定。設定ダイアログの下書き・実体・永続化で共有する値型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CardDateFormat {
    pub mode: CardDateMode,
    /// None = 表示言語に追従（ユーザーが自動時の書式を明示していない）。
    pub auto_style: Option<AutoStyle>,
    pub order: DateOrder,
    pub sep: DateSep,
    pub year: YearDigits,
    pub month: MonthStyle,
}

impl Default for CardDateFormat {
    fn default() -> Self {
        Self {
            mode: CardDateMode::Auto,
            auto_style: None,
            order: DateOrder::Ymd,
            sep: DateSep::Slash,
            year: YearDigits::Four,
            month: MonthStyle::Numeric,
        }
    }
}

impl CardDateFormat {
    /// 生 state 文字列群から復元する。各フィールドは空文字・未知値なら既定へフォールバック
    /// するため、キーが1個欠けても・値が壊れていても全体は既定へ寄って壊れない。
    pub(crate) fn from_state(
        mode: &str,
        auto_style: &str,
        order: &str,
        sep: &str,
        year: &str,
        month: &str,
    ) -> Self {
        Self {
            mode: CardDateMode::parse(mode),
            auto_style: AutoStyle::parse(auto_style),
            order: DateOrder::parse(order),
            sep: DateSep::parse(sep),
            year: YearDigits::parse(year),
            month: MonthStyle::parse(month),
        }
    }

    // ── nekoviewer.state への書き出し用（キー分割）─────────────────────────
    pub(crate) fn mode_str(&self) -> &'static str {
        self.mode.as_str()
    }
    /// auto_style 未指定は空文字を返す（state に空で書く＝言語追従の意味）。
    pub(crate) fn auto_style_str(&self) -> &'static str {
        self.auto_style.map_or("", AutoStyle::as_str)
    }
    pub(crate) fn order_str(&self) -> &'static str {
        self.order.as_str()
    }
    pub(crate) fn sep_str(&self) -> &'static str {
        self.sep.as_str()
    }
    pub(crate) fn year_str(&self) -> &'static str {
        self.year.as_str()
    }
    pub(crate) fn month_str(&self) -> &'static str {
        self.month.as_str()
    }

    /// モード=自動で auto_style 未指定のとき、表示言語から選ぶ既定スタイル。
    /// ja→`2026/01/31` / zh・en→`2026-01-31`（en は曖昧さ回避で ISO）。
    pub(crate) fn lang_default_style(lang: Lang) -> AutoStyle {
        match lang {
            Lang::Japanese => AutoStyle::YmdSlash,
            Lang::English | Lang::Chinese => AutoStyle::YmdHyphen,
        }
    }

    fn resolve(&self, lang: Lang) -> Resolved {
        match self.mode {
            CardDateMode::Sort => Resolved {
                order: DateOrder::Ymd,
                sep: DateSep::None,
                year: YearDigits::Four,
                month: MonthStyle::Numeric,
            },
            CardDateMode::Custom => Resolved {
                order: self.order,
                sep: self.sep,
                year: self.year,
                month: self.month,
            },
            CardDateMode::Auto => {
                let (order, sep) = self
                    .auto_style
                    .unwrap_or_else(|| Self::lang_default_style(lang))
                    .to_parts();
                Resolved {
                    order,
                    sep,
                    year: YearDigits::Four,
                    month: MonthStyle::Numeric,
                }
            }
        }
    }

    /// civil-date 分解済みの (年, 月1..=12, 日1..=31) を現在の設定＋表示言語で文字列化する。
    /// 分解と範囲保証は呼び出し側（panels.rs の format_mtime）の責務。
    pub(crate) fn format_ymd(&self, year: i64, month: i64, day: i64, lang: Lang) -> String {
        let r = self.resolve(lang);
        let sep = r.sep.glyph();
        let year_s = match r.year {
            YearDigits::Four => format!("{year:04}"),
            YearDigits::Two => format!("{:02}", year.rem_euclid(100)),
        };
        let month_s = match r.month {
            MonthStyle::Numeric => format!("{month:02}"),
            MonthStyle::EnglishAbbrev => {
                let idx = month.clamp(1, 12) as usize - 1;
                EN_MONTH_ABBREV[idx].to_string()
            }
        };
        let day_s = format!("{day:02}");
        match r.order {
            DateOrder::Ymd => format!("{year_s}{sep}{month_s}{sep}{day_s}"),
            DateOrder::Dmy => format!("{day_s}{sep}{month_s}{sep}{year_s}"),
            DateOrder::Mdy => format!("{month_s}{sep}{day_s}{sep}{year_s}"),
        }
    }

    /// 設定ダイアログのライブプレビュー行やコンボ選択肢の例示に使う（基準日 2026-01-31）。
    pub(crate) fn preview(&self, lang: Lang) -> String {
        let (y, m, d) = SAMPLE_YMD;
        self.format_ymd(y, m, d, lang)
    }

    /// mode だけ Custom へ倒したコピー。カスタム系コンボの選択肢プレビューを
    /// 「カスタム軸の現在値＋候補1軸」で組み立てるために使う。
    pub(crate) fn as_custom(&self) -> Self {
        Self {
            mode: CardDateMode::Custom,
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JA: Lang = Lang::Japanese;
    const EN: Lang = Lang::English;
    const ZH: Lang = Lang::Chinese;

    fn custom(order: DateOrder, sep: DateSep, year: YearDigits, month: MonthStyle) -> CardDateFormat {
        CardDateFormat {
            mode: CardDateMode::Custom,
            auto_style: None,
            order,
            sep,
            year,
            month,
        }
    }

    #[test]
    fn empty_state_yields_default() {
        assert_eq!(
            CardDateFormat::from_state("", "", "", "", "", ""),
            CardDateFormat::default()
        );
    }

    #[test]
    fn unknown_values_fall_back_per_field() {
        let f = CardDateFormat::from_state("bogus", "bogus", "bogus", "bogus", "bogus", "bogus");
        assert_eq!(f, CardDateFormat::default());
    }

    #[test]
    fn state_round_trips_through_str_accessors() {
        let original = custom(DateOrder::Dmy, DateSep::Dot, YearDigits::Two, MonthStyle::EnglishAbbrev);
        let restored = CardDateFormat::from_state(
            original.mode_str(),
            original.auto_style_str(),
            original.order_str(),
            original.sep_str(),
            original.year_str(),
            original.month_str(),
        );
        assert_eq!(original, restored);
    }

    #[test]
    fn auto_style_none_serializes_empty_and_round_trips() {
        let f = CardDateFormat::default();
        assert_eq!(f.auto_style_str(), "");
        let restored = CardDateFormat::from_state(
            f.mode_str(),
            f.auto_style_str(),
            f.order_str(),
            f.sep_str(),
            f.year_str(),
            f.month_str(),
        );
        assert_eq!(restored.auto_style, None);
    }

    #[test]
    fn auto_style_some_round_trips() {
        let mut f = CardDateFormat::default();
        f.auto_style = Some(AutoStyle::MdySlash);
        assert_eq!(f.auto_style_str(), "mdy_slash");
        let restored = CardDateFormat::from_state(
            f.mode_str(), f.auto_style_str(), f.order_str(), f.sep_str(), f.year_str(), f.month_str(),
        );
        assert_eq!(restored.auto_style, Some(AutoStyle::MdySlash));
    }

    #[test]
    fn sort_mode_is_fixed_and_ignores_custom_fields() {
        let f = CardDateFormat {
            mode: CardDateMode::Sort,
            auto_style: Some(AutoStyle::MdySlash),
            order: DateOrder::Dmy,
            sep: DateSep::Dot,
            year: YearDigits::Two,
            month: MonthStyle::EnglishAbbrev,
        };
        assert_eq!(f.format_ymd(2026, 1, 31, JA), "20260131");
        assert_eq!(f.format_ymd(2026, 1, 31, EN), "20260131");
    }

    #[test]
    fn auto_follows_language_when_style_unset() {
        let f = CardDateFormat::default();
        assert_eq!(f.format_ymd(2026, 1, 31, JA), "2026/01/31");
        assert_eq!(f.format_ymd(2026, 1, 31, EN), "2026-01-31");
        assert_eq!(f.format_ymd(2026, 1, 31, ZH), "2026-01-31");
    }

    #[test]
    fn auto_style_overrides_language() {
        let mut f = CardDateFormat::default();
        f.auto_style = Some(AutoStyle::DmySlash);
        assert_eq!(f.format_ymd(2026, 1, 31, JA), "31/01/2026");
        assert_eq!(f.format_ymd(2026, 1, 31, EN), "31/01/2026");
        f.auto_style = Some(AutoStyle::MdySlash);
        assert_eq!(f.format_ymd(2026, 1, 31, JA), "01/31/2026");
    }

    #[test]
    fn custom_numeric_combinations() {
        assert_eq!(
            custom(DateOrder::Dmy, DateSep::Dot, YearDigits::Two, MonthStyle::Numeric)
                .format_ymd(2026, 1, 31, EN),
            "31.01.26"
        );
        assert_eq!(
            custom(DateOrder::Ymd, DateSep::None, YearDigits::Four, MonthStyle::Numeric)
                .format_ymd(2026, 1, 31, EN),
            "20260131"
        );
        assert_eq!(
            custom(DateOrder::Mdy, DateSep::Slash, YearDigits::Four, MonthStyle::Numeric)
                .format_ymd(2026, 9, 5, EN),
            "09/05/2026"
        );
    }

    #[test]
    fn custom_english_month_is_allowed_in_any_combo() {
        assert_eq!(
            custom(DateOrder::Dmy, DateSep::Slash, YearDigits::Four, MonthStyle::EnglishAbbrev)
                .format_ymd(2026, 9, 5, JA),
            "05/Sep/2026"
        );
        assert_eq!(
            custom(DateOrder::Ymd, DateSep::None, YearDigits::Four, MonthStyle::EnglishAbbrev)
                .format_ymd(2026, 1, 31, EN),
            "2026Jan31"
        );
    }

    #[test]
    fn two_digit_year_pads_and_wraps() {
        assert_eq!(
            custom(DateOrder::Ymd, DateSep::Hyphen, YearDigits::Two, MonthStyle::Numeric)
                .format_ymd(2005, 3, 7, EN),
            "05-03-07"
        );
    }

    #[test]
    fn preview_uses_sample_date() {
        assert_eq!(CardDateFormat::default().preview(JA), "2026/01/31");
    }
}
