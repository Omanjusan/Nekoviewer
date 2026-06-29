use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Japanese,
    English,
    Chinese,
}

impl Lang {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Lang::English,
            2 => Lang::Chinese,
            _ => Lang::Japanese,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Lang::Japanese => 0,
            Lang::English  => 1,
            Lang::Chinese  => 2,
        }
    }

    pub fn sort_name(self) -> &'static str {
        match self {
            Lang::Japanese => "[名前]",
            Lang::English  => "[Name]",
            Lang::Chinese  => "[名称]",
        }
    }

    pub fn sort_date(self) -> &'static str {
        match self {
            Lang::Japanese => "[日付]",
            Lang::English  => "[Date]",
            Lang::Chinese  => "[日期]",
        }
    }

    pub fn sort_size(self) -> &'static str {
        match self {
            Lang::Japanese => "[サイズ]",
            Lang::English  => "[Size]",
            Lang::Chinese  => "[大小]",
        }
    }

    pub fn sort_natural(self) -> &'static str {
        match self {
            Lang::Japanese => "[自然数]",
            Lang::English  => "[Natural]",
            Lang::Chinese  => "[自然序]",
        }
    }

    pub fn sort_asc(self) -> &'static str {
        match self {
            Lang::Japanese => "[昇順]",
            Lang::English  => "[Asc]",
            Lang::Chinese  => "[升序]",
        }
    }

    pub fn sort_desc(self) -> &'static str {
        match self {
            Lang::Japanese => "[降順]",
            Lang::English  => "[Desc]",
            Lang::Chinese  => "[降序]",
        }
    }

    pub fn hidden_on(self) -> &'static str {
        match self {
            Lang::Japanese => "[隠 ON]",
            Lang::English  => "[Hidden ON]",
            Lang::Chinese  => "[隐 ON]",
        }
    }

    pub fn hidden_off(self) -> &'static str {
        match self {
            Lang::Japanese => "[隠OFF]",
            Lang::English  => "[HiddenOFF]",
            Lang::Chinese  => "[隐OFF]",
        }
    }

    pub fn page_single(self) -> &'static str {
        match self {
            Lang::Japanese => "[単ページ]",
            Lang::English  => "[Single]",
            Lang::Chinese  => "[单页]",
        }
    }

    pub fn page_spread_left(self) -> &'static str {
        match self {
            Lang::Japanese => "[見開き左]",
            Lang::English  => "[Spread L]",
            Lang::Chinese  => "[双页左]",
        }
    }

    pub fn page_spread_right(self) -> &'static str {
        match self {
            Lang::Japanese => "[見開き右]",
            Lang::English  => "[Spread R]",
            Lang::Chinese  => "[双页右]",
        }
    }

    pub fn spread_back(self) -> &'static str {
        match self {
            Lang::Japanese => "[1P戻す]",
            Lang::English  => "[←1P]",
            Lang::Chinese  => "[←1页]",
        }
    }

    pub fn spread_fwd(self) -> &'static str {
        match self {
            Lang::Japanese => "[1P進む]",
            Lang::English  => "[1P→]",
            Lang::Chinese  => "[1页→]",
        }
    }

    pub fn spread_offset_on(self) -> &'static str {
        match self {
            Lang::Japanese => "+1Pずれ中",
            Lang::English  => "+1P offset",
            Lang::Chinese  => "+1页偏移",
        }
    }

    pub fn spread_aligned(self) -> &'static str {
        match self {
            Lang::Japanese => "整列中",
            Lang::English  => "aligned",
            Lang::Chinese  => "对齐",
        }
    }

    pub fn loading(self) -> &'static str {
        match self {
            Lang::Japanese => "読み込み中...",
            Lang::English  => "Loading...",
            Lang::Chinese  => "加载中...",
        }
    }

    pub fn drives(self) -> &'static str {
        match self {
            Lang::Japanese => "ドライブ",
            Lang::English  => "Drives",
            Lang::Chinese  => "驱动器",
        }
    }

    pub fn toast_no_prev(self) -> &'static str {
        match self {
            Lang::Japanese => "これ以上開けるファイルは前方に存在しません",
            Lang::English  => "No more files before this one",
            Lang::Chinese  => "前方没有可打开的文件",
        }
    }

    pub fn toast_no_next(self) -> &'static str {
        match self {
            Lang::Japanese => "これ以上開けるファイルは後方に存在しません",
            Lang::English  => "No more files after this one",
            Lang::Chinese  => "后方没有可打开的文件",
        }
    }

    pub fn viewer_fallback(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューア",
            Lang::English  => "Viewer",
            Lang::Chinese  => "查看器",
        }
    }

    pub fn thumb_saved(self, saved: usize, total: usize) -> String {
        match self {
            Lang::Japanese => format!("サムネ保存: {} / {}", saved, total),
            Lang::English  => format!("Thumbs: {} / {}", saved, total),
            Lang::Chinese  => format!("缩略图: {} / {}", saved, total),
        }
    }

    pub fn file_info(self, date_str: &str, mb: f64, filename: &str) -> String {
        match self {
            Lang::Japanese => format!("更新日時:{date_str}   ファイルサイズ：{mb:.1}MB   {filename}"),
            Lang::English  => format!("Modified:{date_str}   Size:{mb:.1}MB   {filename}"),
            Lang::Chinese  => format!("修改时间:{date_str}   大小：{mb:.1}MB   {filename}"),
        }
    }

    pub fn invalid_zip(self, name: &str) -> String {
        match self {
            Lang::Japanese => format!("「{name}」は画像が含まれない無効なZIPです。表示できません"),
            Lang::English  => format!("\"{name}\" contains no images and cannot be opened"),
            Lang::Chinese  => format!("「{name}」不包含图片，无法显示"),
        }
    }

    pub fn slot_label(self, n: usize) -> String {
        match self {
            Lang::Japanese => format!("[位置F{n}]"),
            Lang::English  => format!("[SlotF{n}]"),
            Lang::Chinese  => format!("[位置F{n}]"),
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn t() -> Lang {
    Lang::from_u8(LANG.load(Ordering::Relaxed))
}

pub fn set(lang: Lang) {
    LANG.store(lang.as_u8(), Ordering::Relaxed);
}

pub fn set_from_code(code: &str) {
    let lang = match code {
        "en" => Lang::English,
        "cn" => Lang::Chinese,
        _    => Lang::Japanese,
    };
    set(lang);
}

pub fn lang_code() -> &'static str {
    match t() {
        Lang::Japanese => "ja",
        Lang::English  => "en",
        Lang::Chinese  => "cn",
    }
}
