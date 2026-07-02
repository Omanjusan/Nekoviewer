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

    pub fn memory_warning_title(self) -> &'static str {
        match self {
            Lang::Japanese => "メモリ不足",
            Lang::English  => "Insufficient Memory",
            Lang::Chinese  => "内存不足",
        }
    }

    pub fn memory_warning_body(self) -> &'static str {
        match self {
            Lang::Japanese => "展開に十分なメモリが確保できません",
            Lang::English  => "Not enough memory available to open this file",
            Lang::Chinese  => "没有足够的内存来展开此文件",
        }
    }

    pub fn memory_warning_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "OK",
            Lang::English  => "OK",
            Lang::Chinese  => "确定",
        }
    }

    pub fn redecode_debounce_label(self, ms: u64) -> String {
        match self {
            Lang::Japanese => format!("[デバウンス{ms}ms]"),
            Lang::English  => format!("[Debounce {ms}ms]"),
            Lang::Chinese  => format!("[防抖{ms}ms]"),
        }
    }

    pub fn slot_label(self, n: usize) -> String {
        match self {
            Lang::Japanese => format!("[位置F{n}]"),
            Lang::English  => format!("[SlotF{n}]"),
            Lang::Chinese  => format!("[位置F{n}]"),
        }
    }

    /// 言語選択コンボボックス用の、その言語自身の正式名称（現在の表示言語に依存しない）。
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Japanese => "日本語",
            Lang::English  => "English",
            Lang::Chinese  => "简体中文",
        }
    }

    pub fn settings_button(self) -> &'static str {
        match self {
            Lang::Japanese => "[設定]",
            Lang::English  => "[Settings]",
            Lang::Chinese  => "[设置]",
        }
    }

    pub fn settings_title(self) -> &'static str {
        match self {
            Lang::Japanese => "設定",
            Lang::English  => "Settings",
            Lang::Chinese  => "设置",
        }
    }

    pub fn settings_close(self) -> &'static str {
        match self {
            Lang::Japanese => "閉じる",
            Lang::English  => "Close",
            Lang::Chinese  => "关闭",
        }
    }

    pub fn settings_apply(self) -> &'static str {
        match self {
            Lang::Japanese => "反映",
            Lang::English  => "Apply",
            Lang::Chinese  => "应用",
        }
    }

    pub fn settings_tab_common(self) -> &'static str {
        match self {
            Lang::Japanese => "共通",
            Lang::English  => "Common",
            Lang::Chinese  => "通用",
        }
    }

    pub fn settings_tab_anim(self) -> &'static str {
        match self {
            Lang::Japanese => "アニメ設定",
            Lang::English  => "Animation",
            Lang::Chinese  => "动画设置",
        }
    }

    pub fn settings_tab_static(self) -> &'static str {
        match self {
            Lang::Japanese => "静止画設定",
            Lang::English  => "Still Image",
            Lang::Chinese  => "静止图像设置",
        }
    }

    pub fn settings_tab_other(self) -> &'static str {
        match self {
            Lang::Japanese => "その他",
            Lang::English  => "Other",
            Lang::Chinese  => "其他",
        }
    }

    /// ダイアログ下部に1回だけ出す凡例。■=[反映]ですぐ効く、※=次回起動から効く。
    pub fn settings_legend(self) -> &'static str {
        match self {
            Lang::Japanese => "■ すぐ反映される項目　※ 次回起動時から反映される項目",
            Lang::English  => "■ Takes effect immediately　※ Takes effect after restarting the app",
            Lang::Chinese  => "■ 立即生效的项目　※ 下次启动后生效的项目",
        }
    }

    pub fn settings_base_resolution_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ベース解像度",
            Lang::English  => "■ Base resolution",
            Lang::Chinese  => "■ 基础分辨率",
        }
    }

    pub fn settings_base_resolution_actual(self) -> &'static str {
        match self {
            Lang::Japanese => "原寸",
            Lang::English  => "Original size",
            Lang::Chinese  => "原始尺寸",
        }
    }

    pub fn settings_base_resolution_follow_window(self) -> &'static str {
        match self {
            Lang::Japanese => "ウィンドウ追従",
            Lang::English  => "Follow window size",
            Lang::Chinese  => "跟随窗口",
        }
    }

    pub fn settings_base_resolution_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "「ウィンドウ追従」は、ビューアー窓のリサイズやズーム切替に合わせて表示解像度で再デコードする。「原寸」はファイルの元解像度のまま保持する（メモリ使用量は増えやすい）。",
            Lang::English  => "\"Follow window size\" re-decodes images to match the viewer window's size on resize/zoom changes. \"Original size\" keeps the file's native resolution (uses more memory).",
            Lang::Chinese  => "「跟随窗口」会在调整查看器窗口大小或切换缩放时,按显示分辨率重新解码。「原始尺寸」保持文件原始分辨率（更占内存）。",
        }
    }

    pub fn settings_debounce_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 再デコードのデバウンス時間",
            Lang::English  => "■ Redecode debounce delay",
            Lang::Chinese  => "■ 重新解码防抖延迟",
        }
    }

    pub fn settings_debounce_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "「ウィンドウ追従」時、リサイズ操作が止まってから再デコードを発火するまでの待ち時間。短いほど追従が速いが、リサイズ中の再デコード回数が増える。",
            Lang::English  => "When following window size, the delay after resizing stops before a redecode fires. Shorter values react faster but redecode more often while resizing.",
            Lang::Chinese  => "在「跟随窗口」模式下，从停止调整大小到触发重新解码之间的等待时间。数值越短响应越快，但调整过程中重新解码的次数也会增加。",
        }
    }

    pub fn settings_cache_size_label(self) -> &'static str {
        match self {
            Lang::Japanese => "※ キャッシュサイズ (MB)",
            Lang::English  => "※ Cache size (MB)",
            Lang::Chinese  => "※ 缓存大小 (MB)",
        }
    }

    pub fn settings_cache_size_page(self) -> &'static str {
        match self {
            Lang::Japanese => "ページキャッシュ上限",
            Lang::English  => "Page cache limit",
            Lang::Chinese  => "页面缓存上限",
        }
    }

    pub fn settings_cache_size_file(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイルキャッシュ上限",
            Lang::English  => "File cache limit",
            Lang::Chinese  => "文件缓存上限",
        }
    }

    pub fn settings_cache_size_auto(self) -> &'static str {
        match self {
            Lang::Japanese => "空欄=自動",
            Lang::English  => "blank = auto",
            Lang::Chinese  => "留空=自动",
        }
    }

    pub fn settings_max_decode_label(self) -> &'static str {
        match self {
            Lang::Japanese => "※ 取り扱い上限解像度（長辺）",
            Lang::English  => "※ Max decode resolution (long edge)",
            Lang::Chinese  => "※ 最大解码分辨率（长边）",
        }
    }

    pub fn settings_max_decode_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "「原寸」モードで開く画像の長辺が、ここで指定した px を超える場合に縦横比を保って縮小して読み込むガードレール。短辺はそれに応じて自動的に収まる。メモリ使用量の暴走を防ぐための上限で、通常は変更不要。",
            Lang::English  => "A guardrail that downsizes images whose long edge exceeds this many px when opened in \"Original size\" mode, keeping aspect ratio (the short edge shrinks proportionally). Prevents runaway memory use; usually no need to change.",
            Lang::Chinese  => "当以「原始尺寸」模式打开的图像长边超过此处指定的px时，将保持宽高比按比例缩小（短边自动跟随缩小）以防止内存占用失控。通常无需更改。",
        }
    }

    pub fn settings_resize_filter_viewer_label(self) -> &'static str {
        match self {
            Lang::Japanese => "※ リサイズフィルタ（ビューアー用）",
            Lang::English  => "※ Resize filter (viewer)",
            Lang::Chinese  => "※ 缩放滤镜（查看器用）",
        }
    }

    pub fn settings_resize_filter_thumb_label(self) -> &'static str {
        match self {
            Lang::Japanese => "※ リサイズフィルタ（サムネ用）",
            Lang::English  => "※ Resize filter (thumbnails)",
            Lang::Chinese  => "※ 缩放滤镜（缩略图用）",
        }
    }

    pub fn settings_lang_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 言語",
            Lang::English  => "■ Language",
            Lang::Chinese  => "■ 语言",
        }
    }

    pub fn settings_ring_bounds_label(self) -> &'static str {
        match self {
            Lang::Japanese => "※ リングバッファの先読み枚数（下限〜上限）",
            Lang::English  => "※ Ring buffer prefetch frames (min - max)",
            Lang::Chinese  => "※ 环形缓冲区预读帧数（下限～上限）",
        }
    }

    pub fn settings_ring_bounds_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "GIF/APNG/AVIF/WebPアニメーションを逐次デコードする際、メモリに保持しておくフレーム数の範囲。多いほど滑らかだがメモリを消費する。",
            Lang::English  => "The range of frames kept in memory while sequentially decoding GIF/APNG/AVIF/WebP animations. More frames play smoother but use more memory.",
            Lang::Chinese  => "逐帧解码GIF/APNG/AVIF/WebP动画时，保留在内存中的帧数范围。数值越大播放越流畅，但内存占用也越高。",
        }
    }

    pub fn settings_static_placeholder(self) -> &'static str {
        match self {
            Lang::Japanese => "現在、静止画専用の設定項目はありません",
            Lang::English  => "No still-image-specific settings yet",
            Lang::Chinese  => "目前没有静止图像专用设置项",
        }
    }

    pub fn settings_version_label(self) -> &'static str {
        match self {
            Lang::Japanese => "バージョン",
            Lang::English  => "Version",
            Lang::Chinese  => "版本",
        }
    }

    pub fn settings_viewer_blocked(self) -> &'static str {
        match self {
            Lang::Japanese => "設定変更中は操作できません",
            Lang::English  => "Locked while settings are open",
            Lang::Chinese  => "设置窗口打开期间无法操作",
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
