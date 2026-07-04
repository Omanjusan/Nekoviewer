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

    pub fn explorer_filter_label(self) -> &'static str {
        match self {
            Lang::Japanese => "フィルタ",
            Lang::English  => "Filter",
            Lang::Chinese  => "过滤",
        }
    }

    pub fn explorer_filter_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル名で絞り込み... (* ? [...] 使用可)",
            Lang::English  => "Filter by filename... (* ? [...] supported)",
            Lang::Chinese  => "按文件名筛选...（支持 * ? [...]）",
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

    pub fn wayland_warning_title(self) -> &'static str {
        match self {
            Lang::Japanese => "Wayland環境での既知の制限",
            Lang::English  => "Known limitation on Wayland",
            Lang::Chinese  => "Wayland环境下的已知限制",
        }
    }

    pub fn wayland_warning_body(self) -> &'static str {
        match self {
            Lang::Japanese => "Wayland環境では、ビューアーをフルスクリーン表示中にウィンドウの描画・操作が一時的に固まることがあります（コンポジタ/GPUドライバ側の要因のため、アプリ側では解決できません）。フルスクリーンの利用は非推奨です。",
            Lang::English  => "On Wayland, the viewer window may temporarily freeze while in fullscreen (this is caused by the compositor/GPU driver stack and cannot be fixed on the app side). Using fullscreen is not recommended in this environment.",
            Lang::Chinese  => "在Wayland环境下，查看器窗口在全屏显示时可能会暂时卡住（这是由合成器/GPU驱动层面导致的，应用程序无法修复）。不建议在此环境下使用全屏。",
        }
    }

    pub fn wayland_warning_dont_show_again(self) -> &'static str {
        match self {
            Lang::Japanese => "次回から表示しない",
            Lang::English  => "Don't show this again",
            Lang::Chinese  => "下次不再显示",
        }
    }

    pub fn wayland_warning_close(self) -> &'static str {
        match self {
            Lang::Japanese => "閉じる",
            Lang::English  => "Close",
            Lang::Chinese  => "关闭",
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

    pub fn settings_tab_viewer(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューアー",
            Lang::English  => "Viewer",
            Lang::Chinese  => "查看器",
        }
    }

    /// タブ内の大項目見出し。■は個々の設定項目(即時反映マーク)専用の記号なので、
    /// 見出し自体には付けず、呼び出し側で太字・大きめフォントにして区別する
    /// （settings_legend の凡例と衝突させないため）。
    pub fn settings_thumbbar_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "アーカイブ内サムネイル",
            Lang::English  => "In-archive thumbnails",
            Lang::Chinese  => "压缩包内缩略图",
        }
    }

    pub fn settings_thumbbar_pos_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ サムネイルバー配置",
            Lang::English  => "■ Thumbnail bar position",
            Lang::Chinese  => "■ 缩略图栏位置",
        }
    }

    pub fn settings_thumbbar_pos_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューアー画面を軸とした表示位置。単一ファイル、または1ファイルのみ格納するアーカイブでは、この設定に関わらず表示しない。",
            Lang::English  => "Where the thumbnail bar sits relative to the viewer. Hidden regardless of this setting for a single file, or an archive that contains only one file.",
            Lang::Chinese  => "以查看器画面为基准的显示位置。对于单个文件，或仅包含1个文件的压缩包，无论此设置如何都不会显示。",
        }
    }

    pub fn settings_thumbbar_pos_left(self) -> &'static str {
        match self {
            Lang::Japanese => "左側縦",
            Lang::English  => "Left (vertical)",
            Lang::Chinese  => "左侧竖排",
        }
    }

    pub fn settings_thumbbar_pos_right(self) -> &'static str {
        match self {
            Lang::Japanese => "右側縦",
            Lang::English  => "Right (vertical)",
            Lang::Chinese  => "右侧竖排",
        }
    }

    pub fn settings_thumbbar_pos_top(self) -> &'static str {
        match self {
            Lang::Japanese => "上部横",
            Lang::English  => "Top (horizontal)",
            Lang::Chinese  => "顶部横排",
        }
    }

    pub fn settings_thumbbar_pos_bottom(self) -> &'static str {
        match self {
            Lang::Japanese => "下部横",
            Lang::English  => "Bottom (horizontal)",
            Lang::Chinese  => "底部横排",
        }
    }

    pub fn settings_thumbbar_pos_none(self) -> &'static str {
        match self {
            Lang::Japanese => "表示なし",
            Lang::English  => "Hidden",
            Lang::Chinese  => "不显示",
        }
    }

    pub fn settings_thumbbar_size_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ サムネ長辺サイズ",
            Lang::English  => "■ Thumbnail long-edge size",
            Lang::Chinese  => "■ 缩略图长边尺寸",
        }
    }

    pub fn settings_thumbbar_size_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "サムネイルバーに並ぶサムネイル1枚の長辺サイズ（px）。",
            Lang::English  => "Long-edge size (px) of each thumbnail in the bar.",
            Lang::Chinese  => "缩略图栏中每个缩略图长边的尺寸（px）。",
        }
    }

    pub fn settings_thumbbar_idle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 自動非表示までの待機時間",
            Lang::English  => "■ Auto-hide delay",
            Lang::Chinese  => "■ 自动隐藏等待时间",
        }
    }

    pub fn settings_thumbbar_idle_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "ページ操作が停滞してからサムネイルバーを消すまでの待機時間。0 = 常時表示。",
            Lang::English  => "How long to wait after page navigation stops before hiding the thumbnail bar. 0 = always shown.",
            Lang::Chinese  => "翻页操作停止后到隐藏缩略图栏为止的等待时间。0 = 始终显示。",
        }
    }

    pub fn settings_thumbbar_idle_always(self) -> &'static str {
        match self {
            Lang::Japanese => "常時表示",
            Lang::English  => "Always shown",
            Lang::Chinese  => "始终显示",
        }
    }

    pub fn settings_thumbbar_overlap_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 本画像との重なりを許可",
            Lang::English  => "■ Allow overlap with the main image",
            Lang::Chinese  => "■ 允许与正文图像重叠",
        }
    }

    pub fn settings_thumbbar_overlap_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "ONの場合、本画像はサムネイルバーの領域を意識せずに描画し、サムネイルバーはその前面にオーバーレイ表示する。",
            Lang::English  => "When on, the main image is drawn without reserving space for the thumbnail bar, and the bar overlays on top of it instead.",
            Lang::Chinese  => "开启后，正文图像不为缩略图栏预留空间，缩略图栏将叠加显示在其上方。",
        }
    }

    pub fn settings_thumbbar_marker_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 現在地マーカー色 (RGBA)",
            Lang::English  => "■ Current-position marker color (RGBA)",
            Lang::Chinese  => "■ 当前位置标记颜色 (RGBA)",
        }
    }

    pub fn settings_thumbbar_marker_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "サムネイルバー上で現在表示中のページ（見開きなら2枚とも）に重ねる半透明ボックスの色。",
            Lang::English  => "Color of the translucent box overlaid on the currently viewed page(s) in the thumbnail bar (both pages when in spread mode).",
            Lang::Chinese  => "叠加在缩略图栏中当前显示页面（跨页时为两页）上的半透明方块颜色。",
        }
    }

    /// ダイアログ下部に1回だけ出す凡例。全項目に■が付き、[反映]後に次回起動が必要な
    /// 項目だけ■の直後に※も付く（■ ※<ラベル>）。
    pub fn settings_legend(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ←通常の設定項目マーク、これは変更保存で即時反映されます\n■※ ←保存しても反映されるのは次回起動後の項目マーク",
            Lang::English  => "■ ← Normal setting, applied immediately when saved\n■※ ← Saved now, but only takes effect after restarting the app",
            Lang::Chinese  => "■ ←普通设置项标记，保存后立即生效\n■※ ←保存后仍需重启才能生效的项目标记",
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
            Lang::Japanese => "「ウィンドウ追従」は、ビューアー窓のリサイズやズーム切替に合わせて表示解像度で再デコードする。「原寸」は縦横比を保ったまま、下の「原寸時に許容する最大長辺幅」に収まる解像度で保持する（無制限ではない）。",
            Lang::English  => "\"Follow window size\" re-decodes images to match the viewer window's size on resize/zoom changes. \"Original size\" keeps the file's resolution (aspect ratio preserved) up to the \"Max long edge for original size\" limit below — not truly unlimited.",
            Lang::Chinese  => "「跟随窗口」会在调整查看器窗口大小或切换缩放时,按显示分辨率重新解码。「原始尺寸」在保持宽高比的前提下，保留不超过下方「原始尺寸下允许的最大长边」的分辨率（并非无限制）。",
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

    pub fn settings_cache_system_ram(self, mb: u64) -> String {
        match self {
            Lang::Japanese => format!("システム最大RAM: {mb} MB （指定最大サイズは最大量の50%）"),
            Lang::English  => format!("System RAM: {mb} MB (the max you can specify is 50% of this)"),
            Lang::Chinese  => format!("系统最大内存: {mb} MB （可指定的最大值为该值的50%）"),
        }
    }

    pub fn settings_cache_manual_toggle(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ※ 手動でキャッシュ上限を指定する",
            Lang::English  => "■ ※ Manually set the cache limit",
            Lang::Chinese  => "■ ※ 手动指定缓存上限",
        }
    }

    pub fn settings_cache_manual_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "このアプリが使ってよいキャッシュ合計（ページキャッシュ+ファイルキャッシュ）の上限。チェックを外すとシステムRAMの30%を自動で使う。内訳はページ70%・ファイル30%に自動配分される。",
            Lang::English  => "The total cache limit (page cache + file cache) this app may use. Unchecked = automatically uses 30% of system RAM. Split 70% page / 30% file internally.",
            Lang::Chinese  => "本应用可使用的缓存总量上限（页面缓存+文件缓存）。取消勾选则自动使用系统RAM的30%。内部按页面70%／文件30%自动分配。",
        }
    }

    pub fn settings_cache_over_budget(self) -> &'static str {
        match self {
            Lang::Japanese => "キャッシュサイズ合計がシステム最大RAMの50%を超えています、適用されません",
            Lang::English  => "Total cache size exceeds 50% of system RAM and will not be applied",
            Lang::Chinese  => "缓存总量超过了系统最大内存的50%，不会被应用",
        }
    }

    pub fn settings_max_decode_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ※ 原寸時に許容する最大長辺幅",
            Lang::English  => "■ ※ Max long edge for original size",
            Lang::Chinese  => "■ ※ 原始尺寸下允许的最大长边",
        }
    }

    pub fn settings_max_decode_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "「原寸」モードの実体はこの値。画像の長辺がここで指定した px を超える場合、縦横比を保ったまま長辺がこの値に収まるよう縮小する（短辺は比率に応じて自動的に決まる）。メモリ使用量の暴走を防ぐための上限で、通常は変更不要。",
            Lang::English  => "This value defines what \"Original size\" mode actually means: if an image's long edge exceeds this many px, it's downscaled so the long edge fits this value, aspect ratio preserved (the short edge follows proportionally). Prevents runaway memory use; usually no need to change.",
            Lang::Chinese  => "「原始尺寸」模式的实际含义就是这个值：当图像长边超过此处指定的px时，将保持宽高比缩小，使长边收敛到该值（短边按比例自动决定）。用于防止内存占用失控，通常无需更改。",
        }
    }

    pub fn settings_thumb_size_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ サムネイルサイズ",
            Lang::English  => "■ Thumbnail size",
            Lang::Chinese  => "■ 缩略图尺寸",
        }
    }

    pub fn settings_thumb_size_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "グリッド表示でのサムネイル長辺サイズ（px）。",
            Lang::English  => "Long-edge size (px) of thumbnails in grid view.",
            Lang::Chinese  => "网格视图中缩略图长边尺寸（px）。",
        }
    }

    pub fn settings_resize_filter_viewer_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ※ リサイズフィルタ（ビューアー用）",
            Lang::English  => "■ ※ Resize filter (viewer)",
            Lang::Chinese  => "■ ※ 缩放滤镜（查看器用）",
        }
    }

    pub fn settings_resize_filter_thumb_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ ※ リサイズフィルタ（サムネ用）",
            Lang::English  => "■ ※ Resize filter (thumbnails)",
            Lang::Chinese  => "■ ※ 缩放滤镜（缩略图用）",
        }
    }

    pub fn settings_show_hidden_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 隠しファイルを表示する",
            Lang::English  => "■ Show hidden files",
            Lang::Chinese  => "■ 显示隐藏文件",
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
            Lang::Japanese => "■ ※ リングバッファの先読み枚数（下限〜上限）",
            Lang::English  => "■ ※ Ring buffer prefetch frames (min - max)",
            Lang::Chinese  => "■ ※ 环形缓冲区预读帧数（下限～上限）",
        }
    }

    pub fn settings_ring_min_label(self) -> &'static str {
        match self {
            Lang::Japanese => "下限",
            Lang::English  => "Min",
            Lang::Chinese  => "下限",
        }
    }

    pub fn settings_ring_max_label(self) -> &'static str {
        match self {
            Lang::Japanese => "上限",
            Lang::English  => "Max",
            Lang::Chinese  => "上限",
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
