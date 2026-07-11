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

    pub fn rotate_ccw(self) -> &'static str {
        match self {
            Lang::Japanese => "反時計回りに回転",
            Lang::English  => "Rotate counter-clockwise",
            Lang::Chinese  => "逆时针旋转",
        }
    }

    pub fn rotate_cw(self) -> &'static str {
        match self {
            Lang::Japanese => "時計回りに回転",
            Lang::English  => "Rotate clockwise",
            Lang::Chinese  => "顺时针旋转",
        }
    }

    pub fn rotation_carry_over_label(self) -> &'static str {
        match self {
            Lang::Japanese => "回転を引き継ぐ",
            Lang::English  => "Carry over rotation",
            Lang::Chinese  => "旋转跨页保留",
        }
    }

    pub fn exif_orientation_toolbar_label(self) -> &'static str {
        match self {
            Lang::Japanese => "EXIF回転",
            Lang::English  => "Exif rotation",
            Lang::Chinese  => "Exif旋转",
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

    // spread_offset_on / spread_aligned は廃止（ずれ状態はビューアーツールバーの
    // OffsetIndicator が文言なしの「0 / ←1 / 1→」で表示する。toolbar.rs 参照）

    pub fn spread_save_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "見開き設定保存状態",
            Lang::English  => "Save spread state",
            Lang::Chinese  => "保存双页设置",
        }
    }

    pub fn spread_save_overwrite_label(self) -> &'static str {
        match self {
            Lang::Japanese => "現在の見開き設定で上書き保存",
            Lang::English  => "Overwrite with current spread state",
            Lang::Chinese  => "用当前双页设置覆盖保存",
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

    pub fn favorite_detail_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り詳細設定",
            Lang::English  => "Favorite Details...",
            Lang::Chinese  => "收藏详细设置",
        }
    }

    pub fn favorite_detail_menu_bulk(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("お気に入り詳細設定 ({count}件)"),
            Lang::English  => format!("Favorite Details... ({count} items)"),
            Lang::Chinese  => format!("收藏详细设置（{count} 项）"),
        }
    }

    pub fn favorite_detail_common_only_note(self) -> &'static str {
        match self {
            Lang::Japanese => "※共通のお気に入り以外は省略しています",
            Lang::English  => "* Folders not shared by all selected files are omitted",
            Lang::Chinese  => "※未显示所选文件不共有的收藏夹",
        }
    }

    pub fn favorite_overwrite_confirm_title(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り一括設定の確認",
            Lang::English  => "Confirm Bulk Favorite Update",
            Lang::Chinese  => "确认批量收藏设置",
        }
    }

    pub fn favorite_overwrite_confirm_message(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("選択した{count}件の既存のお気に入り設定は上書きされます。よろしいですか？"),
            Lang::English  => format!("The existing favorite settings for the selected {count} item(s) will be overwritten. Continue?"),
            Lang::Chinese  => format!("所选 {count} 项现有的收藏设置将被覆盖。确定继续吗？"),
        }
    }

    pub fn favorite_overwrite_confirm_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "上書きする",
            Lang::English  => "Overwrite",
            Lang::Chinese  => "覆盖",
        }
    }

    pub fn favorite_detail_dialog_title(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り詳細設定",
            Lang::English  => "Favorite Details",
            Lang::Chinese  => "收藏详细设置",
        }
    }

    pub fn favorite_detail_enable_checkbox(self) -> &'static str {
        match self {
            Lang::Japanese => "このファイルをお気に入りに登録する",
            Lang::English  => "Add this file to favorites",
            Lang::Chinese  => "将此文件加入收藏",
        }
    }

    pub fn favorite_detail_available_label(self) -> &'static str {
        match self {
            Lang::Japanese => "定義済みお気に入りフォルダ",
            Lang::English  => "Available Folders",
            Lang::Chinese  => "已定义的收藏夹",
        }
    }

    pub fn favorite_detail_assigned_label(self) -> &'static str {
        match self {
            Lang::Japanese => "登録先",
            Lang::English  => "Assigned To",
            Lang::Chinese  => "已加入",
        }
    }

    pub fn network_checking_toast(self) -> &'static str {
        match self {
            Lang::Japanese => "ネットワーク接続を確認しています...",
            Lang::English  => "Checking network connection...",
            Lang::Chinese  => "正在检查网络连接...",
        }
    }

    pub fn favorite_view_header_unsorted(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り: 未整理",
            Lang::English  => "Favorites: Unsorted",
            Lang::Chinese  => "收藏：未整理",
        }
    }

    pub fn favorite_view_header_folder(self, name: &str) -> String {
        match self {
            Lang::Japanese => format!("お気に入り: {name}"),
            Lang::English  => format!("Favorites: {name}"),
            Lang::Chinese  => format!("收藏：{name}"),
        }
    }

    pub fn folder_tab_real(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダ",
            Lang::English  => "Folders",
            Lang::Chinese  => "文件夹",
        }
    }

    pub fn folder_tab_favorites(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り",
            Lang::English  => "Favorites",
            Lang::Chinese  => "收藏夹",
        }
    }

    pub fn favorite_unsorted_label(self) -> &'static str {
        match self {
            Lang::Japanese => "（未整理のお気に入り）",
            Lang::English  => "(Unsorted Favorites)",
            Lang::Chinese  => "（未整理的收藏）",
        }
    }

    pub fn favorite_rename_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "リネーム (F2)",
            Lang::English  => "Rename (F2)",
            Lang::Chinese  => "重命名 (F2)",
        }
    }

    pub fn favorite_delete_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "削除",
            Lang::English  => "Delete",
            Lang::Chinese  => "删除",
        }
    }

    pub fn favorite_dialog_title_create(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入りフォルダの新規作成",
            Lang::English  => "Create Favorite Folder",
            Lang::Chinese  => "新建收藏夹",
        }
    }

    pub fn favorite_dialog_title_rename(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入りフォルダのリネーム",
            Lang::English  => "Rename Favorite Folder",
            Lang::Chinese  => "重命名收藏夹",
        }
    }

    pub fn favorite_dialog_prompt(self) -> &'static str {
        match self {
            Lang::Japanese => "新しいお気に入りフォルダ名を設定してください",
            Lang::English  => "Enter a name for this favorite folder",
            Lang::Chinese  => "请输入收藏夹名称",
        }
    }

    pub fn favorite_dialog_marker_label(self) -> &'static str {
        match self {
            Lang::Japanese => "マーカー:",
            Lang::English  => "Marker:",
            Lang::Chinese  => "标记:",
        }
    }

    pub fn favorite_dialog_cancel(self) -> &'static str {
        match self {
            Lang::Japanese => "キャンセル",
            Lang::English  => "Cancel",
            Lang::Chinese  => "取消",
        }
    }

    pub fn favorite_dialog_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "決定",
            Lang::English  => "OK",
            Lang::Chinese  => "确定",
        }
    }

    pub fn favorite_error_name_empty(self) -> &'static str {
        match self {
            Lang::Japanese => "名前を入力してください",
            Lang::English  => "Please enter a name",
            Lang::Chinese  => "请输入名称",
        }
    }

    pub fn favorite_error_name_too_long(self) -> &'static str {
        match self {
            Lang::Japanese => "名前が長すぎます（200文字まで）",
            Lang::English  => "Name is too long (max 200 characters)",
            Lang::Chinese  => "名称过长（最多200个字符）",
        }
    }

    pub fn favorite_error_name_conflict(self) -> &'static str {
        match self {
            Lang::Japanese => "その名前はすでに使われています",
            Lang::English  => "That name is already in use",
            Lang::Chinese  => "该名称已被使用",
        }
    }

    pub fn favorite_error_limit_reached(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入りフォルダは200個までです",
            Lang::English  => "You can create up to 200 favorite folders",
            Lang::Chinese  => "收藏夹最多可创建200个",
        }
    }

    pub fn favorite_error_generic(self) -> &'static str {
        match self {
            Lang::Japanese => "処理に失敗しました",
            Lang::English  => "Operation failed",
            Lang::Chinese  => "操作失败",
        }
    }

    pub fn favorite_delete_confirm_title(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入りフォルダの削除",
            Lang::English  => "Delete Favorite Folder",
            Lang::Chinese  => "删除收藏夹",
        }
    }

    pub fn favorite_delete_confirm_body(self, name: &str) -> String {
        match self {
            Lang::Japanese => format!("「{name}」を削除しますか？\n所属するファイルの登録も解除されます"),
            Lang::English  => format!("Delete \"{name}\"?\nFiles assigned to it will be unassigned."),
            Lang::Chinese  => format!("确定要删除“{name}”吗？\n所属文件的收藏关系也会被解除"),
        }
    }

    pub fn favorite_delete_confirm_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "削除する",
            Lang::English  => "Delete",
            Lang::Chinese  => "删除",
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

    pub fn settings_exif_orientation_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ Exif Orientationによる自動回転",
            Lang::English  => "■ Auto-rotate via Exif Orientation",
            Lang::Chinese  => "■ 根据Exif Orientation自动旋转",
        }
    }

    pub fn settings_exif_orientation_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "OFFにすると、画像に埋め込まれたExif Orientationタグ（誤って付与されている場合を含む）を無視して表示する。ビューアーのみに効き、サムネイルには影響しない。",
            Lang::English  => "When off, the Exif Orientation tag embedded in images (including incorrectly-tagged ones) is ignored when displaying. Affects the viewer only, not thumbnails.",
            Lang::Chinese  => "关闭后，显示时将忽略图像内嵌的Exif Orientation标签（包括错误标签）。仅影响查看器，不影响缩略图。",
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

    pub fn settings_tab_translate(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳機能",
            Lang::English  => "Translate",
            Lang::Chinese  => "翻译功能",
        }
    }

    pub fn settings_translate_experimental_note(self) -> &'static str {
        match self {
            Lang::Japanese => "実験的機能: ローカルAI(Ollama/OpenWebUI等のOpenAI互換API)を利用したOCRテキスト抽出。クラウドAPIは未対応。",
            Lang::English  => "Experimental: OCR text extraction via a local AI (Ollama/OpenWebUI-style OpenAI-compatible API). Cloud APIs are not supported.",
            Lang::Chinese  => "实验性功能：通过本地AI(Ollama/OpenWebUI等OpenAI兼容API)进行OCR文本提取。暂不支持云端API。",
        }
    }

    pub fn settings_translate_url_label(self) -> &'static str {
        match self {
            Lang::Japanese => "APIベースURL",
            Lang::English  => "API base URL",
            Lang::Chinese  => "API基础URL",
        }
    }

    pub fn settings_translate_model_label(self) -> &'static str {
        match self {
            Lang::Japanese => "モデル名",
            Lang::English  => "Model name",
            Lang::Chinese  => "模型名称",
        }
    }

    pub fn settings_translate_test_button(self) -> &'static str {
        match self {
            Lang::Japanese => "接続テスト",
            Lang::English  => "Test connection",
            Lang::Chinese  => "连接测试",
        }
    }

    pub fn settings_translate_testing(self) -> &'static str {
        match self {
            Lang::Japanese => "確認中…",
            Lang::English  => "Checking…",
            Lang::Chinese  => "确认中…",
        }
    }

    pub fn settings_translate_overlay_width_label(self) -> &'static str {
        match self {
            Lang::Japanese => "オーバーレイ横幅",
            Lang::English  => "Overlay width",
            Lang::Chinese  => "浮层宽度",
        }
    }

    pub fn settings_translate_overlay_corner_label(self) -> &'static str {
        match self {
            Lang::Japanese => "オーバーレイ配置(四隅)",
            Lang::English  => "Overlay position (corner)",
            Lang::Chinese  => "浮层位置(四角)",
        }
    }

    pub fn settings_translate_corner_top_left(self) -> &'static str {
        match self {
            Lang::Japanese => "左上",
            Lang::English  => "Top-left",
            Lang::Chinese  => "左上",
        }
    }

    pub fn settings_translate_corner_top_right(self) -> &'static str {
        match self {
            Lang::Japanese => "右上",
            Lang::English  => "Top-right",
            Lang::Chinese  => "右上",
        }
    }

    pub fn settings_translate_corner_bottom_left(self) -> &'static str {
        match self {
            Lang::Japanese => "左下",
            Lang::English  => "Bottom-left",
            Lang::Chinese  => "左下",
        }
    }

    pub fn settings_translate_corner_bottom_right(self) -> &'static str {
        match self {
            Lang::Japanese => "右下",
            Lang::English  => "Bottom-right",
            Lang::Chinese  => "右下",
        }
    }

    pub fn translate_overlay_title(self) -> &'static str {
        match self {
            Lang::Japanese => "OCR(実験的)",
            Lang::English  => "OCR (experimental)",
            Lang::Chinese  => "OCR(实验性)",
        }
    }

    pub fn translate_overlay_run_button(self) -> &'static str {
        match self {
            Lang::Japanese => "実行",
            Lang::English  => "Run",
            Lang::Chinese  => "运行",
        }
    }

    pub fn translate_overlay_open_folder_button(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダを開く",
            Lang::English  => "Open folder",
            Lang::Chinese  => "打开文件夹",
        }
    }

    pub fn translate_overlay_copy_button(self) -> &'static str {
        match self {
            Lang::Japanese => "コピー",
            Lang::English  => "Copy",
            Lang::Chinese  => "复制",
        }
    }

    pub fn translate_overlay_running(self) -> &'static str {
        match self {
            Lang::Japanese => "解析中…（モデル未ロード時は数十秒以上かかることがあります）",
            Lang::English  => "Analyzing… (can take a while on first run if the model needs to load)",
            Lang::Chinese  => "分析中…（模型首次加载时可能需要较长时间）",
        }
    }

    pub fn translate_overlay_empty(self) -> &'static str {
        match self {
            Lang::Japanese => "(未実行)",
            Lang::English  => "(not run yet)",
            Lang::Chinese  => "(尚未运行)",
        }
    }

    pub fn translate_overlay_failed_prefix(self) -> &'static str {
        match self {
            Lang::Japanese => "失敗",
            Lang::English  => "Failed",
            Lang::Chinese  => "失败",
        }
    }

    pub fn translate_overlay_fallback_notice(self) -> &'static str {
        match self {
            Lang::Japanese => "形式解析に失敗、簡易表示です",
            Lang::English  => "Structured parse failed; showing raw fallback",
            Lang::Chinese  => "结构化解析失败，显示为简易结果",
        }
    }

    pub fn translate_overlay_model_missing(self) -> &'static str {
        match self {
            Lang::Japanese => "モデル名が未設定です",
            Lang::English  => "Model name is not set",
            Lang::Chinese  => "尚未设置模型名称",
        }
    }

    pub fn translate_overlay_no_page(self) -> &'static str {
        match self {
            Lang::Japanese => "ページ画像を取得できませんでした",
            Lang::English  => "Could not read the current page image",
            Lang::Chinese  => "无法获取当前页面图像",
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
