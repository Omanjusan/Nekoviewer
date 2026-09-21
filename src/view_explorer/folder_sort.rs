//! フォルダカード（実サブフォルダ・仮想の子）の並び順。
//!
//! ファイルカードと同じソートキー・昇降に従う。フォルダにはサイズが無いので、`Size` は名前順に
//! フォールバックする。`Date` は更新日時で並べ、更新日時が分からないもの（取得できない・ネットワーク配下・
//! リンク切れ）は昇降どちらでも末尾に置く。同時刻・不明同士は名前の昇順。

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::types::ExplorerSortKey;

fn compare(
    key: ExplorerSortKey,
    ascending: bool,
    a: (&str, Option<SystemTime>),
    b: (&str, Option<SystemTime>),
) -> Ordering {
    let directed = |c: Ordering| if ascending { c } else { c.reverse() };
    match key {
        ExplorerSortKey::Date => match (a.1, b.1) {
            (Some(x), Some(y)) => directed(x.cmp(&y)).then_with(|| a.0.cmp(b.0)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.0.cmp(b.0),
        },
        ExplorerSortKey::Name | ExplorerSortKey::Size => directed(a.0.cmp(b.0)),
    }
}

/// `info` が返す（名前, 更新日時）で並べ替えた新しい並びを返す。
pub(super) fn sort_folders<T>(
    items: Vec<T>,
    key: ExplorerSortKey,
    ascending: bool,
    info: impl Fn(&T) -> (String, Option<SystemTime>),
) -> Vec<T> {
    let mut keyed: Vec<(String, Option<SystemTime>, T)> = items
        .into_iter()
        .map(|t| {
            let (name, mtime) = info(&t);
            (name, mtime, t)
        })
        .collect();
    keyed.sort_by(|a, b| compare(key, ascending, (&a.0, a.1), (&b.0, b.1)));
    keyed.into_iter().map(|(_, _, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    /// （名前, 更新日時）の並びを、そのまま info として並べ替えた名前の列を返す。
    fn order(items: &[(&str, Option<SystemTime>)], key: ExplorerSortKey, ascending: bool) -> Vec<String> {
        let v: Vec<(String, Option<SystemTime>)> = items.iter().map(|(n, m)| (n.to_string(), *m)).collect();
        sort_folders(v, key, ascending, |x| x.clone()).into_iter().map(|x| x.0).collect()
    }

    const SAMPLE: [(&str, Option<SystemTime>); 4] = [("b", None), ("c", Some(SystemTime::UNIX_EPOCH)), ("a", None), ("d", Some(SystemTime::UNIX_EPOCH))];

    #[test]
    fn name_sort_follows_direction() {
        let items = [("b", at(1)), ("c", at(2)), ("a", at(3))];
        assert_eq!(order(&items, ExplorerSortKey::Name, true), ["a", "b", "c"]);
        assert_eq!(order(&items, ExplorerSortKey::Name, false), ["c", "b", "a"]);
    }

    #[test]
    fn size_key_falls_back_to_name() {
        let items = [("b", at(1)), ("c", at(2)), ("a", at(3))];
        assert_eq!(order(&items, ExplorerSortKey::Size, true), ["a", "b", "c"]);
        assert_eq!(order(&items, ExplorerSortKey::Size, false), ["c", "b", "a"]);
    }

    #[test]
    fn date_sort_orders_by_mtime_and_direction() {
        let items = [("a", at(30)), ("b", at(10)), ("c", at(20))];
        assert_eq!(order(&items, ExplorerSortKey::Date, true), ["b", "c", "a"]);
        assert_eq!(order(&items, ExplorerSortKey::Date, false), ["a", "c", "b"]);
    }

    #[test]
    fn date_sort_puts_unknown_last_in_both_directions() {
        let items = [("x", None), ("a", at(30)), ("y", None), ("b", at(10))];
        assert_eq!(order(&items, ExplorerSortKey::Date, true), ["b", "a", "x", "y"]);
        assert_eq!(order(&items, ExplorerSortKey::Date, false), ["a", "b", "x", "y"]);
    }

    #[test]
    fn date_ties_break_by_name_ascending_in_both_directions() {
        let (asc, desc) = (order(&SAMPLE, ExplorerSortKey::Date, true), order(&SAMPLE, ExplorerSortKey::Date, false));
        assert_eq!(asc, ["c", "d", "a", "b"]);
        assert_eq!(desc, ["c", "d", "a", "b"]);
    }
}
