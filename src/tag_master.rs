//! タグマスター（カテゴリ・要素の定義）の操作API。画面（egui）の状態には触れず、
//! `Vec<TagManagerCategoryUi>`（＋次のtier_id）を安全に操作する純粋なロジックを集める。
//!
//! 保存・読み込み・メイン判定・単一選択の補正は `tag_manager.rs` にある。ここは「名前で探す・無ければ作る・
//! 保護されたカテゴリを守る・ファイルのタグを安全に足す」操作を、画面の外から呼べる形で提供する。
//!
//! - 要素の同一性は `normalize_key`（全角半角・大文字小文字・空白記号の違いを吸収）で判定する
//! - メインカテゴリとシステムカテゴリは削除できない（`remove_category` が拒否する）
//! - システムカテゴリは種別キー（`SystemKind`）で識別する。名前を変えても追跡できる
//! - ファイルのタグ合成（`merge_file_tags`）は既存のタグを消さず、排他の規則を守って足す
//! - 要素名は `A|B|C`（縦棒区切り）で別名をまとめられる。先頭が主表記。照合・重複判定は、どれかの別名が
//!   一致すれば同じ要素とみなす。名前そのものに `|` を含めたいときは全角 `｜` に置換して登録する

// 一部の検索・集計の関数は、テストと今後の照合用の公開APIで、いまの画面からは呼ばれない。
#![allow(dead_code)]

use std::collections::HashSet;

use crate::view_explorer::{TagManagerCategoryUi, TagManagerTierUi};

/// システムが作るカテゴリの種別。JSONには `as_key` の文字列で保存する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemKind {
    /// アーカイブの拡張子（zip / cbz / 7z …）をタグにする。1ファイルにつき自動付与は1要素まで。
    ArchiveFormat,
}

impl SystemKind {
    pub(crate) fn as_key(self) -> &'static str {
        match self {
            Self::ArchiveFormat => "archive_format",
        }
    }

    /// 未知のキーは None（旧版・将来版のJSONは通常カテゴリとして読む）。
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        match key {
            "archive_format" => Some(Self::ArchiveFormat),
            _ => None,
        }
    }

    /// 起動時に用意する全種別。
    pub(crate) const ALL: [SystemKind; 1] = [SystemKind::ArchiveFormat];

    /// 初期カテゴリ名（現在の言語。作成後はユーザーが変更できる）。
    fn default_name(self) -> String {
        match self {
            Self::ArchiveFormat => crate::i18n::t().tag_system_archive_format_name().to_string(),
        }
    }

    /// 初期要素。
    fn initial_elements(self) -> Vec<String> {
        match self {
            Self::ArchiveFormat => {
                let mut v: Vec<String> = ARCHIVE_FORMAT_EXTENSIONS.iter().map(|s| (*s).to_string()).collect();
                v.push(crate::i18n::t().tag_format_folder_name().to_string());
                v
            }
        }
    }
}

/// アーカイブ形式カテゴリの初期要素（拡張子。小文字）。`tar.gz` のような複合拡張子を含む。
pub(crate) const ARCHIVE_FORMAT_EXTENSIONS: [&str; 10] =
    ["zip", "cbz", "7z", "cb7", "tar", "cbt", "tar.gz", "tgz", "tar.zst", "tzst"];

