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

    /// Windowsエクスプローラーの右クリックメニューに表示するNekoviewer起動項目のラベル
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn windows_context_menu_label(self) -> &'static str {
        match self {
            Lang::Japanese => "Nekoviewerで開く",
            Lang::English  => "Open with Nekoviewer",
            Lang::Chinese  => "用Nekoviewer打开",
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

    pub fn card_info_off(self) -> &'static str {
        match self {
            Lang::Japanese => "情報:OFF",
            Lang::English  => "Info: Off",
            Lang::Chinese  => "信息:关",
        }
    }

    pub fn card_info_name(self) -> &'static str {
        match self {
            Lang::Japanese => "情報:名前",
            Lang::English  => "Info: Name",
            Lang::Chinese  => "信息:名称",
        }
    }

    pub fn card_info_name_date(self) -> &'static str {
        match self {
            Lang::Japanese => "情報:名前+日付",
            Lang::English  => "Info: Name+Date",
            Lang::Chinese  => "信息:名称+日期",
        }
    }

    pub fn card_info_name_date_size(self) -> &'static str {
        match self {
            Lang::Japanese => "情報:名前+日付+容量",
            Lang::English  => "Info: Name+Date+Size",
            Lang::Chinese  => "信息:名称+日期+大小",
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

    /// ツールバーの[翻訳]ボタン(OCR/翻訳子ウィンドウの開閉トグル)。英語のみ幅を抑えて[Tr.]。
    pub fn toolbar_translate_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳",
            Lang::English  => "[Tr.]",
            Lang::Chinese  => "翻译",
        }
    }

    /// 上記ボタンのホバーツールチップ。押せない場合は理由を説明する。
    pub fn toolbar_translate_toggle_tip(self, enabled: bool) -> &'static str {
        match (self, enabled) {
            (Lang::Japanese, true)  => "OCR/翻訳ウィンドウの表示切替",
            (Lang::Japanese, false) => "設定タブで疎通確認と翻訳モデルの選択が必要です",
            (Lang::English, true)   => "Toggle the OCR/translation window",
            (Lang::English, false)  => "Test the connection and select a translation model in Settings first",
            (Lang::Chinese, true)   => "切换OCR/翻译窗口显示",
            (Lang::Chinese, false)  => "请先在设置中测试连接并选择翻译模型",
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

    pub fn sort_save_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "現在のソート条件を保存する",
            Lang::English  => "Save current sort settings",
            Lang::Chinese  => "保存当前排序条件",
        }
    }

    pub fn sort_save_new_label(self) -> &'static str {
        match self {
            Lang::Japanese => "新しい保存値",
            Lang::English  => "New saved value",
            Lang::Chinese  => "新的保存值",
        }
    }

    pub fn sort_save_changed_label(self) -> &'static str {
        match self {
            Lang::Japanese => "変更あり",
            Lang::English  => "changed",
            Lang::Chinese  => "有更改",
        }
    }

    pub fn bookmark_save_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "しおりを保存する",
            Lang::English  => "Save bookmark",
            Lang::Chinese  => "保存书签",
        }
    }

    pub fn thumbnail_register_page_label(self) -> &'static str {
        match self {
            Lang::Japanese => "このページをサムネイルとして登録",
            Lang::English  => "Use this page as the thumbnail",
            Lang::Chinese  => "将此页设为缩略图",
        }
    }

    pub fn thumbnail_register_left_half_label(self) -> &'static str {
        match self {
            Lang::Japanese => "表示画像の左側をサムネイル登録",
            Lang::English  => "Use the left half of the displayed image",
            Lang::Chinese  => "将显示图像的左半部分设为缩略图",
        }
    }

    pub fn thumbnail_register_right_half_label(self) -> &'static str {
        match self {
            Lang::Japanese => "表示画像の右側をサムネイル登録",
            Lang::English  => "Use the right half of the displayed image",
            Lang::Chinese  => "将显示图像的右半部分设为缩略图",
        }
    }

    pub fn thumbnail_left_generated_label(self) -> &'static str {
        match self {
            Lang::Japanese => "左側生成画像",
            Lang::English  => "generated from left side",
            Lang::Chinese  => "左侧生成图像",
        }
    }

    pub fn thumbnail_right_generated_label(self) -> &'static str {
        match self {
            Lang::Japanese => "右側生成画像",
            Lang::English  => "generated from right side",
            Lang::Chinese  => "右侧生成图像",
        }
    }

    pub fn thumbnail_current_label(self) -> &'static str {
        match self {
            Lang::Japanese => "現在のサムネイル",
            Lang::English  => "Current thumbnail",
            Lang::Chinese  => "当前缩略图",
        }
    }

    pub fn thumbnail_default_label(self) -> &'static str {
        match self {
            Lang::Japanese => "デフォルト",
            Lang::English  => "Default",
            Lang::Chinese  => "默认",
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

    /// ビューアー右クリックメニュー「スライドショー」チェックボックス。
    /// チェック済み = 実行中。文言自体は状態に関わらず固定（チェック状態で表現する）。
    pub fn slideshow_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "スライドショー",
            Lang::English  => "Slideshow",
            Lang::Chinese  => "幻灯片放映",
        }
    }

    /// ビューアー右クリックメニュー「お気に入りに追加」。ダイアログを介さず、
    /// 現在のアーカイブ（生ファイル表示中はそのファイル自身）を未整理のお気に入りへ
    /// 即登録するワンアクション項目。フォルダ選択等の詳細設定はエクスプローラー部の
    /// [`favorite_detail_menu`](Self::favorite_detail_menu) に委ねる。
    pub fn favorite_quick_add_label(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入りに追加",
            Lang::English  => "Add to Favorites",
            Lang::Chinese  => "添加到收藏",
        }
    }

    /// favorite_quick_add_label 実行後のトースト: 未整理のお気に入りへ新規登録できた。
    pub fn favorite_quick_add_toast_success(self) -> &'static str {
        match self {
            Lang::Japanese => "現在のアーカイブを未分類のお気に入りフォルダに追加しました",
            Lang::English  => "Added the current archive to the unsorted favorites folder",
            Lang::Chinese  => "已将当前压缩包添加到未分类收藏夹",
        }
    }

    /// favorite_quick_add_label 実行時のトースト: 既にお気に入り登録済み（フォルダ割当済み含む）だった。
    pub fn favorite_quick_add_toast_already(self) -> &'static str {
        match self {
            Lang::Japanese => "既にお気に入りフォルダに登録済みです",
            Lang::English  => "Already registered in a favorites folder",
            Lang::Chinese  => "已在收藏夹中登记",
        }
    }

    /// favorite_quick_add_label 実行時のトースト: DB未接続等で登録できなかった。
    pub fn favorite_quick_add_toast_error(self) -> &'static str {
        match self {
            Lang::Japanese => "お気に入り登録に失敗しました",
            Lang::English  => "Failed to add to favorites",
            Lang::Chinese  => "添加收藏失败",
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

    /// エクスプローラー部アイテムカード右クリックメニュー「フォルダを開く」
    /// （OS標準ファイラーで現在表示中ディレクトリを開く）
    pub fn explorer_open_folder_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダを開く",
            Lang::English  => "Open Folder",
            Lang::Chinese  => "打开文件夹",
        }
    }

    pub fn favorite_detail_common_only_note(self) -> &'static str {
        match self {
            Lang::Japanese => "※共通のお気に入り以外は省略しています",
            Lang::English  => "* Folders not shared by all selected files are omitted",
            Lang::Chinese  => "※未显示所选文件不共有的收藏夹",
        }
    }

    /// エクスプローラー部アイテムカード右クリックメニュー「ソート条件」（単一選択時）
    pub fn sort_condition_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "ソート条件...",
            Lang::English  => "Sort Condition...",
            Lang::Chinese  => "排序条件...",
        }
    }

    pub fn sort_condition_menu_bulk(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("ソート条件... ({count}件)"),
            Lang::English  => format!("Sort Condition... ({count} items)"),
            Lang::Chinese  => format!("排序条件...（{count} 项）"),
        }
    }

    pub fn sort_condition_dialog_title(self) -> &'static str {
        match self {
            Lang::Japanese => "ソート条件の変更",
            Lang::English  => "Change Sort Condition",
            Lang::Chinese  => "更改排序条件",
        }
    }

    /// エクスプローラー部アイテムカード右クリックメニュー「しおり保存」（単一選択時）
    pub fn bookmark_setting_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "しおり保存...",
            Lang::English  => "Bookmark Setting...",
            Lang::Chinese  => "书签设置...",
        }
    }

    pub fn bookmark_setting_menu_bulk(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("しおり保存... ({count}件)"),
            Lang::English  => format!("Bookmark Setting... ({count} items)"),
            Lang::Chinese  => format!("书签设置...（{count} 项）"),
        }
    }

    pub fn bookmark_setting_dialog_title(self) -> &'static str {
        match self {
            Lang::Japanese => "しおり保存設定の変更",
            Lang::English  => "Change Bookmark Setting",
            Lang::Chinese  => "更改书签设置",
        }
    }

    /// エクスプローラー部アイテムカード右クリックメニュー「見開き設定」（単一選択時）
    pub fn spread_setting_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "見開き設定...",
            Lang::English  => "Spread Setting...",
            Lang::Chinese  => "双页设置...",
        }
    }

    pub fn spread_setting_menu_bulk(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("見開き設定... ({count}件)"),
            Lang::English  => format!("Spread Setting... ({count} items)"),
            Lang::Chinese  => format!("双页设置...（{count} 项）"),
        }
    }

    pub fn spread_setting_dialog_title(self) -> &'static str {
        match self {
            Lang::Japanese => "見開き設定の変更",
            Lang::English  => "Change Spread Setting",
            Lang::Chinese  => "更改双页设置",
        }
    }

    pub fn spread_mode_single_label(self) -> &'static str {
        match self {
            Lang::Japanese => "単ページ",
            Lang::English  => "Single Page",
            Lang::Chinese  => "单页",
        }
    }

    pub fn spread_mode_right_label(self) -> &'static str {
        match self {
            Lang::Japanese => "右綴じ",
            Lang::English  => "Right Bind",
            Lang::Chinese  => "右装订",
        }
    }

    pub fn spread_mode_left_label(self) -> &'static str {
        match self {
            Lang::Japanese => "左綴じ",
            Lang::English  => "Left Bind",
            Lang::Chinese  => "左装订",
        }
    }

    pub fn spread_offset_virtual_first_label(self) -> &'static str {
        match self {
            Lang::Japanese => "1ページ目を単ページとして開く",
            Lang::English  => "Open page 1 alone",
            Lang::Chinese  => "第1页单独显示",
        }
    }

    pub fn spread_offset_no_virtual_label(self) -> &'static str {
        match self {
            Lang::Japanese => "最初から見開きページとして開く",
            Lang::English  => "Pair pages from page 1",
            Lang::Chinese  => "从第1页开始双页显示",
        }
    }

    /// 一括設定変更ダイアログ（ソート条件/しおり保存/見開き設定）共通の反映ボタン
    pub fn bulk_setting_apply_button(self) -> &'static str {
        match self {
            Lang::Japanese => "反映",
            Lang::English  => "Apply",
            Lang::Chinese  => "应用",
        }
    }

    /// 一括設定変更: 右クリック対象を対象外フィルタ後、0件になった時のトースト
    pub fn bulk_setting_no_target_toast(self) -> &'static str {
        match self {
            Lang::Japanese => "対象となるファイルがありません",
            Lang::English  => "No files are eligible for this setting",
            Lang::Chinese  => "没有符合条件的文件",
        }
    }

    /// 一括設定変更: 反映完了後、正常終了件数のトースト1行目
    pub fn bulk_setting_success_toast(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("{count}件のファイルに対して設定を行いました"),
            Lang::English  => format!("Applied the setting to {count} file(s)"),
            Lang::Chinese  => format!("已对 {count} 个文件应用设置"),
        }
    }

    /// 一括設定変更: 反映に失敗したファイル1件ごとのトースト行
    pub fn bulk_setting_failure_toast(self, name: &str) -> String {
        match self {
            Lang::Japanese => format!("ファイル名:{name} において設定が反映できませんでした"),
            Lang::English  => format!("Failed to apply the setting to: {name}"),
            Lang::Chinese  => format!("无法对以下文件应用设置：{name}"),
        }
    }

    /// 一括設定変更: 異常件数が表示上限を超えた時の集約行（11行目）
    pub fn bulk_setting_failure_overflow_toast(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("他{count}件で異常終了"),
            Lang::English  => format!("and {count} more failed"),
            Lang::Chinese  => format!("另有 {count} 个文件失败"),
        }
    }

    pub fn file_detail_menu(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル詳細",
            Lang::English  => "File Details...",
            Lang::Chinese  => "文件详细信息",
        }
    }

    pub fn file_detail_dialog_title(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル詳細",
            Lang::English  => "File Details",
            Lang::Chinese  => "文件详细信息",
        }
    }

    pub fn file_detail_entry_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル名:",
            Lang::English  => "File:",
            Lang::Chinese  => "文件名:",
        }
    }

    pub fn file_detail_entry_right_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル名（右ページ）:",
            Lang::English  => "File (right page):",
            Lang::Chinese  => "文件名（右页）:",
        }
    }

    pub fn file_detail_entry_left_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル名（左ページ）:",
            Lang::English  => "File (left page):",
            Lang::Chinese  => "文件名（左页）:",
        }
    }

    pub fn file_detail_archive_label(self) -> &'static str {
        match self {
            Lang::Japanese => "アーカイブファイル名:",
            Lang::English  => "Archive:",
            Lang::Chinese  => "压缩包文件名:",
        }
    }

    pub fn file_detail_close(self) -> &'static str {
        match self {
            Lang::Japanese => "閉じる",
            Lang::English  => "Close",
            Lang::Chinese  => "关闭",
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

    pub fn folder_tab_search(self) -> &'static str {
        match self {
            Lang::Japanese => "検索",
            Lang::English  => "Search",
            Lang::Chinese  => "搜索",
        }
    }

    pub fn folder_tab_virtual(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダ",
            Lang::English  => "Virtual Folders",
            Lang::Chinese  => "虚拟文件夹",
        }
    }

    pub fn virtual_real_tree_title(self) -> &'static str {
        match self {
            Lang::Japanese => "実ツリー",
            Lang::English  => "Real Tree",
            Lang::Chinese  => "实际目录树",
        }
    }

    pub fn virtual_grip_open(self) -> &'static str {
        match self {
            Lang::Japanese => "実ツリーを開く",
            Lang::English  => "Open real tree",
            Lang::Chinese  => "打开实际目录树",
        }
    }

    pub fn virtual_grip_close(self) -> &'static str {
        match self {
            Lang::Japanese => "実ツリーを閉じる",
            Lang::English  => "Close real tree",
            Lang::Chinese  => "关闭实际目录树",
        }
    }

    pub fn virtual_menu_register(self) -> &'static str {
        match self {
            Lang::Japanese => "実フォルダ登録",
            Lang::English  => "Register Real Folder",
            Lang::Chinese  => "登记实际文件夹",
        }
    }

    pub fn virtual_menu_delete(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダ削除",
            Lang::English  => "Delete Virtual Folder",
            Lang::Chinese  => "删除虚拟文件夹",
        }
    }

    pub fn virtual_menu_add_from_real(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダに追加する",
            Lang::English  => "Add to Virtual Folder",
            Lang::Chinese  => "添加到虚拟文件夹",
        }
    }

    pub fn virtual_picker_title_real(self) -> &'static str {
        match self {
            Lang::Japanese => "登録したいフォルダをダブルクリックで確定",
            Lang::English  => "Double-click the folder to register",
            Lang::Chinese  => "双击要登记的文件夹以确认",
        }
    }

    pub fn virtual_picker_title_dest(self) -> &'static str {
        match self {
            Lang::Japanese => "追加先の仮想フォルダをダブルクリックで確定",
            Lang::English  => "Double-click the destination virtual folder",
            Lang::Chinese  => "双击目标虚拟文件夹以确认",
        }
    }

    pub fn virtual_confirm_title(self) -> &'static str {
        match self {
            Lang::Japanese => "登録の確認",
            Lang::English  => "Confirm Registration",
            Lang::Chinese  => "确认登记",
        }
    }

    pub fn virtual_confirm_body(self) -> &'static str {
        match self {
            Lang::Japanese => "次のフォルダを仮想フォルダに登録します",
            Lang::English  => "The following folder will be registered as a virtual folder",
            Lang::Chinese  => "将把以下文件夹登记为虚拟文件夹",
        }
    }

    pub fn virtual_confirm_path(self, path: &str) -> String {
        match self {
            Lang::Japanese => format!("登録パス: {path}"),
            Lang::English  => format!("Path: {path}"),
            Lang::Chinese  => format!("登记路径：{path}"),
        }
    }

    pub fn virtual_confirm_dest(self, dest: &str) -> String {
        match self {
            Lang::Japanese => format!("登録先: {dest}"),
            Lang::English  => format!("Destination: {dest}"),
            Lang::Chinese  => format!("登记位置：{dest}"),
        }
    }

    pub fn virtual_delete_title(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダの削除",
            Lang::English  => "Delete Virtual Folder",
            Lang::Chinese  => "删除虚拟文件夹",
        }
    }

    pub fn virtual_delete_path(self, path: &str) -> String {
        match self {
            Lang::Japanese => format!("削除する仮想パス: {path}"),
            Lang::English  => format!("Virtual path to delete: {path}"),
            Lang::Chinese  => format!("要删除的虚拟路径：{path}"),
        }
    }

    pub fn virtual_delete_descendants(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("配下 {count} 件のフォルダも削除されます"),
            Lang::English  => format!("{count} sub-folder(s) will also be deleted"),
            Lang::Chinese  => format!("其下 {count} 个文件夹也将被删除"),
        }
    }

    pub fn virtual_delete_real_untouched(self) -> &'static str {
        match self {
            Lang::Japanese => "実フォルダには影響しません",
            Lang::English  => "Real folders are not affected",
            Lang::Chinese  => "不会影响实际文件夹",
        }
    }

    pub fn virtual_link_broken_label(self) -> &'static str {
        match self {
            Lang::Japanese => "（リンク切れ）",
            Lang::English  => "(broken link)",
            Lang::Chinese  => "（链接失效）",
        }
    }

    pub fn virtual_register_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダに正常に登録されました",
            Lang::English  => "Registered to the virtual folder",
            Lang::Chinese  => "已成功登记到虚拟文件夹",
        }
    }

    pub fn virtual_register_progress(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダに登録中…",
            Lang::English  => "Registering to the virtual folder…",
            Lang::Chinese  => "正在登记到虚拟文件夹…",
        }
    }

    pub fn virtual_register_failed(self, reason: &str) -> String {
        match self {
            Lang::Japanese => format!("仮想フォルダへの登録に失敗しました（{reason}）"),
            Lang::English  => format!("Failed to register to the virtual folder ({reason})"),
            Lang::Chinese  => format!("登记到虚拟文件夹失败（{reason}）"),
        }
    }

    pub fn virtual_reason_unreachable(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダが見つかりません、または接続できません",
            Lang::English  => "Folder not found or unreachable",
            Lang::Chinese  => "找不到文件夹或无法连接",
        }
    }

    pub fn virtual_reason_not_dir(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダではありません",
            Lang::English  => "Not a folder",
            Lang::Chinese  => "不是文件夹",
        }
    }

    pub fn virtual_reason_unreadable(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダを読み取れません",
            Lang::English  => "Cannot read the folder",
            Lang::Chinese  => "无法读取该文件夹",
        }
    }

    pub fn virtual_reason_too_large(self) -> &'static str {
        match self {
            Lang::Japanese => "サブフォルダが多すぎます",
            Lang::English  => "Too many subfolders",
            Lang::Chinese  => "子文件夹过多",
        }
    }

    pub fn virtual_reason_duplicate(self) -> &'static str {
        match self {
            Lang::Japanese => "同じ登録先に同じ実フォルダが既に登録されています",
            Lang::English  => "The same real folder is already registered at this destination",
            Lang::Chinese  => "该位置已登记同一实际文件夹",
        }
    }

    pub fn virtual_reason_limit(self, max: usize) -> String {
        match self {
            Lang::Japanese => format!("登録できる上限（{max}件）を超えます"),
            Lang::English  => format!("Exceeds the registration limit ({max})"),
            Lang::Chinese  => format!("超过可登记上限（{max} 个）"),
        }
    }

    pub fn virtual_reason_dest_missing(self) -> &'static str {
        match self {
            Lang::Japanese => "登録先の仮想フォルダが見つかりません",
            Lang::English  => "Destination virtual folder not found",
            Lang::Chinese  => "找不到目标虚拟文件夹",
        }
    }

    pub fn virtual_reason_name_invalid(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダ名が長すぎる、または空です",
            Lang::English  => "Folder name is too long or empty",
            Lang::Chinese  => "文件夹名称过长或为空",
        }
    }

    pub fn virtual_reason_db(self) -> &'static str {
        match self {
            Lang::Japanese => "データベースエラー",
            Lang::English  => "Database error",
            Lang::Chinese  => "数据库错误",
        }
    }

    pub fn virtual_overlap_same_here(self) -> &'static str {
        match self {
            Lang::Japanese => "この登録先には同じ実フォルダが既に登録されています（登録に失敗します）",
            Lang::English  => "This destination already has the same real folder (registration will fail)",
            Lang::Chinese  => "该位置已登记同一实际文件夹（登记将失败）",
        }
    }

    pub fn virtual_overlap_same(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("同じ実フォルダが別の場所に {count} 件登録済みです"),
            Lang::English  => format!("The same real folder is already registered in {count} other place(s)"),
            Lang::Chinese  => format!("同一实际文件夹已在其他 {count} 处登记"),
        }
    }

    pub fn virtual_overlap_ancestor(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("登録済みフォルダ {count} 件の配下にあたります"),
            Lang::English  => format!("It lies inside {count} registered folder(s)"),
            Lang::Chinese  => format!("位于 {count} 个已登记文件夹之内"),
        }
    }

    pub fn virtual_overlap_descendant(self, count: usize) -> String {
        match self {
            Lang::Japanese => format!("登録済みフォルダ {count} 件を含みます"),
            Lang::English  => format!("It contains {count} registered folder(s)"),
            Lang::Chinese  => format!("包含 {count} 个已登记文件夹"),
        }
    }

    pub fn virtual_delete_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "仮想フォルダを削除しました",
            Lang::English  => "Virtual folder deleted",
            Lang::Chinese  => "已删除虚拟文件夹",
        }
    }

    pub fn virtual_delete_failed(self, reason: &str) -> String {
        match self {
            Lang::Japanese => format!("仮想フォルダの削除に失敗しました（{reason}）"),
            Lang::English  => format!("Failed to delete the virtual folder ({reason})"),
            Lang::Chinese  => format!("删除虚拟文件夹失败（{reason}）"),
        }
    }

    pub fn virtual_delete_reason_changed(self) -> &'static str {
        match self {
            Lang::Japanese => "対象のフォルダが変更されています",
            Lang::English  => "The target folder has changed",
            Lang::Chinese  => "目标文件夹已发生变化",
        }
    }

    pub fn virtual_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "OK",
            Lang::English  => "OK",
            Lang::Chinese  => "确定",
        }
    }

    pub fn search_start_button(self) -> &'static str {
        match self {
            Lang::Japanese => "検索開始",
            Lang::English  => "Search",
            Lang::Chinese  => "开始搜索",
        }
    }

    pub fn search_clear_button(self) -> &'static str {
        match self {
            Lang::Japanese => "条件クリア",
            Lang::English  => "Clear",
            Lang::Chinese  => "清除条件",
        }
    }

    pub fn search_base_dir_label(self) -> &'static str {
        match self {
            Lang::Japanese => "検索基底フォルダ",
            Lang::English  => "Search base folder",
            Lang::Chinese  => "搜索根目录",
        }
    }

    pub fn search_name_pattern_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ファイル名",
            Lang::English  => "Name",
            Lang::Chinese  => "文件名",
        }
    }

    pub fn search_include_subdirs_label(self) -> &'static str {
        match self {
            Lang::Japanese => "サブディレクトリを含む",
            Lang::English  => "Include subdirectories",
            Lang::Chinese  => "包含子目录",
        }
    }

    pub fn search_size_min_label(self) -> &'static str {
        match self {
            Lang::Japanese => "サイズ以上(MB)",
            Lang::English  => "Size ≥ (MB)",
            Lang::Chinese  => "大小 ≥ (MB)",
        }
    }

    pub fn search_size_max_label(self) -> &'static str {
        match self {
            Lang::Japanese => "サイズ以下(MB)",
            Lang::English  => "Size ≤ (MB)",
            Lang::Chinese  => "大小 ≤ (MB)",
        }
    }

    pub fn search_date_after_label(self) -> &'static str {
        match self {
            Lang::Japanese => "日付以降(YYYY-MM-DD)",
            Lang::English  => "After (YYYY-MM-DD)",
            Lang::Chinese  => "此日期之后 (YYYY-MM-DD)",
        }
    }

    pub fn search_date_before_label(self) -> &'static str {
        match self {
            Lang::Japanese => "日付以前(YYYY-MM-DD)",
            Lang::English  => "Before (YYYY-MM-DD)",
            Lang::Chinese  => "此日期之前 (YYYY-MM-DD)",
        }
    }

    /// 検索結果リストの1行ラベル（検索ファイル名/パターンが表示できるだけの文字数で見える）。
    pub fn search_result_label(self, pattern: &str, count: usize) -> String {
        let name = if pattern.trim().is_empty() {
            match self {
                Lang::Japanese => "(全ファイル)".to_string(),
                Lang::English  => "(all files)".to_string(),
                Lang::Chinese  => "(所有文件)".to_string(),
            }
        } else {
            pattern.to_string()
        };
        match self {
            Lang::Japanese => format!("{name} ({count}件)"),
            Lang::English  => format!("{name} ({count} items)"),
            Lang::Chinese  => format!("{name} ({count}项)"),
        }
    }

    /// 検索タブを開いていて、まだどの検索結果も選択していない間のアイテムペイン表示。
    pub fn search_select_result_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "検索結果を選択してください",
            Lang::English  => "Select a search result",
            Lang::Chinese  => "请选择搜索结果",
        }
    }

    pub fn search_no_results_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "検索結果はまだありません",
            Lang::English  => "No search results yet",
            Lang::Chinese  => "暂无搜索结果",
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

    pub fn toast_bookmark_restored(self) -> &'static str {
        match self {
            Lang::Japanese => "前回閉じたページから復帰します",
            Lang::English  => "Resuming from where you last left off",
            Lang::Chinese  => "从上次关闭的页面继续",
        }
    }

    pub fn toast_bookmark_invalidated(self) -> &'static str {
        match self {
            Lang::Japanese => "しおりが無効になりました",
            Lang::English  => "The bookmark is no longer valid",
            Lang::Chinese  => "书签已失效",
        }
    }

    pub fn viewer_fallback(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューア",
            Lang::English  => "Viewer",
            Lang::Chinese  => "查看器",
        }
    }

    pub fn thumbnail_status(
        self,
        current: usize,
        total: usize,
        errors: usize,
        replacing_old: bool,
    ) -> String {
        let mut text = match self {
            Lang::Japanese => format!("サムネイル {current}/{total}"),
            Lang::English  => format!("Thumbnails {current}/{total}"),
            Lang::Chinese  => format!("缩略图 {current}/{total}"),
        };
        if errors > 0 {
            match self {
                Lang::Japanese => text.push_str(&format!(" エラー {errors}")),
                Lang::English  => text.push_str(&format!(" Errors {errors}")),
                Lang::Chinese  => text.push_str(&format!(" 错误 {errors}")),
            }
        }
        if replacing_old {
            text.push_str(match self {
                Lang::Japanese => " 新形式に更新中",
                Lang::English  => " Updating to the new format",
                Lang::Chinese  => " 正在更新为新格式",
            });
        }
        text
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

    /// アーカイブオープン中オーバーレイ: フォーマット未確定（起動直後、最初の進捗コールバック前）の文言。
    pub fn archive_open_progress_starting(self) -> &'static str {
        match self {
            Lang::Japanese => "読み込み中…",
            Lang::English  => "Loading…",
            Lang::Chinese  => "正在读取…",
        }
    }

    /// アーカイブオープン中オーバーレイ: 件数が判明している場合の進捗文言。
    pub fn archive_open_progress(self, current: usize, total: usize) -> String {
        let percent = if total == 0 { 0 } else { (current * 100) / total };
        match self {
            Lang::Japanese => format!("読み込み中… {percent}% ({current}/{total})"),
            Lang::English  => format!("Loading… {percent}% ({current}/{total})"),
            Lang::Chinese  => format!("正在读取… {percent}% ({current}/{total})"),
        }
    }

    /// アーカイブオープン中オーバーレイ: 件数不明（tar）の場合の進捗文言。
    pub fn archive_open_progress_indeterminate(self) -> &'static str {
        match self {
            Lang::Japanese => "tarファイル読み込み中…",
            Lang::English  => "Reading tar file…",
            Lang::Chinese  => "正在读取tar文件…",
        }
    }

    /// アーカイブオープン中オーバーレイ: メモリ見積もり（サンプル画像デコード）中の文言。
    pub fn archive_open_estimating(self) -> &'static str {
        match self {
            Lang::Japanese => "確認中…",
            Lang::English  => "Checking…",
            Lang::Chinese  => "正在检查…",
        }
    }

    pub fn archive_open_cancel(self) -> &'static str {
        match self {
            Lang::Japanese => "キャンセル",
            Lang::English  => "Cancel",
            Lang::Chinese  => "取消",
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

    pub fn tool_palette_toggle_button(self, on: bool) -> &'static str {
        match (self, on) {
            (Lang::Japanese, true)  => "ツールボックスON",
            (Lang::Japanese, false) => "ツールボックスOFF",
            (Lang::English, true)   => "Toolbox: ON",
            (Lang::English, false)  => "Toolbox: OFF",
            (Lang::Chinese, true)   => "工具箱：开",
            (Lang::Chinese, false)  => "工具箱：关",
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

    pub fn settings_tab_debug(self) -> &'static str {
        match self {
            Lang::Japanese => "デバッグ",
            Lang::English  => "Debug",
            Lang::Chinese  => "调试",
        }
    }

    pub fn settings_debug_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "通常は全てOFFのままで問題ありません。不具合報告時など、開発者に依頼された場合のみ有効にしてください。",
            Lang::English  => "Normally leave these all off. Enable them only when a developer asks you to, e.g. while reporting an issue.",
            Lang::Chinese  => "通常保持全部关闭即可。仅在开发者要求时（例如报告问题时）才启用。",
        }
    }

    pub fn settings_debug_log_perf(self) -> &'static str {
        match self {
            Lang::Japanese => "パフォーマンス計測ログ（ページ読み込み時間など）",
            Lang::English  => "Performance log (page load timing, etc.)",
            Lang::Chinese  => "性能测量日志（页面加载耗时等）",
        }
    }

    pub fn settings_debug_log_key(self) -> &'static str {
        match self {
            Lang::Japanese => "キーイベント・スクロールの入力ログ",
            Lang::English  => "Key/scroll input log",
            Lang::Chinese  => "按键与滚动输入日志",
        }
    }

    pub fn settings_debug_log_common(self) -> &'static str {
        match self {
            Lang::Japanese => "起動・初期化など共通ログ",
            Lang::English  => "Common log (startup/initialization, etc.)",
            Lang::Chinese  => "启动、初始化等通用日志",
        }
    }

    pub fn settings_decode_threads_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ページデコードの並列スレッド数",
            Lang::English  => "Parallel page-decode threads",
            Lang::Chinese  => "页面解码并行线程数",
        }
    }

    pub fn settings_decode_threads_manual_toggle(self) -> &'static str {
        match self {
            Lang::Japanese => "手動で指定する（既定は自動：論理コア数の半分）",
            Lang::English  => "Set manually (default: automatic, half the logical cores)",
            Lang::Chinese  => "手动指定（默认自动：逻辑核心数的一半）",
        }
    }

    pub fn settings_decode_threads_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "通常は自動のままで問題ありません。増やすとデコードは速くなりますがCPU/メモリ負荷も増えます。",
            Lang::English  => "Normally leave this automatic. Increasing it speeds up decoding but also raises CPU/memory load.",
            Lang::Chinese  => "通常保持自动即可。增大数值可加快解码，但会提高CPU/内存负载。",
        }
    }

    #[cfg(windows)]
    pub fn settings_tab_windows(self) -> &'static str {
        match self {
            Lang::Japanese => "Windows",
            Lang::English  => "Windows",
            Lang::Chinese  => "Windows",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_context_menu_label(self) -> &'static str {
        match self {
            Lang::Japanese => "エクスプローラーの右クリックメニュー",
            Lang::English  => "Explorer right-click menu",
            Lang::Chinese  => "资源管理器右键菜单",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_context_menu_desc(self) -> &'static str {
        match self {
            Lang::Japanese => "対応するファイル（画像/アーカイブ）とフォルダの右クリックメニューに「Nekoviewerで開く」を追加する。",
            Lang::English  => "Adds \"Open with Nekoviewer\" to the right-click menu for supported files (images/archives) and folders.",
            Lang::Chinese  => "在支持的文件（图片/压缩包）和文件夹的右键菜单中添加“用Nekoviewer打开”。",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_register_button(self) -> &'static str {
        match self {
            Lang::Japanese => "登録",
            Lang::English  => "Register",
            Lang::Chinese  => "注册",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_unregister_button(self) -> &'static str {
        match self {
            Lang::Japanese => "削除",
            Lang::English  => "Remove",
            Lang::Chinese  => "删除",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_status_registered(self) -> &'static str {
        match self {
            Lang::Japanese => "登録済み",
            Lang::English  => "Registered",
            Lang::Chinese  => "已注册",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_status_not_registered(self) -> &'static str {
        match self {
            Lang::Japanese => "未登録",
            Lang::English  => "Not registered",
            Lang::Chinese  => "未注册",
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_register_failed(self, detail: &str) -> String {
        match self {
            Lang::Japanese => format!("登録に失敗: {detail}"),
            Lang::English  => format!("Registration failed: {detail}"),
            Lang::Chinese  => format!("注册失败: {detail}"),
        }
    }

    #[cfg(windows)]
    pub fn settings_windows_unregister_failed(self, detail: &str) -> String {
        match self {
            Lang::Japanese => format!("削除に失敗: {detail}"),
            Lang::English  => format!("Removal failed: {detail}"),
            Lang::Chinese  => format!("删除失败: {detail}"),
        }
    }

    pub fn settings_tab_viewer(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューアー",
            Lang::English  => "Viewer",
            Lang::Chinese  => "查看器",
        }
    }

    pub fn settings_tab_slideshow(self) -> &'static str {
        match self {
            Lang::Japanese => "スライドショー",
            Lang::English  => "Slideshow",
            Lang::Chinese  => "幻灯片放映",
        }
    }

    /// スライドショータブの大項目見出し。通常時とスライドショー実行中でトランジション
    /// 設定（種類・遷移時間）を独立して選べるようセクション分けする。
    pub fn settings_slideshow_normal_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "通常時のトランジション設定",
            Lang::English  => "Normal transition settings",
            Lang::Chinese  => "平时转场设置",
        }
    }

    pub fn settings_slideshow_active_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "スライドショー時のトランジション設定",
            Lang::English  => "Slideshow transition settings",
            Lang::Chinese  => "幻灯片放映时转场设置",
        }
    }

    pub fn settings_transition_none(self) -> &'static str {
        match self {
            Lang::Japanese => "なし（即時切り替え）",
            Lang::English  => "None (instant)",
            Lang::Chinese  => "无（立即切换）",
        }
    }

    pub fn settings_transition_horizontal_slide(self) -> &'static str {
        match self {
            Lang::Japanese => "横スライド",
            Lang::English  => "Horizontal slide",
            Lang::Chinese  => "横向滑动",
        }
    }

    pub fn settings_transition_cross_fade(self) -> &'static str {
        match self {
            Lang::Japanese => "クロスフェード",
            Lang::English  => "Cross-fade",
            Lang::Chinese  => "交叉淡化",
        }
    }

    pub fn settings_transition_clockwise_wipe(self) -> &'static str {
        match self {
            Lang::Japanese => "時計回りワイプ",
            Lang::English  => "Clockwise wipe",
            Lang::Chinese  => "顺时针擦除",
        }
    }

    pub fn settings_transition_duration_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 遷移時間",
            Lang::English  => "■ Transition duration",
            Lang::Chinese  => "■ 转场时长",
        }
    }

    pub fn settings_transition_duration_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "トランジションが完了するまでの時間(ms)。全種類共通。",
            Lang::English  => "Time (ms) for the transition to complete. Shared by all transition types.",
            Lang::Chinese  => "转场完成所需的时间(ms)。所有类型共用。",
        }
    }

    pub fn settings_slideshow_interval_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ スライドショー間隔",
            Lang::English  => "■ Slideshow interval",
            Lang::Chinese  => "■ 幻灯片间隔",
        }
    }

    pub fn settings_slideshow_interval_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "自動でページ送りするまでの待機時間。",
            Lang::English  => "Time to wait before automatically turning to the next page.",
            Lang::Chinese  => "自动翻页前的等待时间。",
        }
    }

    pub fn settings_slideshow_manual_behavior_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 手動ページ送り時の挙動",
            Lang::English  => "■ On manual page turn",
            Lang::Chinese  => "■ 手动翻页时的行为",
        }
    }

    pub fn settings_slideshow_manual_behavior_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "スライドショー中にユーザーが手動でページを送った場合の扱い。",
            Lang::English  => "What happens when the user manually turns a page during a slideshow.",
            Lang::Chinese  => "幻灯片放映中用户手动翻页时的处理方式。",
        }
    }

    pub fn settings_slideshow_manual_behavior_reset(self) -> &'static str {
        match self {
            Lang::Japanese => "タイマーをリセットして\nスライドショーを継続させる",
            Lang::English  => "Reset the timer and\ncontinue the slideshow",
            Lang::Chinese  => "重置计时器\n继续幻灯片放映",
        }
    }

    pub fn settings_slideshow_manual_behavior_stop(self) -> &'static str {
        match self {
            Lang::Japanese => "手動操作がされた時点で\nスライドショーを停止させる",
            Lang::English  => "Stop the slideshow\non manual operation",
            Lang::Chinese  => "手动操作时\n停止幻灯片放映",
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

    pub fn settings_default_slot_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ビューアーを開くときの既定の位置・サイズ",
            Lang::English  => "Default position/size when opening the viewer",
            Lang::Chinese  => "打开查看器时的默认位置与大小",
        }
    }

    pub fn settings_default_slot_none(self) -> &'static str {
        match self {
            Lang::Japanese => "なし",
            Lang::English  => "None",
            Lang::Chinese  => "无",
        }
    }

    pub fn settings_default_slot_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "F5〜F8で保存した位置・サイズを、ビューアーを開くたびに既定として適用する。該当スロットが未保存の場合はデフォルト無しと同じ扱いになる。適用後でもF5〜F8でその回だけ別スロットへ切り替えられる。",
            Lang::English  => "Applies the position/size saved to F5-F8 as the default every time the viewer opens. If that slot isn't saved yet, it behaves as if none were selected. You can still switch to a different slot for just that session with F5-F8.",
            Lang::Chinese  => "每次打开查看器时，将F5~F8保存的位置与大小作为默认应用。若该槽位尚未保存，则视为未选择。应用后仍可通过F5~F8临时切换到其他槽位。",
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
            Lang::Japanese => "グリッド表示と新規生成に使うサムネイル長辺サイズ（px）。",
            Lang::English  => "Long-edge size (px) used for grid display and newly generated thumbnails.",
            Lang::Chinese  => "用于网格显示和新生成缩略图的长边尺寸（px）。",
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

    pub fn settings_anim_frame_hard_limit_label(self) -> &'static str {
        match self {
            Lang::Japanese => "アニメ1フレームあたりの生デコードサイズ上限",
            Lang::English  => "Per-frame raw decode size limit for animations",
            Lang::Chinese  => "动画单帧原始解码大小上限",
        }
    }

    pub fn settings_anim_frame_hard_limit_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "同一アニメ内で解像度が異常に大きいフレームに遭遇した際、そのフレームだけ縮小して再生を継続する。一般的な解像度（4K級まで）は約34MBに収まるため、通常は既定のままで問題ない。",
            Lang::English  => "If a frame in an animation has an unusually large resolution, only that frame is downscaled to keep playback going. Typical resolutions (up to 4K) fit within ~34MB, so the default is usually fine.",
            Lang::Chinese  => "当同一动画中出现分辨率异常大的帧时，仅缩小该帧以继续播放。常见分辨率（最高4K）约占34MB，通常保持默认值即可。",
        }
    }

    pub fn settings_tab_translate(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳機能",
            Lang::English  => "Translate",
            Lang::Chinese  => "翻译功能",
        }
    }

    pub fn settings_tab_explorer(self) -> &'static str {
        match self {
            Lang::Japanese => "エクスプローラー",
            Lang::English  => "Explorer",
            Lang::Chinese  => "资源管理器",
        }
    }

    pub fn settings_card_date_heading(self) -> &'static str {
        match self {
            Lang::Japanese => "サムネカードの日付表示",
            Lang::English  => "Thumbnail card date",
            Lang::Chinese  => "缩略图卡片的日期显示",
        }
    }

    pub fn settings_card_date_mode_label(self) -> &'static str {
        match self {
            Lang::Japanese => "モード",
            Lang::English  => "Mode",
            Lang::Chinese  => "模式",
        }
    }

    pub fn settings_card_date_mode_auto(self) -> &'static str {
        match self {
            Lang::Japanese => "自動（表示言語に従う）",
            Lang::English  => "Auto (follow UI language)",
            Lang::Chinese  => "自动（跟随界面语言）",
        }
    }

    pub fn settings_card_date_mode_sort(self) -> &'static str {
        match self {
            Lang::Japanese => "ソート基準（20260131）",
            Lang::English  => "Sort-friendly (20260131)",
            Lang::Chinese  => "排序优先（20260131）",
        }
    }

    pub fn settings_card_date_mode_custom(self) -> &'static str {
        match self {
            Lang::Japanese => "カスタム",
            Lang::English  => "Custom",
            Lang::Chinese  => "自定义",
        }
    }

    pub fn settings_card_date_auto_style_label(self) -> &'static str {
        match self {
            Lang::Japanese => "自動時の書式",
            Lang::English  => "Auto format",
            Lang::Chinese  => "自动模式的格式",
        }
    }

    pub fn settings_card_date_order_label(self) -> &'static str {
        match self {
            Lang::Japanese => "表示順",
            Lang::English  => "Order",
            Lang::Chinese  => "顺序",
        }
    }

    pub fn settings_card_date_sep_label(self) -> &'static str {
        match self {
            Lang::Japanese => "区切り文字",
            Lang::English  => "Separator",
            Lang::Chinese  => "分隔符",
        }
    }

    pub fn settings_card_date_year_label(self) -> &'static str {
        match self {
            Lang::Japanese => "年の桁",
            Lang::English  => "Year digits",
            Lang::Chinese  => "年份位数",
        }
    }

    pub fn settings_card_date_month_label(self) -> &'static str {
        match self {
            Lang::Japanese => "月の表記",
            Lang::English  => "Month style",
            Lang::Chinese  => "月份表示",
        }
    }

    pub fn settings_card_date_sep_none(self) -> &'static str {
        match self {
            Lang::Japanese => "なし",
            Lang::English  => "None",
            Lang::Chinese  => "无",
        }
    }

    pub fn settings_card_date_preview(self, example: &str) -> String {
        match self {
            Lang::Japanese => format!("例: {example}"),
            Lang::English  => format!("Example: {example}"),
            Lang::Chinese  => format!("示例：{example}"),
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

    /// 翻訳モデル選択(主)。OCRモデルは既定でこの値に追従する。
    pub fn settings_translate_translation_model_label(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳モデル",
            Lang::English  => "Translation model",
            Lang::Chinese  => "翻译模型",
        }
    }

    /// OCRモデル選択。未変更なら翻訳モデルに追従し、選ぶと以後は独立する。
    pub fn settings_translate_ocr_model_label(self) -> &'static str {
        match self {
            Lang::Japanese => "OCRモデル",
            Lang::English  => "OCR model",
            Lang::Chinese  => "OCR模型",
        }
    }

    pub fn settings_translate_model_unselected(self) -> &'static str {
        match self {
            Lang::Japanese => "(未選択)",
            Lang::English  => "(none selected)",
            Lang::Chinese  => "(未选择)",
        }
    }

    pub fn settings_translate_no_models_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "先に「モデル取得」を実行してください",
            Lang::English  => "Fetch the model list first",
            Lang::Chinese  => "请先执行「获取模型」",
        }
    }

    /// モデル一覧が未取得(ダイアログ限定の一時状態、開き直すたびに空になる)でも、
    /// 保存済みの選択値自体は消えていないことを示す表示。「設定が消えた」ように
    /// 見せないための保険（実際の値はtranslate_cfgに残ったまま）。
    pub fn settings_translate_current_model_label(self, model: &str) -> String {
        match self {
            Lang::Japanese => format!("現在の設定: {model}"),
            Lang::English  => format!("Current: {model}"),
            Lang::Chinese  => format!("当前设置: {model}"),
        }
    }

    pub fn settings_translate_test_button(self) -> &'static str {
        match self {
            Lang::Japanese => "モデル取得",
            Lang::English  => "Fetch models",
            Lang::Chinese  => "获取模型",
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

    pub fn translate_overlay_open_folder_button(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダを開く",
            Lang::English  => "Open folder",
            Lang::Chinese  => "打开文件夹",
        }
    }

    /// 子ウィンドウ左ペインの見出し(OCR原文)。
    pub fn translate_child_ocr_pane_title(self) -> &'static str {
        match self {
            Lang::Japanese => "OCR原文",
            Lang::English  => "OCR text",
            Lang::Chinese  => "OCR原文",
        }
    }

    /// 子ウィンドウ右ペインの見出し(翻訳結果)。
    pub fn translate_child_translation_pane_title(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳結果",
            Lang::English  => "Translation",
            Lang::Chinese  => "翻译结果",
        }
    }

    /// 子ウィンドウの[取得]ボタン(1P単位でOCRを実行)。
    pub fn translate_child_retry_button(self) -> &'static str {
        match self {
            Lang::Japanese => "取得",
            Lang::English  => "OCR",
            Lang::Chinese  => "识别",
        }
    }

    /// 子ウィンドウの[翻訳]ボタン。
    pub fn translate_child_retranslate_button(self) -> &'static str {
        match self {
            Lang::Japanese => "翻訳",
            Lang::English  => "Translate",
            Lang::Chinese  => "翻译",
        }
    }

    /// 子ウィンドウの最前面固定トグル。
    pub fn translate_child_always_on_top_toggle(self) -> &'static str {
        match self {
            Lang::Japanese => "最前面固定",
            Lang::English  => "Always on top",
            Lang::Chinese  => "始终置顶",
        }
    }

    /// 翻訳実行の絶対条件(OCR txt取得済み)を満たさない場合のフォールバックメッセージ。
    /// OCRと翻訳は完全に独立したボタン/処理であり、E2Eで自動連鎖はしない。
    pub fn translate_child_ocr_required(self) -> &'static str {
        match self {
            Lang::Japanese => "OCRを取得してから実行してください",
            Lang::English  => "Run OCR first before translating",
            Lang::Chinese  => "请先执行OCR后再翻译",
        }
    }

    /// 翻訳機能の言語ドロップダウン（原文/翻訳先共通）の表示名。
    pub fn translate_lang_label(self, lang: crate::translate::TranslateLang) -> &'static str {
        use crate::translate::TranslateLang;
        match (self, lang) {
            (Lang::Japanese, TranslateLang::Japanese)         => "日本語",
            (Lang::Japanese, TranslateLang::ChineseSimplified)  => "中国語(簡体字)",
            (Lang::Japanese, TranslateLang::ChineseTraditional) => "中国語(繁体字)",
            (Lang::Japanese, TranslateLang::English)            => "英語",
            (Lang::Japanese, TranslateLang::Korean)             => "韓国語",
            (Lang::English, TranslateLang::Japanese)          => "Japanese",
            (Lang::English, TranslateLang::ChineseSimplified)  => "Chinese (Simplified)",
            (Lang::English, TranslateLang::ChineseTraditional) => "Chinese (Traditional)",
            (Lang::English, TranslateLang::English)            => "English",
            (Lang::English, TranslateLang::Korean)             => "Korean",
            (Lang::Chinese, TranslateLang::Japanese)          => "日语",
            (Lang::Chinese, TranslateLang::ChineseSimplified)  => "简体中文",
            (Lang::Chinese, TranslateLang::ChineseTraditional) => "繁体中文",
            (Lang::Chinese, TranslateLang::English)            => "英语",
            (Lang::Chinese, TranslateLang::Korean)             => "韩语",
        }
    }

    /// 言語ドロップダウンが未設定状態のときの表示名。
    pub fn translate_lang_unset_label(self) -> &'static str {
        match self {
            Lang::Japanese => "未設定",
            Lang::English  => "Not set",
            Lang::Chinese  => "未设置",
        }
    }

    /// 原文/翻訳先言語のどちらかが未設定のまま翻訳を実行しようとした場合のエラー。
    pub fn translate_child_lang_required(self) -> &'static str {
        match self {
            Lang::Japanese => "原文言語と翻訳先言語を設定してください",
            Lang::English  => "Please set both the source and target languages",
            Lang::Chinese  => "请设置原文语言和翻译目标语言",
        }
    }

    /// 保存済み翻訳データの言語ペアと、現在UIで選択中の言語ペアが食い違っている場合の警告。
    /// `saved`には「原文→翻訳先」形式でラベル済みの文字列を渡す。
    pub fn translate_child_lang_mismatch_notice(self, saved: &str) -> String {
        match self {
            Lang::Japanese => format!("保存済みデータの言語: {saved}（現在の選択と異なります）"),
            Lang::English  => format!("Saved data language: {saved} (differs from current selection)"),
            Lang::Chinese  => format!("已保存数据的语言: {saved}（与当前选择不同）"),
        }
    }

    /// 子ウィンドウの[言語判定]ボタン。OCR原文を翻訳モデルへ渡して原文言語を推測する。
    pub fn translate_child_lang_detect_button(self) -> &'static str {
        match self {
            Lang::Japanese => "言語判定",
            Lang::English  => "Detect language",
            Lang::Chinese  => "语言判定",
        }
    }

    /// 言語判定結果が現在の原文/翻訳先設定と食い違う場合の確認メッセージ。
    pub fn translate_lang_detect_conflict_notice(self, detected: &str) -> String {
        match self {
            Lang::Japanese => format!("判定結果: {detected}（現在の設定と異なります）"),
            Lang::English  => format!("Detected: {detected} (differs from current setting)"),
            Lang::Chinese  => format!("判定结果: {detected}（与当前设置不同）"),
        }
    }

    /// 上記確認の[判定結果を設定]ボタン。
    pub fn translate_lang_detect_apply_button(self) -> &'static str {
        match self {
            Lang::Japanese => "判定結果を設定",
            Lang::English  => "Apply detected",
            Lang::Chinese  => "应用判定结果",
        }
    }

    /// 上記確認の[現在の設定を維持]ボタン。
    pub fn translate_lang_detect_keep_button(self) -> &'static str {
        match self {
            Lang::Japanese => "現在の設定を維持",
            Lang::English  => "Keep current setting",
            Lang::Chinese  => "保留当前设置",
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

    pub fn settings_image_filter_reset_button(self) -> &'static str {
        match self {
            Lang::Japanese => "既定値に戻す",
            Lang::English  => "Reset",
            Lang::Chinese  => "恢复默认",
        }
    }

    pub fn settings_image_filter_instant_save_notice(self) -> &'static str {
        match self {
            Lang::Japanese => "このタブの変更は即時セーブされます",
            Lang::English  => "Changes in this tab are saved instantly",
            Lang::Chinese  => "此标签页的更改会即时保存",
        }
    }

    pub fn settings_image_filter_color_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 色系統フィルター",
            Lang::English  => "■ Color filter",
            Lang::Chinese  => "■ 色彩滤镜",
        }
    }

    pub fn settings_image_filter_color_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "同時に有効化できるのは1つのみ。ビューアーの静止画表示にのみ効き、アニメーション再生やサムネイルには影響しない。",
            Lang::English  => "Only one can be active at a time. Affects the viewer's still-image display only — not animation playback or thumbnails.",
            Lang::Chinese  => "同一时间只能启用一种。仅影响查看器的静止图像显示，不影响动画播放或缩略图。",
        }
    }

    pub fn settings_image_filter_mode_none(self) -> &'static str {
        match self {
            Lang::Japanese => "なし",
            Lang::English  => "None",
            Lang::Chinese  => "无",
        }
    }

    pub fn settings_image_filter_mode_blue_light_cut(self) -> &'static str {
        match self {
            Lang::Japanese => "ブルーライトカット",
            Lang::English  => "Blue light cut",
            Lang::Chinese  => "蓝光过滤",
        }
    }

    pub fn settings_image_filter_mode_sepia(self) -> &'static str {
        match self {
            Lang::Japanese => "セピア",
            Lang::English  => "Sepia",
            Lang::Chinese  => "怀旧棕褐",
        }
    }

    pub fn settings_image_filter_mode_grayscale(self) -> &'static str {
        match self {
            Lang::Japanese => "モノクロ",
            Lang::English  => "Grayscale",
            Lang::Chinese  => "黑白",
        }
    }

    pub fn settings_image_filter_blc_temp_label(self) -> &'static str {
        match self {
            Lang::Japanese => "色温度",
            Lang::English  => "Color temperature",
            Lang::Chinese  => "色温",
        }
    }

    pub fn settings_image_filter_blc_temp_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "プリセットボタンとスライダーは同じ値を書き換える。最後に操作した方が有効値になる。",
            Lang::English  => "Preset buttons and the slider write the same value — whichever you touch last takes effect.",
            Lang::Chinese  => "预设按钮与滑块共用同一个值，以最后操作的为准。",
        }
    }

    pub fn settings_image_filter_tone_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ トーン調整",
            Lang::English  => "■ Tone adjustments",
            Lang::Chinese  => "■ 色调调整",
        }
    }

    pub fn settings_image_filter_tone_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "色系統フィルターとは独立して、常に重ねがけできる。",
            Lang::English  => "These stack independently of the color filter above and can always be combined.",
            Lang::Chinese  => "与上方色彩滤镜相互独立，随时可叠加使用。",
        }
    }

    pub fn settings_image_filter_gamma_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ガンマ",
            Lang::English  => "Gamma",
            Lang::Chinese  => "伽马",
        }
    }

    pub fn settings_image_filter_brightness_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ブライトネス",
            Lang::English  => "Brightness",
            Lang::Chinese  => "亮度",
        }
    }

    pub fn settings_image_filter_sharpness_label(self) -> &'static str {
        match self {
            Lang::Japanese => "シャープネス",
            Lang::English  => "Sharpness",
            Lang::Chinese  => "锐化",
        }
    }

    pub fn settings_image_filter_order_section_label(self) -> &'static str {
        match self {
            Lang::Japanese => "■ 処理順",
            Lang::English  => "■ Processing order",
            Lang::Chinese  => "■ 处理顺序",
        }
    }

    pub fn settings_image_filter_order_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "カードをドラッグして並べ替える。上から順に適用される。",
            Lang::English  => "Drag the cards to reorder. Applied from top to bottom.",
            Lang::Chinese  => "拖动卡片调整顺序，按从上到下的顺序应用。",
        }
    }

    pub fn settings_image_filter_stage_color_label(self) -> &'static str {
        match self {
            Lang::Japanese => "色系統フィルター",
            Lang::English  => "Color filter",
            Lang::Chinese  => "色彩滤镜",
        }
    }

    pub fn blue_light_cut_toggle_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ブルーライトカット",
            Lang::English  => "Blue light cut",
            Lang::Chinese  => "蓝光过滤",
        }
    }

    pub fn settings_version_label(self) -> &'static str {
        match self {
            Lang::Japanese => "バージョン",
            Lang::English  => "Version",
            Lang::Chinese  => "版本",
        }
    }

    pub fn settings_startup_use_last_dir(self) -> &'static str {
        match self {
            Lang::Japanese => "アプリ終了時に居たフォルダへ復帰する",
            Lang::English  => "Restore the folder open at exit on next launch",
            Lang::Chinese  => "启动时恢复上次退出时所在的文件夹",
        }
    }

    pub fn settings_startup_use_last_dir_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "アクセスできない場合（ネットワークドライブ切断など）は下の固定フォルダへフォールバックします。",
            Lang::English  => "Falls back to the fixed folder below if it's no longer accessible (e.g. a disconnected network drive).",
            Lang::Chinese  => "若无法访问（如网络驱动器断开），将回退到下方的固定文件夹。",
        }
    }

    pub fn settings_startup_fixed_dir_label(self) -> &'static str {
        match self {
            Lang::Japanese => "起動時に開く固定フォルダ",
            Lang::English  => "Fixed folder to open at startup",
            Lang::Chinese  => "启动时打开的固定文件夹",
        }
    }

    pub fn settings_startup_fixed_dir_explain(self) -> &'static str {
        match self {
            Lang::Japanese => "空欄ならホームディレクトリ、ホームにも移動できなければルートを使います。",
            Lang::English  => "Leave empty to use the home directory, or the root if that's unavailable too.",
            Lang::Chinese  => "留空则使用主目录，若主目录也无法使用则使用根目录。",
        }
    }

    pub fn settings_viewer_blocked(self) -> &'static str {
        match self {
            Lang::Japanese => "設定変更中は操作できません",
            Lang::English  => "Locked while settings are open",
            Lang::Chinese  => "设置窗口打开期间无法操作",
        }
    }

    // ── ツールパレット（ビューアー内オーバーレイの5x2グリッド） ──────────────

    pub fn tool_palette_toggle_label_blue_light_cut(self) -> &'static str {
        match self {
            Lang::Japanese => "ブルーライトカット",
            Lang::English  => "Blue Light Cut",
            Lang::Chinese  => "蓝光过滤",
        }
    }

    pub fn tool_palette_toggle_label_gamma(self) -> &'static str {
        match self {
            Lang::Japanese => "ガンマ有効",
            Lang::English  => "Gamma",
            Lang::Chinese  => "伽马校正",
        }
    }

    pub fn tool_palette_toggle_label_brightness(self) -> &'static str {
        match self {
            Lang::Japanese => "ブライトネス有効",
            Lang::English  => "Brightness",
            Lang::Chinese  => "亮度调整",
        }
    }

    pub fn tool_palette_toggle_label_sharpness(self) -> &'static str {
        match self {
            Lang::Japanese => "シャープネス有効",
            Lang::English  => "Sharpness",
            Lang::Chinese  => "锐化开启",
        }
    }

    pub fn tool_palette_action_label_next_page(self) -> &'static str {
        match self {
            Lang::Japanese => "次のページ",
            Lang::English  => "Next Page",
            Lang::Chinese  => "下一页",
        }
    }

    pub fn tool_palette_action_label_prev_page(self) -> &'static str {
        match self {
            Lang::Japanese => "前のページ",
            Lang::English  => "Prev Page",
            Lang::Chinese  => "上一页",
        }
    }

    pub fn tool_palette_action_label_open_folder(self) -> &'static str {
        match self {
            Lang::Japanese => "フォルダを開く",
            Lang::English  => "Open Folder",
            Lang::Chinese  => "打开文件夹",
        }
    }

    pub fn tool_palette_action_label_toggle_fullscreen(self) -> &'static str {
        match self {
            Lang::Japanese => "最大化",
            Lang::English  => "Maximize",
            Lang::Chinese  => "最大化",
        }
    }

    pub fn tool_palette_action_label_slideshow_toggle(self) -> &'static str {
        match self {
            Lang::Japanese => "スライドショー",
            Lang::English  => "Slideshow",
            Lang::Chinese  => "幻灯片放映",
        }
    }

    pub fn tool_palette_dialog_title_image_filter(self) -> &'static str {
        match self {
            Lang::Japanese => "画像フィルタ",
            Lang::English  => "Image Filter",
            Lang::Chinese  => "图像滤镜",
        }
    }

    pub fn tool_palette_drag_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "ドラッグで移動",
            Lang::English  => "Drag to move",
            Lang::Chinese  => "拖动以移动",
        }
    }

    pub fn tool_palette_lock_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "位置の固定ON/OFF（ONの間はドラッグ移動できない）",
            Lang::English  => "Lock position ON/OFF (dragging disabled while ON)",
            Lang::Chinese  => "锁定位置开关（开启时无法拖动）",
        }
    }

    pub fn tool_palette_opacity_hint(self, pct: u8) -> String {
        match self {
            Lang::Japanese => format!("背景の透過度：{pct}%（クリックで10%刻みに変更）"),
            Lang::English  => format!("Background opacity: {pct}% (click to change by 10%)"),
            Lang::Chinese  => format!("背景透明度：{pct}%（点击以10%为单位调整）"),
        }
    }

    pub fn tool_palette_auto_hide_on_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "自動ハイドLOCK：ON（常時表示。クリックでOFFにするとポインタが外れて0.5秒後に自動的に隠れるようになる）",
            Lang::English  => "Auto-hide LOCK: ON (always shown. Click to turn OFF so it auto-hides 0.5s after the pointer leaves)",
            Lang::Chinese  => "自动隐藏锁定：开启（始终显示。点击关闭后，指针移出0.5秒将自动隐藏）",
        }
    }

    pub fn tool_palette_auto_hide_off_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "自動ハイドLOCK：OFF（ポインタが外れて0.5秒後に自動的に隠れる。クリックでONにすると常時表示に戻る）",
            Lang::English  => "Auto-hide LOCK: OFF (auto-hides 0.5s after the pointer leaves. Click to turn ON to always show)",
            Lang::Chinese  => "自动隐藏锁定：关闭（指针移出0.5秒后自动隐藏。点击开启可始终显示）",
        }
    }

    pub fn tool_palette_size_hint(self, size_px: i32) -> String {
        match self {
            Lang::Japanese => format!("マスのサイズ：{size_px}px（クリックで段階変更）"),
            Lang::English  => format!("Slot size: {size_px}px (click to change)"),
            Lang::Chinese  => format!("格子尺寸：{size_px}px（点击切换）"),
        }
    }

    pub fn tool_palette_close_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "パレットを隠す（画面上で右クリックすると再表示）",
            Lang::English  => "Hide the palette (right-click the screen to show it again)",
            Lang::Chinese  => "隐藏工具面板（在画面上右键点击可重新显示）",
        }
    }

    pub fn tool_palette_dialog_close(self) -> &'static str {
        match self {
            Lang::Japanese => "閉じる",
            Lang::English  => "Close",
            Lang::Chinese  => "关闭",
        }
    }

    pub fn tool_palette_slot_empty_hint(self) -> &'static str {
        match self {
            Lang::Japanese => "空欄（右クリックで登録）",
            Lang::English  => "Empty (right-click to assign)",
            Lang::Chinese  => "空（右键点击以设置）",
        }
    }

    pub fn tool_palette_slot_change_suffix(self) -> &'static str {
        match self {
            Lang::Japanese => "（右クリックで変更）",
            Lang::English  => " (right-click to change)",
            Lang::Chinese  => "（右键点击以更改）",
        }
    }

    pub fn tool_palette_rename_menu_label(self) -> &'static str {
        match self {
            Lang::Japanese => "ボタン名称の変更",
            Lang::English  => "Rename button",
            Lang::Chinese  => "更改按钮名称",
        }
    }

    pub fn tool_palette_rename_hint_text(self) -> &'static str {
        match self {
            Lang::Japanese => "空欄で非表示",
            Lang::English  => "Leave blank to hide",
            Lang::Chinese  => "留空以隐藏",
        }
    }

    pub fn tool_palette_rename_ok(self) -> &'static str {
        match self {
            Lang::Japanese => "OK",
            Lang::English  => "OK",
            Lang::Chinese  => "确定",
        }
    }

    pub fn tool_palette_rename_cancel(self) -> &'static str {
        match self {
            Lang::Japanese => "キャンセル",
            Lang::English  => "Cancel",
            Lang::Chinese  => "取消",
        }
    }

    pub fn tool_palette_slot_clear_label(self) -> &'static str {
        match self {
            Lang::Japanese => "空欄に戻す",
            Lang::English  => "Clear",
            Lang::Chinese  => "清空",
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

#[cfg(test)]
mod thumbnail_status_tests {
    use super::Lang;

    #[test]
    fn japanese_status_keeps_error_before_update_suffix() {
        assert_eq!(
            Lang::Japanese.thumbnail_status(77, 80, 3, true),
            "サムネイル 77/80 エラー 3 新形式に更新中",
        );
    }

    #[test]
    fn normal_status_has_no_extra_suffix() {
        assert_eq!(
            Lang::Japanese.thumbnail_status(80, 80, 0, false),
            "サムネイル 80/80",
        );
    }
}
