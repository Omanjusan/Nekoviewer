//! ツリー（仮想フォルダ・実フォルダ）の並び条件。右クリックの「ソート条件設定」で選び、stateファイルに保存する。
//! フォルダカード側の並びはエクスプローラーのソート指定に従うので、ここでは扱わない（独立）。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeSortKey {
    /// 登録した順（仮想ツリーのみ）
    Registration,
    Name,
    /// 実フォルダの更新日時
    Date,
}

impl TreeSortKey {
    fn to_state_str(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::Name => "name",
            Self::Date => "date",
        }
    }

    fn from_state_str(s: &str) -> Option<Self> {
        match s {
            "registration" => Some(Self::Registration),
            "name" => Some(Self::Name),
            "date" => Some(Self::Date),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeSort {
    pub key: TreeSortKey,
    pub ascending: bool,
}

impl TreeSort {
    /// 仮想ツリーの既定（登録順・昇順）。従来の並びと同じ。
    pub const VIRTUAL_DEFAULT: Self = Self { key: TreeSortKey::Registration, ascending: true };
    /// 実ツリーの既定（名前・昇順）。従来の並び（パス昇順）と同じ。
    pub const REAL_DEFAULT: Self = Self { key: TreeSortKey::Name, ascending: true };

    /// stateファイルの値（例: `name:desc`）。
    pub fn to_state_str(self) -> String {
        format!("{}:{}", self.key.to_state_str(), if self.ascending { "asc" } else { "desc" })
    }

    pub fn from_state_str(s: &str) -> Option<Self> {
        let (key, dir) = s.trim().split_once(':')?;
        let ascending = match dir {
            "asc" => true,
            "desc" => false,
            _ => return None,
        };
        Some(Self { key: TreeSortKey::from_state_str(key)?, ascending })
    }
}

/// ユーザーが選んだ並び条件（未保存・既定のままは None）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeSorts {
    pub virtual_tree: Option<TreeSort>,
    pub real_tree: Option<TreeSort>,
}

impl TreeSorts {
    pub fn virtual_tree_or_default(&self) -> TreeSort {
        self.virtual_tree.unwrap_or(TreeSort::VIRTUAL_DEFAULT)
    }

    pub fn real_tree_or_default(&self) -> TreeSort {
        self.real_tree.unwrap_or(TreeSort::REAL_DEFAULT)
    }

    /// stateファイルに書く `tree_sort_*` の行（Some のものだけ）。
    pub fn state_lines(&self) -> String {
        let mut out = String::new();
        if let Some(s) = self.virtual_tree {
            out.push_str(&format!("tree_sort_virtual={}\n", s.to_state_str()));
        }
        if let Some(s) = self.real_tree {
            out.push_str(&format!("tree_sort_real={}\n", s.to_state_str()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_string_roundtrips_for_every_key_and_direction() {
        for key in [TreeSortKey::Registration, TreeSortKey::Name, TreeSortKey::Date] {
            for ascending in [true, false] {
                let s = TreeSort { key, ascending };
                assert_eq!(TreeSort::from_state_str(&s.to_state_str()), Some(s));
            }
        }
        assert_eq!(TreeSort { key: TreeSortKey::Name, ascending: false }.to_state_str(), "name:desc");
    }

    #[test]
    fn invalid_state_strings_are_rejected() {
        for bad in ["", "name", "name:", "name:up", "size:asc", ":asc", "name:asc:x"] {
            assert_eq!(TreeSort::from_state_str(bad), None, "{bad}");
        }
        assert_eq!(TreeSort::from_state_str(" date:asc "), Some(TreeSort { key: TreeSortKey::Date, ascending: true }));
    }

    #[test]
    fn default_is_registration_ascending_and_writes_no_line() {
        assert_eq!(TreeSorts::default().virtual_tree_or_default(), TreeSort::VIRTUAL_DEFAULT);
        assert_eq!(TreeSorts::default().state_lines(), "");
        let s = TreeSorts { virtual_tree: Some(TreeSort { key: TreeSortKey::Date, ascending: false }), real_tree: None };
        assert_eq!(s.state_lines(), "tree_sort_virtual=date:desc\n");
        assert_eq!(TreeSorts::default().real_tree_or_default(), TreeSort::REAL_DEFAULT);
        let r = TreeSorts { virtual_tree: None, real_tree: Some(TreeSort { key: TreeSortKey::Name, ascending: false }) };
        assert_eq!(r.state_lines(), "tree_sort_real=name:desc\n");
    }
}