/// 要素名の同一性判定用キー。全角ASCII→半角、全角空白→空白、小文字化し、空白と記号を取り除く。
/// ひらがな・カタカナの違いは区別する（日本語の表記ゆれの吸収は過剰になりやすいため）。
pub(crate) fn normalize_key(name: &str) -> String {
    name.chars()
        .map(|c| match c as u32 {
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace() && !is_ignorable_symbol(*c))
        .collect()
}

/// 別名の区切り。要素名 `A|B|C` は、A・B・C を別名に持つ1要素。
pub(crate) const ALIAS_SEP: char = '|';

/// 要素名を別名に分ける。各別名は前後の空白を除き、空のものは捨てる。先頭が主表記。
/// 区切りの無い名前は、それ自身1つだけ（空白だけの名前は空）。
pub(crate) fn split_aliases(name: &str) -> Vec<&str> {
    name.split(ALIAS_SEP).map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// 別名を `|` でつないで要素名にする。空の別名は捨てる。
pub(crate) fn join_aliases<S: AsRef<str>>(aliases: &[S]) -> String {
    let parts: Vec<&str> = aliases.iter().map(|a| a.as_ref().trim()).filter(|s| !s.is_empty()).collect();
    parts.join(&ALIAS_SEP.to_string())
}

/// 表示用の主表記（先頭の別名）。別名が無い（空）なら元の名前を返す。
pub(crate) fn primary_alias(name: &str) -> &str {
    split_aliases(name).first().copied().unwrap_or(name)
}

/// 名前に含まれる `|` を全角 `｜` に置換する（別名の区切りと区別するため。候補を要素として登録する前に使う）。
pub(crate) fn escape_alias_sep(name: &str) -> String {
    name.replace(ALIAS_SEP, "｜")
}

/// 要素名が別名を持つか（`|` で2つ以上に分かれる）。
pub(crate) fn has_aliases(name: &str) -> bool {
    split_aliases(name).len() > 1
}

/// 要素名の、別名ごとの正規化キー。空になるものと重複は除く（順序は別名の順）。
pub(crate) fn alias_keys(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in split_aliases(name) {
        let k = normalize_key(a);
        if !k.is_empty() && !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// カテゴリ内で、`name` の別名のどれかと別名が重なる要素（tier_id, 要素の全体名）。
pub(crate) fn overlapping_elements(cats: &[TagManagerCategoryUi], cat_idx: usize, name: &str) -> Vec<(u64, String)> {
    let keys = alias_keys(name);
    let Some(c) = cats.get(cat_idx) else { return Vec::new() };
    if keys.is_empty() {
        return Vec::new();
    }
    c.tiers
        .iter()
        .filter_map(|t| t.element.as_deref().filter(|e| element_hits(e, &keys)).map(|e| (t.id, e.to_string())))
        .collect()
}

/// 要素名の別名のどれかが、`keys`（正規化キー）のどれかと一致するか。
fn element_hits(element: &str, keys: &[String]) -> bool {
    split_aliases(element).iter().any(|a| {
        let k = normalize_key(a);
        !k.is_empty() && keys.contains(&k)
    })
}

/// 同一性判定で無視する記号。名前の区切りや装飾に使われるもの。
fn is_ignorable_symbol(c: char) -> bool {
    matches!(
        c,
        '_' | '-' | '.' | ',' | '\'' | '"' | '`' | '!' | '?' | ':' | ';' | '/' | '\\' | '|' | '(' | ')' | '[' | ']'
            | '{' | '}' | '<' | '>' | '・' | '･' | '、' | '。' | '「' | '」' | '『' | '』' | '【' | '】' | '〈' | '〉'
            | '《' | '》' | '〔' | '〕' | '～' | '〜' | '—' | '–' | '…' | '＊' | '*' | '#' | '&' | '+' | '=' | '~'
    )
}

/// `remove_category` の拒否理由。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemoveError {
    OutOfRange,
    /// メインカテゴリは削除できない。
    Main,
    /// システムカテゴリは削除できない。
    System,
}

/// マスターの操作窓口。カテゴリ配列と次のtier_idを借りて操作する。
pub(crate) struct TagMaster<'a> {
    cats: &'a mut Vec<TagManagerCategoryUi>,
    next_tier_id: &'a mut u64,
}

impl<'a> TagMaster<'a> {
    pub(crate) fn new(cats: &'a mut Vec<TagManagerCategoryUi>, next_tier_id: &'a mut u64) -> Self {
        Self { cats, next_tier_id }
    }

    /// カテゴリ数。
    pub(crate) fn len(&self) -> usize {
        self.cats.len()
    }

    // ── 検索 ──────────────────────────────────────────────────────────

    /// 種別キーでシステムカテゴリを探す。
    pub(crate) fn find_system(&self, kind: SystemKind) -> Option<usize> {
        self.cats.iter().position(|c| c.system_kind == Some(kind))
    }

    /// カテゴリ名（正規化して比較）で探す。メインカテゴリは保存名ではなく現在の言語の表示名で比べる。
    pub(crate) fn find_category(&self, name: &str) -> Option<usize> {
        let key = normalize_key(name);
        self.cats.iter().position(|c| normalize_key(&c.display_name()) == key)
    }

    /// カテゴリ内の要素を名前（正規化して比較）で探して、tier_idを返す。
    /// 要素の別名のどれかが、探す名前の別名のどれかと一致すれば見つかったとする（先頭から見て最初の1つ）。
    pub(crate) fn find_element(&self, cat_idx: usize, name: &str) -> Option<u64> {
        self.find_elements(cat_idx, name).into_iter().next()
    }

    /// `find_element` の、一致する要素を全部返す版（別名が複数の既存要素に重なっていないかの確認用）。
    pub(crate) fn find_elements(&self, cat_idx: usize, name: &str) -> Vec<u64> {
        let keys = alias_keys(name);
        if keys.is_empty() {
            return Vec::new();
        }
        let Some(c) = self.cats.get(cat_idx) else { return Vec::new() };
        c.tiers.iter().filter(|t| t.element.as_deref().is_some_and(|e| element_hits(e, &keys))).map(|t| t.id).collect()
    }

    /// 全カテゴリから、要素を名前（正規化して比較）で探す（（カテゴリ番号, tier_id）の一覧）。
    /// 作者かグループかの判定など、既存の確定タグとの照合に使う。別名のどれかが一致すれば含める。
    pub(crate) fn find_element_anywhere(&self, name: &str) -> Vec<(usize, u64)> {
        let keys = alias_keys(name);
        if keys.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (ci, c) in self.cats.iter().enumerate() {
            for t in &c.tiers {
                if t.element.as_deref().is_some_and(|e| element_hits(e, &keys)) {
                    out.push((ci, t.id));
                }
            }
        }
        out
    }

    /// `cat_idx` の `tier_id` の要素名（全体）。
    pub(crate) fn element_name(&self, cat_idx: usize, tier_id: u64) -> Option<String> {
        self.cats.get(cat_idx)?.tiers.iter().find(|t| t.id == tier_id).and_then(|t| t.element.clone())
    }

    /// tier_id が属するカテゴリ番号。
    pub(crate) fn category_of(&self, tier_id: u64) -> Option<usize> {
        self.cats.iter().position(|c| c.tiers.iter().any(|t| t.id == tier_id && t.element.is_some()))
    }

    // ── 作成 ──────────────────────────────────────────────────────────

    /// カテゴリを末尾に追加して、その番号を返す。色は既存色と離れた色を選ぶ。
    pub(crate) fn add_category(&mut self, name: String, single_select: bool, system_kind: Option<SystemKind>) -> usize {
        let existing: Vec<egui::Color32> = self.cats.iter().map(|c| c.color).collect();
        self.cats.push(TagManagerCategoryUi {
            name,
            tiers: Vec::new(),
            color: crate::view_explorer::pick_distinct_tag_color(&existing),
            is_main: false,
            single_select,
            system_kind,
        });
        self.cats.len() - 1
    }

    fn next_id(&mut self) -> u64 {
        let id = *self.next_tier_id;
        *self.next_tier_id += 1;
        id
    }

    /// 要素を末尾に追加する（同名があっても追加する。画面の手動入力用）。空白だけの名前は None。
    pub(crate) fn push_element(&mut self, cat_idx: usize, name: &str) -> Option<u64> {
        let name = name.trim();
        if name.is_empty() || cat_idx >= self.cats.len() {
            return None;
        }
        let id = self.next_id();
        let tiers = &mut self.cats[cat_idx].tiers;
        let tier_no = tiers.len() as i32 + 1;
        tiers.push(TagManagerTierUi { id, tier_no, negative: false, element: Some(name.to_string()) });
        Some(id)
    }

    /// 要素を追加する。同じ正規化名の要素が既にあれば、それを返す（重複させない）。
    /// 戻り値は（tier_id, 新規に作ったか）。空白だけの名前は None。
    pub(crate) fn add_element_unique(&mut self, cat_idx: usize, name: &str) -> Option<(u64, bool)> {
        if split_aliases(name).is_empty() {
            return None; // 空白と `|` だけの名前
        }
        if let Some(id) = self.find_element(cat_idx, name) {
            return Some((id, false));
        }
        self.push_element(cat_idx, name).map(|id| (id, true))
    }

    /// 書き込まずに、`names` を `cat_idx` へ `add_element_unique` で追加したときの新規要素数を数える。
    /// `cat_idx` が None なら、まだ無いカテゴリ（全て新規）。`names` 内の同一視できる重複は1つと数える。
    /// 別名つきの名前は、別名のどれかが既存（または先に数えた名前）と一致すれば新規と数えない。
    pub(crate) fn count_new_elements(&self, cat_idx: Option<usize>, names: &[String]) -> usize {
        let mut have: HashSet<String> = match cat_idx.and_then(|i| self.cats.get(i)) {
            None => HashSet::new(),
            Some(c) => c.tiers.iter().filter_map(|t| t.element.as_deref()).flat_map(alias_keys).collect(),
        };
        let mut count = 0;
        for n in names {
            let keys = alias_keys(n);
            if keys.is_empty() || keys.iter().any(|k| have.contains(k)) {
                continue;
            }
            have.extend(keys);
            count += 1;
        }
        count
    }

    // ── 削除 ──────────────────────────────────────────────────────────

    /// カテゴリを削除する。メインとシステムは拒否する。
    pub(crate) fn remove_category(&mut self, idx: usize) -> Result<(), RemoveError> {
        let c = self.cats.get(idx).ok_or(RemoveError::OutOfRange)?;
        if c.is_main {
            return Err(RemoveError::Main);
        }
        if c.system_kind.is_some() {
            return Err(RemoveError::System);
        }
        self.cats.remove(idx);
        Ok(())
    }

    /// tier（要素）を削除し、tier番号を連番に振り直す。範囲外は false。
    pub(crate) fn remove_tier(&mut self, cat_idx: usize, tier_idx: usize) -> bool {
        let Some(c) = self.cats.get_mut(cat_idx) else { return false };
        if tier_idx >= c.tiers.len() {
            return false;
        }
        c.tiers.remove(tier_idx);
        for (i, t) in c.tiers.iter_mut().enumerate() {
            t.tier_no = i as i32 + 1;
        }
        true
    }

    // ── システムカテゴリ ──────────────────────────────────────────────

    /// 無いシステムカテゴリを、初期要素つきで作る。作ったら true（呼び出し側が保存する）。
    /// 既にあるカテゴリは、要素を全部消されていても補充しない（ユーザーの編集を尊重する）。
    pub(crate) fn ensure_system_categories(&mut self) -> bool {
        let mut changed = false;
        for kind in SystemKind::ALL {
            if self.find_system(kind).is_some() {
                continue;
            }
            // 同じ名前の通常カテゴリがあっても、別カテゴリとして作る（種別キーで識別するため）。
            let idx = self.add_category(kind.default_name(), false, Some(kind));
            for name in kind.initial_elements() {
                self.push_element(idx, &name);
            }
            changed = true;
        }
        changed
    }

    // ── ファイルのタグ合成 ────────────────────────────────────────────

    /// 自動付与の排他規則を持つカテゴリか（メイン・単一選択・アーカイブ形式）。
    pub(crate) fn auto_exclusive(c: &TagManagerCategoryUi) -> bool {
        c.is_main || c.single_select || c.system_kind == Some(SystemKind::ArchiveFormat)
    }

    /// 既存のタグID集合に、自動で足したいID集合を安全に合成して返す。
    /// - 既存のタグは消さない（順序も保つ）
    /// - 排他のカテゴリ（メイン・単一選択・アーカイブ形式）は、既存にそのカテゴリの要素が無いときだけ、
    ///   足したいIDの先頭1つを足す（メインは未設定のときだけ付く）
    /// - マスターに無いID・重複は足さない
    pub(crate) fn merge_file_tags(&self, existing: &[u64], additions: &[u64]) -> Vec<u64> {
        merge_file_tags_in(self.cats, existing, additions)
    }
}

/// `TagMaster::merge_file_tags` の、借用なしで呼べる版（別スレッドでカテゴリの写しを読むだけのときに使う）。
pub(crate) fn merge_file_tags_in(cats: &[TagManagerCategoryUi], existing: &[u64], additions: &[u64]) -> Vec<u64> {
    let category_of = |tier_id: u64| cats.iter().position(|c| c.tiers.iter().any(|t| t.id == tier_id && t.element.is_some()));
    let mut out: Vec<u64> = existing.to_vec();
    let mut have: HashSet<u64> = existing.iter().copied().collect();
    for &id in additions {
        if have.contains(&id) {
            continue;
        }
        let Some(ci) = category_of(id) else { continue };
        let cat = &cats[ci];
        if TagMaster::auto_exclusive(cat) {
            let taken = cat.tiers.iter().any(|t| t.element.is_some() && have.contains(&t.id));
            if taken {
                continue;
            }
        }
        out.push(id);
        have.insert(id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(id: u64, name: &str) -> TagManagerTierUi {
        TagManagerTierUi { id, tier_no: id as i32, negative: false, element: (!name.is_empty()).then(|| name.to_string()) }
    }

    fn cat(name: &str, is_main: bool, single: bool, kind: Option<SystemKind>, tiers: Vec<TagManagerTierUi>) -> TagManagerCategoryUi {
        TagManagerCategoryUi {
            name: name.to_string(),
            tiers,
            color: egui::Color32::GRAY,
            is_main,
            single_select: is_main || single,
            system_kind: kind,
        }
    }

    /// メイン(1,2) / 作者(10,11,12) / 単一選択の評価(20,21) / 形式(30,31)
    fn sample() -> (Vec<TagManagerCategoryUi>, u64) {
        (
            vec![
                cat("main", true, true, None, vec![tier(1, "漫画"), tier(2, "同人誌")]),
                cat("作者", false, false, None, vec![tier(10, "Kouji"), tier(11, "ＡＢＣ　Ｄ"), tier(12, "")]),
                cat("評価", false, true, None, vec![tier(20, "良"), tier(21, "悪")]),
                cat("形式", false, false, Some(SystemKind::ArchiveFormat), vec![tier(30, "zip"), tier(31, "cbz")]),
            ],
            100,
        )
    }

    #[test]
    fn normalize_unifies_width_case_space_and_symbols() {
        assert_eq!(normalize_key("ＡＢＣ　Ｄ"), normalize_key("abc d"));
        assert_eq!(normalize_key("Hawk Bit"), normalize_key("hawk-bit"));
        assert_eq!(normalize_key("Hawk Bit"), normalize_key("HawkBit"));
        assert_eq!(normalize_key("A・B"), normalize_key("ab"));
        assert_ne!(normalize_key("あ"), normalize_key("ア"), "ひらがなとカタカナは区別する");
        assert_eq!(normalize_key("  "), "");
        assert_eq!(normalize_key("【】"), "");
    }

    #[test]
    fn find_helpers_use_normalized_names() {
        let (mut cats, mut next) = sample();
        let m = TagMaster::new(&mut cats, &mut next);
        assert_eq!(m.find_system(SystemKind::ArchiveFormat), Some(3));
        assert_eq!(m.find_category("ＡＢＣ"), None);
        assert_eq!(m.find_category("作者"), Some(1));
        assert_eq!(m.find_element(1, "kouji"), Some(10));
        assert_eq!(m.find_element(1, "abc d"), Some(11));
        assert_eq!(m.find_element(1, "  "), None, "空の名前は一致しない");
        assert_eq!(m.find_element(9, "kouji"), None, "範囲外");
        assert_eq!(m.find_element_anywhere("ZIP"), vec![(3, 30)]);
        assert_eq!(m.category_of(21), Some(2));
        assert_eq!(m.category_of(12), None, "要素の無いtierは数えない");
    }

    #[test]
    fn add_element_unique_dedupes_and_push_allows_duplicates() {
        let (mut cats, mut next) = sample();
        let mut m = TagMaster::new(&mut cats, &mut next);
        assert_eq!(m.add_element_unique(1, "KOUJI"), Some((10, false)));
        let (id, created) = m.add_element_unique(1, " 新作者 ").unwrap();
        assert_eq!((id, created), (100, true));
        assert_eq!(m.add_element_unique(1, "新作者"), Some((100, false)));
        assert_eq!(m.add_element_unique(1, "   "), None);
        // 手動入力用: 同名でも追加する。
        let dup = m.push_element(1, "Kouji").unwrap();
        assert_eq!(dup, 101);
        assert_eq!(next, 102);
        let tiers = &cats[1].tiers;
        assert_eq!(tiers.last().unwrap().tier_no, tiers.len() as i32);
    }

    #[test]
    fn count_new_elements_is_a_dry_run() {
        let (mut cats, mut next) = sample();
        let m = TagMaster::new(&mut cats, &mut next);
        let names: Vec<String> = ["kouji", "新人", "新人", "ＡＢＣ　Ｄ", "Hawk Bit", "hawkbit", " "].map(String::from).to_vec();
        assert_eq!(m.count_new_elements(Some(1), &names), 2, "既存と、同一視できる重複を除く");
        assert_eq!(m.count_new_elements(None, &names), 4, "カテゴリが無ければ全て新規（重複除く）");
        assert_eq!(next, 100, "何も書き込まない");
        assert_eq!(cats[1].tiers.len(), 3);
    }

    #[test]
    fn alias_split_join_and_escape() {
        assert_eq!(split_aliases("Kouji|Koji|コウジ"), ["Kouji", "Koji", "コウジ"]);
        assert_eq!(split_aliases(" A | |B "), ["A", "B"], "前後の空白と空の別名は捨てる");
        assert_eq!(split_aliases("単独"), ["単独"]);
        assert!(split_aliases("  ").is_empty());
        assert!(split_aliases("|").is_empty());
        assert_eq!(join_aliases(&["Kouji", " ", "Koji"]), "Kouji|Koji");
        assert_eq!(join_aliases::<&str>(&[]), "");
        assert_eq!(primary_alias("Kouji|Koji"), "Kouji");
        assert_eq!(primary_alias("Kouji"), "Kouji");
        assert_eq!(primary_alias("|Koji"), "Koji", "先頭が空なら次の別名");
        assert_eq!(escape_alias_sep("A|B"), "A｜B");
        assert_eq!(split_aliases(&escape_alias_sep("A|B")), ["A｜B"], "全角は区切りにならない");
        assert!(has_aliases("A|B"));
        assert!(!has_aliases("A|"));
        assert!(!has_aliases("A"));
        assert_eq!(alias_keys("Kouji|KOJI|ｋｏｊｉ|-"), ["kouji", "koji"], "重複と空は除く");
    }

    #[test]
    fn find_matches_any_alias_on_either_side() {
        let (mut cats, mut next) = sample();
        cats[1].tiers[0].element = Some("Kouji|Koji|コウジ".to_string());
        let m = TagMaster::new(&mut cats, &mut next);
        assert_eq!(m.find_element(1, "koji"), Some(10), "2番目の別名");
        assert_eq!(m.find_element(1, "コウジ"), Some(10));
        assert_eq!(m.find_element(1, "KOUJI"), Some(10));
        assert_eq!(m.find_element(1, "Other|Koji"), Some(10), "探す側の別名でも一致する");
        assert_eq!(m.find_element(1, "Kou ji"), Some(10), "区切り記号・空白の違いは吸収");
        assert_eq!(m.find_element(1, "Other|Another"), None);
        assert_eq!(m.find_element(1, "|"), None);
        assert_eq!(m.find_element_anywhere("Koji"), vec![(1, 10)]);
        assert_eq!(m.find_elements(1, "Koji|abc d"), vec![10, 11], "複数の既存要素に重なる");
        assert!(m.find_elements(9, "koji").is_empty());
    }

    #[test]
    fn pipe_in_search_name_is_not_swallowed_as_a_symbol() {
        let (mut cats, mut next) = sample();
        let m = TagMaster::new(&mut cats, &mut next);
        // 以前は `|` が無視記号で、"ko|uji" は "kouji" と同一視されていた。いまは別名の区切り。
        assert_eq!(m.find_element(1, "ko|uji"), None);
        // 全角 `｜` で登録した名前は、1つの名前として扱う（記号として無視される）。
        assert_eq!(normalize_key("ko｜uji"), "kouji");
    }

    #[test]
    fn add_element_unique_dedupes_by_alias() {
        let (mut cats, mut next) = sample();
        let mut m = TagMaster::new(&mut cats, &mut next);
        // 既存 Kouji と別名が重なる → 新規にしない。
        assert_eq!(m.add_element_unique(1, "Koji|KOUJI"), Some((10, false)));
        // 新しい別名つき要素は、そのまま1要素で登録される。
        assert_eq!(m.add_element_unique(1, "Tarou|Taro"), Some((100, true)));
        assert_eq!(m.add_element_unique(1, "taro"), Some((100, false)));
        assert_eq!(m.add_element_unique(1, "|"), None);
        assert_eq!(cats[1].tiers.last().unwrap().element.as_deref(), Some("Tarou|Taro"));
    }

    #[test]
    fn count_new_elements_handles_aliases() {
        let (mut cats, mut next) = sample();
        cats[1].tiers[0].element = Some("Kouji|Koji".to_string());
        let m = TagMaster::new(&mut cats, &mut next);
        let names: Vec<String> = ["koji", "Taro|Tarou", "tarou", "Jiro", "Jiro|Jirou", "|"].map(String::from).to_vec();
        assert_eq!(m.count_new_elements(Some(1), &names), 2, "koji は既存の別名、tarou は直前と重複、Jiro|Jirou は Jiro と重複");
        assert_eq!(m.count_new_elements(None, &names), 3, "koji / Taro|Tarou / Jiro（Jiro|Jirou は重複）");
    }

    #[test]
    fn remove_category_protects_main_and_system() {
        let (mut cats, mut next) = sample();
        let mut m = TagMaster::new(&mut cats, &mut next);
        assert_eq!(m.remove_category(0), Err(RemoveError::Main));
        assert_eq!(m.remove_category(3), Err(RemoveError::System));
        assert_eq!(m.remove_category(9), Err(RemoveError::OutOfRange));
        assert_eq!(m.remove_category(1), Ok(()));
        assert_eq!(cats.len(), 3);
    }

    #[test]
    fn remove_tier_renumbers() {
        let (mut cats, mut next) = sample();
        let mut m = TagMaster::new(&mut cats, &mut next);
        assert!(m.remove_tier(1, 0));
        assert!(!m.remove_tier(1, 9));
        assert!(!m.remove_tier(9, 0));
        assert_eq!(cats[1].tiers.iter().map(|t| t.tier_no).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn ensure_system_categories_creates_once_and_respects_user_edits() {
        let mut cats: Vec<TagManagerCategoryUi> = Vec::new();
        let mut next = 1u64;
        let mut m = TagMaster::new(&mut cats, &mut next);
        assert!(m.ensure_system_categories());
        let idx = m.find_system(SystemKind::ArchiveFormat).unwrap();
        assert!(m.find_element(idx, "tar.gz").is_some());
        assert!(m.find_element(idx, "CBZ").is_some());
        assert!(!m.ensure_system_categories(), "2回目は何もしない");
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].tiers.len(), ARCHIVE_FORMAT_EXTENSIONS.len() + 1, "拡張子＋フォルダ");
        assert_eq!(next as usize, cats[0].tiers.len() + 1);
        // 要素を全部消されても、カテゴリがあれば補充しない。
        cats[0].tiers.clear();
        let mut m = TagMaster::new(&mut cats, &mut next);
        assert!(!m.ensure_system_categories());
        assert!(cats[0].tiers.is_empty());
        // 名前を変えても種別キーで追跡できる。
        cats[0].name = "形式(改名)".to_string();
        let m = TagMaster::new(&mut cats, &mut next);
        assert_eq!(m.find_system(SystemKind::ArchiveFormat), Some(0));
    }

    #[test]
    fn merge_keeps_existing_and_respects_exclusive_categories() {
        let (mut cats, mut next) = sample();
        let m = TagMaster::new(&mut cats, &mut next);
        // 何も無いファイル: メイン1つ・形式1つ・作者は複数。
        assert_eq!(m.merge_file_tags(&[], &[1, 2, 30, 31, 10, 11]), vec![1, 30, 10, 11]);
        // メインが設定済みなら、メインは足さない。既存は順序ごと保つ。
        assert_eq!(m.merge_file_tags(&[2, 10], &[1, 11]), vec![2, 10, 11]);
        // 形式が既にあれば足さない。
        assert_eq!(m.merge_file_tags(&[31], &[30]), vec![31]);
        // 単一選択カテゴリも排他。
        assert_eq!(m.merge_file_tags(&[], &[21, 20]), vec![21]);
        // マスターに無いID・要素の無いtier・重複は足さない。
        assert_eq!(m.merge_file_tags(&[10], &[999, 12, 10, 11, 11]), vec![10, 11]);
        // 既存の孤立IDも消さない（自己修復は読み込み側の仕事）。
        assert_eq!(m.merge_file_tags(&[777], &[10]), vec![777, 10]);
    }

    #[test]
    fn system_kind_keys_round_trip() {
        for k in SystemKind::ALL {
            assert_eq!(SystemKind::from_key(k.as_key()), Some(k));
        }
        assert_eq!(SystemKind::from_key("future_kind"), None);
    }
}
