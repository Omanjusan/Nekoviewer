//! 一括タグ取得の確認ウィンドウに出す文面の組み立て（純関数）。画面（egui）には触れない。
//!
//! - 行の結論文、変更ダイアログの選択肢（見出しと説明）、「要確認」の種類と理由（短文・長文）を作る
//! - 用語は「タグ採用予定」「タグ不採用」に統一する。「付ける」「付けない」の単独表記は使わず、
//!   「ファイルにタグ付けする／しない」とフルで書く
//! - 日本語・English・中文の3言語。固定の文言は `i18n::tr`、数や名前を埋め込む文言は `i18n::render` を通す。
//!   言語トグルの「日中（テスト）」で、日本語と中文が併記される（訳の確認用）。新しい文言も、この形で足す
//! - 呼び出し側は、確認ウィンドウの行描画・変更ダイアログ・理由ポップアップ（フェーズB以降）

use crate::i18n::{self, Lang};
use crate::tag_candidates::{Candidate, CandidateInput, CatRef, Choice, ParentMode, Role};
use crate::tag_master::{display_name, primary_alias};
use crate::view_explorer::TagManagerCategoryUi;

/// 採用する（ファイルにタグ付けする）状態の名前。
pub(crate) fn adopt_label() -> &'static str {
    i18n::tr("タグ採用予定", "Adopt", "计划采用")
}
fn adopt_for(l: Lang) -> &'static str {
    i18n::pick(l, "タグ採用予定", "Adopt", "计划采用")
}
/// 採用しない（ファイルにタグ付けしない）状態の名前。
pub(crate) fn reject_label() -> &'static str {
    i18n::tr("タグ不採用", "Reject", "不采用")
}
fn reject_for(l: Lang) -> &'static str {
    i18n::pick(l, "タグ不採用", "Reject", "不采用")
}
/// 変更ダイアログの副題。
pub(crate) fn resolve_subtitle() -> &'static str {
    i18n::tr("適用するまで、ファイルは変わりません。", "Nothing changes in your files until you apply.", "在应用之前，文件不会改变。")
}

/// 行の採否トグルに出す文字。
pub(crate) fn toggle_label(adopted: bool) -> String {
    if adopted {
        format!("✓ {}", adopt_label())
    } else {
        reject_label().to_string()
    }
}

// ── 「他の要素と同一としてまとめる」 ────────────────────────────────

/// 行の「⋯」メニュー・類似品の見出しのボタン名。
pub(crate) fn merge_label() -> &'static str {
    i18n::tr("他の要素と同一としてまとめる…", "Merge with another element…", "与其他元素合并为同一个…")
}
/// まとめた行の「⋯」メニューの項目。
pub(crate) fn split_label() -> &'static str {
    i18n::tr("まとめを解除（適用前のみ）", "Undo merge (before applying only)", "取消合并（仅限应用前）")
}
/// ダイアログのタイトル。
pub(crate) fn merge_title() -> &'static str {
    i18n::tr("他の要素と同一としてまとめる", "Merge with another element", "与其他元素合并为同一个")
}
/// ダイアログの実行ボタン。
pub(crate) fn merge_ok_label() -> &'static str {
    i18n::tr("まとめる", "Merge", "合并")
}
/// まとめた行に付ける、同じものの目印の見出し。
pub(crate) fn merged_label() -> &'static str {
    i18n::tr("同一", "Same", "相同")
}

/// ダイアログの冒頭の説明（用途）。
pub(crate) fn merge_intro(name: &str) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!(
            "『{name}』を、下記のタグと同じものとしてまとめます。「同人誌|doujinshi」のように、英語と日本語を同一視したいときや、カテゴリの数を増やさずに、検索結果を同じにしたいときに使ってください。"
        ),
        Lang::English => format!(
            "Treats ‘{name}’ as the same thing as the tags below. Use this when you want to equate English and Japanese names, such as “同人誌|doujinshi”, or to get the same search results without adding more categories."
        ),
        Lang::Chinese => format!(
            "将『{name}』与下列标签合并为同一个。适用于想把英文和日文视为同一事物（如“同人誌|doujinshi”），或不增加分类数量而让搜索结果相同的情况。"
        ),
    })
}

/// ダイアログの「まとめる前に知っておくこと」（デメリット）。
pub(crate) fn merge_consequences() -> [&'static str; 4] {
    [
        i18n::tr(
            "まとめたタグは1つのタグとして扱われます。片方の名前だけで、単独に検索することはできなくなります。",
            "Merged tags are treated as one tag. You can no longer search by only one of the names.",
            "合并后的标签会被视为一个标签，无法再只用其中一个名称单独搜索。",
        ),
        i18n::tr(
            "まとめを、あとから分離する機能はありません。適用前なら「まとめを解除」で戻せますが、適用後は、タグ管理で名前の別名を消すことしかできず、すでに付いたタグが別々のタグに戻ることはありません。",
            "There is no way to separate them later. Before applying you can undo with “Undo merge”; after applying you can only delete the alias in Tag Manager, and tags already attached will not go back to separate tags.",
            "之后无法再将其分开。应用前可通过“取消合并”恢复；应用后只能在标签管理中删除名称的别名，已添加的标签不会恢复为各自独立的标签。",
        ),
        i18n::tr(
            "すでに別々に登録されているタグどうしは、まとめられません。",
            "Tags that are already registered separately cannot be merged.",
            "已经分别登记的标签之间无法合并。",
        ),
        i18n::tr(
            "まとめた結果は、必ず「タグ採用予定」になります。",
            "The merged result is always set to “Adopt”.",
            "合并的结果一定会设为“计划采用”。",
        ),
    ]
}

/// 選んだ候補の、まとめる前の確認文（件数つき）。2件未満のときは促す文。
pub(crate) fn merge_preview(names: &[&str], files: usize) -> String {
    i18n::render(|l| {
        if names.len() < 2 {
            return pick_str(l, "2件以上を選んでください", "Select two or more", "请选择2个以上");
        }
        match l {
            Lang::Japanese => format!("まとめる候補：{}（合計{files}ファイル）", names.join("、")),
            Lang::English => format!("Candidates to merge: {} ({files} files in total)", names.join(", ")),
            Lang::Chinese => format!("要合并的候选：{}（共{files}个文件）", names.join("、")),
        }
    })
}

fn pick_str(l: Lang, ja: &str, en: &str, zh: &str) -> String {
    match l {
        Lang::Japanese => ja,
        Lang::English => en,
        Lang::Chinese => zh,
    }
    .to_string()
}

/// 類似品の束の見出し。
pub(crate) fn cluster_header(size: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("名前が似ている候補 {size}件"),
        Lang::English => format!("{size} candidates with similar names"),
        Lang::Chinese => format!("名称相似的候选 {size}项"),
    })
}

/// 類似品の束の見出しのボタン。
pub(crate) fn cluster_adopt_label() -> &'static str {
    i18n::tr("まとめて「タグ採用予定」へ", "Adopt all", "全部设为“计划采用”")
}
pub(crate) fn cluster_reject_label() -> &'static str {
    i18n::tr("まとめて「タグ不採用」へ", "Reject all", "全部设为“不采用”")
}

/// 採否トグルのホバー文。
pub(crate) fn toggle_hint() -> &'static str {
    i18n::tr("押すと「タグ採用予定」と「タグ不採用」が切り替わります", "Click to switch between “Adopt” and “Reject”", "点击可在“计划采用”和“不采用”之间切换")
}

/// 行に出す対象ファイル数。
pub(crate) fn file_count(n: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("{n}ファイル"),
        Lang::English => format!("{n} files"),
        Lang::Chinese => format!("{n}个文件"),
    })
}

/// 名前列の先頭に出す対象ファイル数。
pub(crate) fn file_count_short(n: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("{n}件"),
        Lang::English => format!("{n}"),
        Lang::Chinese => format!("{n}项"),
    })
}

/// 行の1段目に出す、要確認の印（短い理由つき）。
pub(crate) fn review_badge(kind: &ReviewKind) -> String {
    i18n::render(|l| format!("{}{}", pick_str(l, "要確認：", "Needs review: ", "需确认："), review_short_for(kind, l)))
}

// ── 一覧（表）の見出しと表示密度 ───────────────────────────────────

pub(crate) fn col_toggle() -> &'static str {
    i18n::tr("採否", "Adopt?", "采用与否")
}
pub(crate) fn col_name() -> &'static str {
    i18n::tr("名前", "Name", "名称")
}
/// 要確認の印と、何をどこにどうするかの結論文が入る列の見出し。
pub(crate) fn col_state() -> &'static str {
    i18n::tr("内容・要確認", "Details / needs review", "内容・需确认")
}

/// 種類の帯（行をまたいで中央に出す見出し）の前置き。


/// 種類の帯の文字。メインタグの種類は「カテゴリ（メインタグ）」という名前なので、「カテゴリ：メインタグ」にする。
pub(crate) fn band_title(role_label: impl Fn(Lang) -> String, is_main: bool) -> String {
    i18n::render(|l| {
        let prefix = pick_str(l, "カテゴリ：", "Category: ", "分类：");
        if is_main {
            format!("{prefix}{}", pick_str(l, "メインタグ", "Main tag", "主标签"))
        } else {
            format!("{prefix}{}", role_label(l))
        }
    })
}
pub(crate) fn density_standard() -> &'static str {
    i18n::tr("標準（2段）", "Standard (2 lines)", "标准（2行）")
}
pub(crate) fn density_compact() -> &'static str {
    i18n::tr("コンパクト（1段）", "Compact (1 line)", "紧凑（1行）")
}
pub(crate) fn density_hint() -> &'static str {
    i18n::tr("一覧の行の密度を切り替えます。コンパクトは1行で、行数が多いときに便利です", "Switches the row density of the list. Compact uses one line per row, handy for long lists.", "切换列表的行密度。紧凑模式每行只占一行，适合条目很多的时候。")
}

// ── 窓冒頭の説明・よくある質問・適用前の確認・上限ダイアログ ─────────────

pub(crate) fn intro_title() -> &'static str {
    i18n::tr("この窓で求めること", "What this window asks of you", "此窗口需要您做的事")
}
pub(crate) fn intro_fold() -> &'static str {
    i18n::tr("説明をたたむ", "Collapse", "收起说明")
}
pub(crate) fn intro_unfold() -> &'static str {
    i18n::tr("説明を開く", "Expand", "展开说明")
}
/// 冒頭の説明（段落ごと）。
pub(crate) fn intro_paras() -> [&'static str; 3] {
    [
        i18n::tr(
            "各候補は、ファイルから自動抽出したタグです。検出元のファイルに付けるか（「タグ採用予定」）、付けないか（「タグ不採用」）のどちらかを選びます。「タグ採用予定」は、最終確認で適用した時点で、ファイルのタグDBに書き込まれます。適用するまで、ファイルは変わりません。",
            "Each candidate is a tag extracted automatically from the files. Choose whether to attach it to the file it was found in (“Adopt”) or not (“Reject”). “Adopt” items are written to the files’ tag database when you apply at the final confirmation. Nothing changes in your files until you apply.",
            "每个候选都是从文件中自动提取的标签。请选择是否为其来源文件添加该标签（“计划采用”）或不添加（“不采用”）。“计划采用”的候选会在最终确认并应用时写入文件的标签数据库。在应用之前，文件不会改变。",
        ),
        i18n::tr(
            "「タグ不採用」の候補は、登録もしません。付けたくないものは、「タグ不採用」にしてください（不採用は保存されないので、次の自動抽出で、また候補に出ます）。",
            "Candidates set to “Reject” are not registered either. Set anything you do not want to “Reject” (rejections are not saved, so they appear again in the next extraction).",
            "设为“不采用”的候选也不会被登记。不想添加的请设为“不采用”（不采用的结果不会被保存，下次自动提取时仍会作为候选出现）。",
        ),
        i18n::tr(
            "迷ったら、「変更…」で、どう扱うかを選べます。「要確認」と出ている候補は、内容を見て、扱いを決めてください。",
            "If you are unsure, use “Change…” to choose how to handle it. For candidates marked “Needs review”, check the details and decide.",
            "拿不准时，可以点击“更改…”选择处理方式。标有“需确认”的候选，请查看内容后决定如何处理。",
        ),
    ]
}
pub(crate) fn failed_title() -> &'static str {
    i18n::tr("失敗したと思ったら", "If something seems to have gone wrong", "如果觉得出了问题")
}
/// 「失敗したと思ったら」の説明（段落ごと）。
pub(crate) fn failed_paras() -> [&'static str; 3] {
    [
        i18n::tr(
            "適用の前に、データベースのバックアップを自動で取ります。",
            "A database backup is taken automatically before applying.",
            "应用之前，会自动备份数据库。",
        ),
        i18n::tr(
            "この窓の操作を最後まで終えると、「試用中」になります。このとき、タグパネルの「確定」か「破棄」を選べます。",
            "When you finish all the steps in this window, it enters the “trial” state. Then you can choose “Confirm” or “Discard” in the tag panel.",
            "完成此窗口的全部操作后，会进入“试用中”。此时可在标签面板中选择“确定”或“放弃”。",
        ),
        i18n::tr(
            "「破棄」を選ぶと、この自動抽出で足したタグだけをファイルから外し、操作する前に戻します（適用のあとに手で付けた・外したタグには触りません）。データベースのバックアップからの復元は、別に、設定のバックアップ画面でできます。",
            "Choosing “Discard” removes only the tags added by this extraction and returns to the state before you started (tags you added or removed by hand after applying are left alone). Restoring from a database backup is a separate action in the settings backup screen.",
            "选择“放弃”后，仅会从文件中移除本次自动提取添加的标签，恢复到操作之前（应用后手动添加或移除的标签不受影响）。从数据库备份恢复，请另外在设置的备份界面中进行。",
        ),
    ]
}
pub(crate) fn faq_button() -> &'static str {
    i18n::tr("よくある質問", "FAQ", "常见问题")
}
pub(crate) fn faq_title() -> &'static str {
    i18n::tr("よくある質問（見出しをクリックして展開）", "FAQ (click a heading to expand)", "常见问题（点击标题展开）")
}
/// よくある質問（質問, 答え）。
pub(crate) fn faq_items() -> [(&'static str, &'static str); 3] {
    [
        (
            i18n::tr(
                "自動で付けたタグを、手動で消すと、どうなりますか？",
                "What happens if I delete an automatically added tag by hand?",
                "手动删除自动添加的标签会怎样？",
            ),
            i18n::tr(
                "消したタグは、外れたままです。ただし、「消した」ことは記憶されません。次の自動抽出で同じ候補が出れば、また付きます。付けたくないときは、この窓で「タグ不採用」にしてください。",
                "The deleted tag stays removed. However, the deletion is not remembered: if the same candidate appears in the next extraction, it will be added again. If you do not want it, set it to “Reject” in this window.",
                "被删除的标签会保持删除状态。但“已删除”这件事不会被记住：如果下次自动提取时再次出现相同候选，它会被重新添加。不想添加时，请在此窗口中设为“不采用”。",
            ),
        ),
        (
            i18n::tr(
                "手動のタグと、自動のタグは、どう混ざりますか？",
                "How do manual tags and automatic tags mix?",
                "手动标签和自动标签会如何混合？",
            ),
            i18n::tr(
                "手動で付けたタグは、自動抽出で消えません。自動は、足すだけです。形式・メインなど、1つしか選べないカテゴリは、手動のタグが優先されます（メインは、未設定のときだけ付きます）。同じタグがすでにあるときは、何も起きません。逆に、自動で付けたあとに手動で付け直したタグは、自動付与の記録から外れます。「付け直し」や「破棄」で、外れることはありません。",
                "Tags you added by hand are never removed by automatic extraction; automatic extraction only adds. For categories that allow only one choice, such as format and main, the manual tag takes priority (a main tag is added only when none is set). If the same tag already exists, nothing happens. Conversely, a tag you re-add by hand after it was added automatically is removed from the automatic-tagging record. “Re-attach” and “Discard” never remove such tags.",
                "手动添加的标签不会被自动提取删除，自动提取只会添加。对于格式、主分类等只能选择一个的分类，手动标签优先（主标签仅在未设置时才会添加）。如果已有相同的标签，则不会发生任何变化。反过来，自动添加之后又手动重新添加的标签，会从自动添加的记录中移除。“重新标记”和“放弃”都不会移除这类标签。",
            ),
        ),
        (
            i18n::tr(
                "旧バージョンの手動タグも含めて、全部やり直したいときは？",
                "What if I want to start over from scratch, including manual tags from older versions?",
                "想连同旧版本的手动标签一起全部重来，该怎么办？",
            ),
            i18n::tr(
                "自動付与の分だけなら、試用中は「破棄」、確定後は、適用のときの「前回の自動付与分を外して付け直す」を使います（手動のタグは外れません）。手動のタグも含めて全部消すなら、タグ管理の「危険な設定」の「タグのクリア」です。全ファイルのタグが消え、タグマスターも初期状態に戻ります（評価・お気に入り・学習データは残ります）。直前に、データベースとマスターを自動でバックアップします。心配なときは、先にリカバリポイントを作ってください。",
                "For the automatically added tags only: use “Discard” during the trial, or after confirming, use “Remove the previous automatic tags and re-attach” when applying (manual tags are not removed). To delete everything including manual tags, use “Clear tags” under “Dangerous settings” in Tag Manager. All file tags are deleted and the tag master returns to its initial state (ratings, favorites and learning data remain). The database and master are backed up automatically just before. If you are worried, create a recovery point first.",
                "如果只想处理自动添加的部分：试用期间使用“放弃”；确定之后，在应用时使用“移除上次自动标记后重新标记”（手动标签不会被移除）。如果要连手动标签一起全部清除，请使用标签管理“危险设置”中的“清除标签”。所有文件的标签会被清除，标签主表也会恢复为初始状态（评分、收藏和学习数据会保留）。执行前会自动备份数据库和主表。如果不放心，请先创建恢复点。",
            ),
        ),
    ]
}

/// 1枚目の窓の短い呼び名（下端の「戻る」ボタンの文面に使う）。

/// 2枚目の窓の短い呼び名（下端の「次へ」ボタンの文面に使う）。


/// 1枚目から2枚目へ進むボタン。
pub(crate) fn footer_next_label() -> String {
    i18n::render(|l| match l {
        Lang::Japanese => "次へ：2/2 作者・グループの解決ページへ進む".to_string(),
        Lang::English => "Next: go to 2/2 – authors and groups".to_string(),
        Lang::Chinese => "下一步：前往 2/2 作者・团体 的确认页".to_string(),
    })
}

/// 2枚目から1枚目へ戻るボタン。
pub(crate) fn footer_back_label() -> String {
    i18n::render(|l| match l {
        Lang::Japanese => "戻る：1/2 形式・カテゴリ・言語・イベントの解決ページへ戻る".to_string(),
        Lang::English => "Back: return to 1/2 – format, category, language, event".to_string(),
        Lang::Chinese => "返回：回到 1/2 格式・分类・语言・活动 的确认页".to_string(),
    })
}

pub(crate) fn cancel_confirm_title() -> &'static str {
    i18n::tr("確認をやめますか？", "Stop reviewing?", "要停止确认吗？")
}
pub(crate) fn cancel_confirm_body() -> &'static str {
    i18n::tr(
        "ここまでの選択はすべて捨てられ、ファイルにはタグが付きません。",
        "All choices made so far will be discarded, and no tags will be added to the files.",
        "到目前为止的所有选择都会被丢弃，文件不会被添加标签。",
    )
}
pub(crate) fn cancel_confirm_continue() -> &'static str {
    i18n::tr("続けて確認する", "Keep reviewing", "继续确认")
}
pub(crate) fn cancel_confirm_go() -> &'static str {
    i18n::tr("やめる", "Stop", "停止")
}
pub(crate) fn footer_apply() -> &'static str {
    i18n::tr("今までの選択を適用", "Apply selections so far", "应用目前的选择")
}

pub(crate) fn precheck_title() -> &'static str {
    i18n::tr("要確認の候補が残っています", "Some items still need review", "仍有需要确认的候选")
}
pub(crate) fn precheck_back() -> &'static str {
    i18n::tr("戻って確認する", "Go back and review", "返回确认")
}
pub(crate) fn precheck_go() -> &'static str {
    i18n::tr("このまま適用する", "Apply anyway", "直接应用")
}

/// 適用前の確認の本文（未確認の要確認が残っているとき）。
pub(crate) fn precheck_body(count: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("まだ確認していない「要確認」の候補が{count}件あります。このまま適用すると、いまの「タグ採用予定」「タグ不採用」の選択どおりに、ファイルにタグが付きます。確認してから適用しますか？"),
        Lang::English => format!("{count} “Needs review” candidates have not been checked yet. If you apply now, tags will be attached exactly as the current “Adopt” / “Reject” choices say. Do you want to review them before applying?"),
        Lang::Chinese => format!("还有{count}项“需确认”的候选尚未确认。如果现在直接应用，将按照目前的“计划采用”“不采用”选择为文件添加标签。要先确认再应用吗？"),
    })
}

pub(crate) fn limit_title() -> &'static str {
    i18n::tr("新規に登録する要素の数が、上限を超えています", "The number of new elements exceeds the limit", "新登记的元素数量超过了上限")
}

/// 上限ダイアログの本文。`rows` は種類ごとの超過の行、`db_size` はいまのDBのサイズ、`step` は拡張の幅。
pub(crate) fn limit_body(limit: usize, rows: &str, db_size: &str, step: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!(
            "カテゴリごとの、新規に登録する要素の上限（{limit}個）を超えています。\n\n{rows}\n\n現在のデータベースのサイズ：{db_size}\n\n【{step}個拡張する（おすすめ）】上限を{step}個増やし、すべての要素を登録します。要素が増えたぶん、データベースは大きくなります。ストレージに余裕があるなら、こちらをおすすめします。\n【上限を維持する】データベースは、これ以上大きくなりません。ただし、ファイル数の多い要素から{limit}個までしか登録されません。残りの要素は新しく取り込まれず、その要素が付くはずだったファイルには、タグが付きません。"
        ),
        Lang::English => format!(
            "The number of new elements to register per category exceeds the limit ({limit}).\n\n{rows}\n\nCurrent database size: {db_size}\n\n[Raise by {step} (recommended)] Raises the limit by {step} and registers all elements. The database grows as elements are added. Recommended if you have spare storage.\n[Keep the limit] The database will not grow any further. However, only up to {limit} elements are registered, starting with those with the most files. The remaining elements are not imported, and the files they would have tagged get no tag."
        ),
        Lang::Chinese => format!(
            "每个分类新登记的元素数量超过了上限（{limit}个）。\n\n{rows}\n\n当前数据库大小：{db_size}\n\n【扩展{step}个（推荐）】将上限增加{step}个，并登记所有元素。元素增加后，数据库也会变大。如果存储空间充足，推荐选择此项。\n【保持上限】数据库不会再继续变大。但只会按文件数从多到少登记至多{limit}个元素。其余元素不会被新导入，原本会带有这些元素的文件将不会被添加标签。"
        ),
    })
}

// ── 名前列の「元のファイル名」 ────────────────────────────────────────

/// 元のファイル名を探すとき、候補のファイルの先頭から何件まで調べるか（件数が多い候補で、描画のたびに重くしない）。
pub(crate) const SOURCE_SCAN_CAP: usize = 30;
/// ホバーで一覧にするファイル名の件数。
pub(crate) const SOURCE_HOVER_CAP: usize = 10;
pub(crate) fn source_not_in_name() -> &'static str {
    i18n::tr("（ファイル名に含まれない表記）", "(not found in the file name)", "（文件名中不含该写法）")
}
pub(crate) const SOURCE_SEPARATOR: &str = " ｜ ";

/// 「（他N件）」。
pub(crate) fn source_others(n: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("（他{n}件）"),
        Lang::English => format!(" (+{n} more)"),
        Lang::Chinese => format!("（另有{n}项）"),
    })
}

/// 名前列に出す、元のファイル名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceFile {
    /// ファイル名（拡張子つき）。
    pub name: String,
    /// 候補の元になった表記が、ファイル名のどこにあるか（バイト範囲。ファイル名に無ければ None）。
    pub range: Option<(usize, usize)>,
    /// 代表の1件のほかに、この候補が付くファイルの数。
    pub others: usize,
}

/// 候補の元になった表記（見つかった表記・名前・まとめた別名・対の表記）。長いものを先に当てる。
fn source_terms(c: &Candidate) -> Vec<&str> {
    let mut v: Vec<&str> = Vec::new();
    v.extend(c.forms.iter().map(String::as_str));
    v.push(c.name.as_str());
    v.extend(c.aliases.iter().map(String::as_str));
    v.extend(c.alt_forms.iter().map(String::as_str));
    for m in &c.members {
        v.extend(m.forms.iter().map(String::as_str));
        v.push(m.name.as_str());
    }
    v.retain(|t| !t.trim().is_empty());
    v.sort_by_key(|t| std::cmp::Reverse(t.len()));
    v.dedup();
    v
}

/// `term` が `name` のどこにあるか。完全一致を先に、なければ英字の大文字小文字を区別しない一致。
fn find_range(name: &str, term: &str) -> Option<(usize, usize)> {
    if let Some(i) = name.find(term) {
        return Some((i, i + term.len()));
    }
    // 英字だけを小文字にしても、バイト長は変わらない。
    name.to_ascii_lowercase().find(&term.to_ascii_lowercase()).map(|i| (i, i + term.len()))
}

/// 候補の「元のファイル名」。表記を含むファイル名を、先頭 `SOURCE_SCAN_CAP` 件から探して優先し、無ければ先頭のファイル。
/// 候補にファイルが無いときは None。
pub(crate) fn source_file(c: &Candidate, inputs: &[CandidateInput]) -> Option<SourceFile> {
    let first = *c.files.first()?;
    let terms = source_terms(c);
    let others = c.files.len() - 1;
    for &f in c.files.iter().take(SOURCE_SCAN_CAP) {
        let Some(input) = inputs.get(f) else { continue };
        for t in &terms {
            if let Some(range) = find_range(&input.file_name, t) {
                return Some(SourceFile { name: input.file_name.clone(), range: Some(range), others });
            }
        }
    }
    let name = inputs.get(first).map(|i| i.file_name.clone()).unwrap_or_default();
    Some(SourceFile { name, range: None, others })
}

/// ホバーの一覧の見出し。
pub(crate) fn source_hover_heading() -> &'static str {
    i18n::tr("◆対象となるファイル", "◆ Target files", "◆ 对象文件")
}

/// ホバーの一覧の1行（番号つき）。
pub(crate) fn source_hover_line(n: usize, name: &str) -> String {
    format!("{n}. {name}")
}

/// ホバーに出す、この候補が付くファイル名の一覧（先頭 `SOURCE_HOVER_CAP` 件と、残りの件数）。
pub(crate) fn source_hover(c: &Candidate, inputs: &[CandidateInput]) -> (Vec<String>, usize) {
    let names = c.files.iter().take(SOURCE_HOVER_CAP).filter_map(|&f| inputs.get(f)).map(|i| i.file_name.clone()).collect();
    (names, c.files.len().saturating_sub(SOURCE_HOVER_CAP))
}

// ── 変更・理由ダイアログのボタン ────────────────────────────────────

/// 要確認のままの行のボタン。
pub(crate) fn resolve_button_label() -> &'static str {
    i18n::tr("解決する…", "Resolve…", "解决…")
}
/// 確認済み（または要確認でない）行のボタン。
pub(crate) fn change_button_label() -> &'static str {
    i18n::tr("変更…", "Change…", "更改…")
}
/// 要確認の印のとなりのボタン。
pub(crate) fn why_button_label() -> &'static str {
    i18n::tr("理由を見る", "Why?", "查看原因")
}
/// 解決した行の印。
pub(crate) fn confirmed_label() -> &'static str {
    i18n::tr("✓ 確認済み", "✓ Confirmed", "✓ 已确认")
}
/// 変更ダイアログの実行ボタン。
pub(crate) fn resolve_ok_label() -> &'static str {
    i18n::tr("この内容にする", "Use this", "采用此内容")
}
/// 理由ダイアログの閉じるボタン。
pub(crate) fn close_label() -> &'static str {
    i18n::tr("閉じる", "Close", "关闭")
}

/// 理由ダイアログのタイトル。
pub(crate) fn why_title(name: &str) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("要確認の理由：{name}"),
        Lang::English => format!("Why this needs review: {name}"),
        Lang::Chinese => format!("需确认的原因：{name}"),
    })
}

// ── 一覧の見出し帯・下端のサマリー・照合の設定 ───────────────────────

pub(crate) fn view_all() -> &'static str {
    i18n::tr("すべて", "All", "全部")
}
pub(crate) fn view_needs_review() -> &'static str {
    i18n::tr("要確認", "Needs review", "需确认")
}
pub(crate) fn default_review_label() -> &'static str {
    i18n::tr("デフォルト表示を要確認項目にする", "Show items needing review by default", "默认显示需确认的项目")
}
pub(crate) fn default_review_hint() -> &'static str {
    i18n::tr("オンのとき、確認窓を開いたら要確認の候補だけを先に表示します（要確認が無いときは、すべてを表示します）", "When on, the window opens showing only items that need review first (all items if none need review).", "开启时，打开确认窗口后会先只显示需确认的候选（没有需确认的候选时显示全部）。")
}
pub(crate) fn needs_review_empty() -> &'static str {
    i18n::tr("要確認の候補はありません。", "No items need review.", "没有需确认的候选。")
}

// ── 多言語の文言の書き方（見本） ──
// 固定の文言は `i18n::tr(日本語, English, 中文)`、数や名前を埋め込む文言は `i18n::render(|lang| …)` を通す。
// こう書くと、「日中（テスト）」表示で、日本語と中文が併記される（訳の確認用）。以後の文言も、この形で足す。
pub(crate) fn recommended_suffix() -> &'static str {
    i18n::tr("（おすすめ）", " (recommended)", "（推荐）")
}
pub(crate) fn merge_warning_heading() -> &'static str {
    i18n::tr("まとめる前に知っておくこと", "Before you merge", "合并前须知")
}
pub(crate) fn select_shown_label() -> &'static str {
    i18n::tr("表示中をすべて選ぶ", "Select all shown", "选择显示中的全部")
}
pub(crate) fn deselect_all_label() -> &'static str {
    i18n::tr("すべて外す", "Deselect all", "全部取消选择")
}
pub(crate) fn parent_hint_hover() -> &'static str {
    i18n::tr("「変更…」で、上位名も登録するかを選べます", "Use “Change…” to choose whether to also register the parent name", "可通过“更改…”选择是否同时登记上位名")
}
pub(crate) fn filter_heading() -> &'static str {
    i18n::tr("絞り込み：", "Filter:", "筛选：")
}
pub(crate) fn bulk_heading() -> &'static str {
    i18n::tr("一括操作：", "Bulk actions:", "批量操作：")
}
pub(crate) fn option_button() -> &'static str {
    i18n::tr("オプション ▾", "Options ▾", "选项 ▾")
}
pub(crate) fn option_density_heading() -> &'static str {
    i18n::tr("表示", "View", "显示")
}
pub(crate) fn bulk_adopt_all() -> &'static str {
    i18n::tr("すべて「タグ採用予定」にする", "Set all to “Adopt”", "全部设为“计划采用”")
}
pub(crate) fn bulk_reject_all() -> &'static str {
    i18n::tr("すべて「タグ不採用」にする", "Set all to “Reject”", "全部设为“不采用”")
}
pub(crate) fn bulk_reject_unconfirmed() -> &'static str {
    i18n::tr("未確定の候補を「タグ不採用」にする", "Set unconfirmed items to “Reject”", "将未确定的候选设为“不采用”")
}

pub(crate) fn settings_button() -> &'static str {
    i18n::tr("⚙ 照合の設定", "⚙ Matching settings", "⚙ 匹配设置")
}
pub(crate) fn settings_hint() -> &'static str {
    i18n::tr("普通は変更不要です", "Usually no change is needed", "通常无需更改")
}
pub(crate) fn settings_title() -> &'static str {
    i18n::tr("照合の設定", "Matching settings", "匹配设置")
}
pub(crate) fn settings_note() -> &'static str {
    i18n::tr("普通は変更不要です。表記のゆれを、どこまで同じものとみなすかの設定です。変えると、候補が作り直されます。", "Usually no change is needed. These settings decide how far spelling variants are treated as the same. Changing them rebuilds the candidates.", "通常无需更改。用于设定把写法差异视为同一事物的程度。更改后会重新生成候选。")
}
pub(crate) fn threshold_note() -> &'static str {
    i18n::tr("低いほど、似ている候補が多く出ます。", "The lower the value, the more similar candidates are shown.", "数值越低，显示的相似候选越多。")
}

/// 一覧の下端のサマリー（この窓の分）。
pub(crate) fn summary(planned: usize, new_elements: usize, rejected: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("{} {planned}件 / マスターに新しく登録 {new_elements}個 / {} {rejected}件", adopt_for(l), reject_for(l)),
        Lang::English => format!("{} {planned} / new in master {new_elements} / {} {rejected}", adopt_for(l), reject_for(l)),
        Lang::Chinese => format!("{} {planned}项 / 新登记到主表 {new_elements}个 / {} {rejected}项", adopt_for(l), reject_for(l)),
    })
}

/// ルールが働いたことを知らせるボタン（ルールの種類の数）。
pub(crate) fn rule_hits_button(kinds: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("ルールが働いた候補（{kinds}種）"),
        Lang::English => format!("Candidates affected by rules ({kinds} kinds)"),
        Lang::Chinese => format!("规则生效的候选（{kinds}种）"),
    })
}

pub(crate) fn rule_hits_title() -> &'static str {
    i18n::tr("ルールが働いた候補", "Candidates affected by rules", "规则生效的候选")
}
pub(crate) fn rule_hits_note() -> &'static str {
    i18n::tr("ルールで「タグ不採用」になった候補には、行に「要確認」が付きます。ファイルにタグ付けしたい場合は、その行で「解決する…」を押してください。", "Candidates set to “Reject” by a rule are marked “Needs review” in their row. To tag the files anyway, press “Resolve…” on that row.", "被规则设为“不采用”的候选，其行内会标有“需确认”。若想为文件添加标签，请在该行点击“解决…”。")
}

/// ルールが成立したファイル数の1行。
pub(crate) fn rule_hit_line(rule: &str, files: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("{rule}：{files}ファイル"),
        Lang::English => format!("{rule}: {files} files"),
        Lang::Chinese => format!("{rule}：{files}个文件"),
    })
}

/// 確認窓を開いたとき・窓を切り替えたときの、最初の表示。
/// 「要確認を既定にする」がオンで、確認が済んでいない要確認があるときだけ「要確認」。それ以外は「すべて」。
pub(crate) fn initial_view_is_review(default_review: bool, pending_review: usize) -> bool {
    default_review && pending_review > 0
}

// ── 上位名（COMIC1★15 に対する COMIC1） ────────────────────────────

/// 変更ダイアログの、上位名の扱いの選択肢1つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParentOption {
    pub mode: ParentMode,
    pub title: String,
    pub desc: String,
}

/// 上位名の候補がある候補の、行に出す小さな知らせ（まだ使っていないとき）。
pub(crate) fn parent_hint(c: &Candidate) -> Option<String> {
    match (&c.parent_name, c.parent_mode) {
        (Some(p), ParentMode::None) if c.members.is_empty() => Some(i18n::render(|l| match l {
            Lang::Japanese => format!("上位名の候補：『{p}』"),
            Lang::English => format!("Parent-name candidate: ‘{p}’"),
            Lang::Chinese => format!("上位名候选：『{p}』"),
        })),
        _ => None,
    }
}

/// 変更ダイアログの、上位名の見出し。
pub(crate) fn parent_heading(parent: &str) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("上位名『{parent}』の扱い"),
        Lang::English => format!("How to handle the parent name ‘{parent}’"),
        Lang::Chinese => format!("上位名『{parent}』的处理方式"),
    })
}

/// 上位名の扱いの選択肢（細かい名前だけ／両方／上位名だけ）。上位名の候補が無い・まとめた候補なら None。
pub(crate) fn parent_options(c: &Candidate) -> Option<Vec<ParentOption>> {
    let parent = c.parent_name.as_deref().filter(|_| c.members.is_empty())?;
    let name = &c.name;
    Some(vec![
        ParentOption {
            mode: ParentMode::None,
            title: i18n::render(|l| match l {
                Lang::Japanese => format!("細かい名前『{name}』だけを登録する"),
                Lang::English => format!("Register only the detailed name ‘{name}’"),
                Lang::Chinese => format!("只登记详细名称『{name}』"),
            }),
            desc: i18n::render(|l| match l {
                Lang::Japanese => format!("『{parent}』は登録しません。回ごとに、別のタグになります。"),
                Lang::English => format!("‘{parent}’ is not registered. Each installment becomes a separate tag."),
                Lang::Chinese => format!("不登记『{parent}』。每一期会成为各自独立的标签。"),
            }),
        },
        ParentOption {
            mode: ParentMode::Both,
            title: i18n::render(|l| match l {
                Lang::Japanese => format!("上位名『{parent}』と、細かい名前『{name}』の両方を登録する"),
                Lang::English => format!("Register both the parent name ‘{parent}’ and the detailed name ‘{name}’"),
                Lang::Chinese => format!("同时登记上位名『{parent}』和详细名称『{name}』"),
            }),
            desc: i18n::render(|l| match l {
                Lang::Japanese => format!("同じファイルに、両方のタグが付きます。検索では、『{parent}』だけで全ての回、2つを選ぶと特定の回を探せます。"),
                Lang::English => format!("Both tags are attached to the same file. In search, ‘{parent}’ alone finds every installment; selecting both finds a specific one."),
                Lang::Chinese => format!("同一文件会带有两个标签。搜索时，只选『{parent}』可找到所有期数，同时选两个可找到特定的一期。"),
            }),
        },
        ParentOption {
            mode: ParentMode::ParentOnly,
            title: i18n::render(|l| match l {
                Lang::Japanese => format!("上位名『{parent}』だけを登録する"),
                Lang::English => format!("Register only the parent name ‘{parent}’"),
                Lang::Chinese => format!("只登记上位名『{parent}』"),
            }),
            desc: i18n::render(|l| match l {
                Lang::Japanese => format!("『{name}』のような回ごとの名前は登録せず、全ての回を『{parent}』として扱います。あとから、回ごとに分けることはできません。"),
                Lang::English => format!("Per-installment names such as ‘{name}’ are not registered; every installment is treated as ‘{parent}’. You cannot split them by installment later."),
                Lang::Chinese => format!("不登记『{name}』这类每一期的名称，而是将所有期数都视为『{parent}』。之后无法再按期数拆分。"),
            }),
        },
    ])
}

/// 照合の設定ポップアップ内の、区切り記号の管理。
pub(crate) fn separators_title() -> &'static str {
    i18n::tr("上位名の区切り記号", "Separators for parent names", "上位名的分隔符")
}
pub(crate) fn separators_note() -> &'static str {
    i18n::tr("名前の最後が「区切り記号＋番号」のとき（例：COMIC1★15）、区切り記号の前を、上位名の候補にします。変えると、候補の上位名が探し直されます。", "When a name ends with “separator + number” (e.g. COMIC1★15), the part before the separator becomes a parent-name candidate. Changing this re-detects parent names.", "当名称末尾是“分隔符＋数字”时（例：COMIC1★15），分隔符之前的部分会成为上位名的候选。更改后会重新查找上位名。")
}
pub(crate) fn separators_add_label() -> &'static str {
    i18n::tr("追加", "Add", "添加")
}
pub(crate) fn separators_add_hint() -> &'static str {
    i18n::tr("例：＠", "e.g. ＠", "例：＠")
}
pub(crate) fn separators_reset_label() -> &'static str {
    i18n::tr("既定に戻す", "Reset to default", "恢复默认")
}
pub(crate) fn separators_empty() -> &'static str {
    i18n::tr("区切り記号がありません。区切り記号が無い名前でも、同じ上位名の候補が2件以上あれば、上位名を提案します。", "No separators. Even for names without a separator, a parent name is suggested when two or more candidates share it.", "没有分隔符。即使名称中没有分隔符，只要有2个以上相同上位名的候选，也会提出上位名。")
}

// ── 登録先のカテゴリを選び直す ─────────────────────────────────────

/// カテゴリ振替ルールで決まった行の、結論文の添え書き。

/// 変更ダイアログのチェック（次回以降も同じ扱いにする）。
pub(crate) fn remember_label() -> &'static str {
    i18n::tr("次回以降も、この名前はこのカテゴリとして扱う", "From now on, treat this name as this category", "今后也将此名称视为此分类")
}
pub(crate) fn remember_hint() -> &'static str {
    i18n::tr("ルールとして保存します。管理と削除は、「⚙ 照合の設定」の中でできます。照合は名前の完全一致です（全角半角・大文字小文字・空白の違いは同じ名前として扱います）", "Saved as a rule. Manage and delete it under “⚙ Matching settings”. Matching is by exact name (differences in full/half width, case and spaces are ignored).", "将保存为规则。可在“⚙ 匹配设置”中管理和删除。匹配为名称完全一致（全角半角、大小写、空格的差异视为同名）。")
}
/// 変更ダイアログで、保存済みのルールがある候補に出す注意。
pub(crate) fn rule_exists_note() -> &'static str {
    i18n::tr("この名前には、保存したカテゴリ振替ルールがあります。", "A saved category-reassignment rule exists for this name.", "此名称已有保存的分类改派规则。")
}

/// 照合の設定ポップアップ内の、ルール管理の見出し・説明。
pub(crate) fn rules_section_title() -> &'static str {
    i18n::tr("カテゴリ振替ルール", "Category reassignment rules", "分类改派规则")
}
pub(crate) fn rules_section_note() -> &'static str {
    i18n::tr("「この名前は、このカテゴリとして扱う」と保存したルールです。次回の取得から有効で、いまの候補は変わりません。", "Rules saved as “treat this name as this category”. They take effect from the next scan; current candidates do not change.", "保存为“将此名称视为此分类”的规则。从下次获取起生效，当前候选不会改变。")
}
pub(crate) fn rules_empty() -> &'static str {
    i18n::tr("保存したルールはありません。", "No saved rules.", "没有已保存的规则。")
}
pub(crate) fn rules_delete_label() -> &'static str {
    i18n::tr("削除", "Delete", "删除")
}
pub(crate) fn rules_read_only() -> &'static str {
    i18n::tr("ルールのファイルを読めなかったため、ルールは保存できません（元のファイルは変更していません）。", "The rules file could not be read, so rules cannot be saved (the original file was not changed).", "无法读取规则文件，因此无法保存规则（原文件未被更改）。")
}

/// 一覧の1行。
pub(crate) fn rule_line(role: &str, name: &str, category: &str) -> String {
    format!("【{role}】『{name}』→『{category}』")
}

/// 使えなかったルールの知らせ（一覧の下端）。
pub(crate) fn missing_rules_notice(names: &[String]) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("カテゴリ振替ルール {}件は、移し先のカテゴリが見つからないため使いませんでした（{}）", names.len(), names.join("、")),
        Lang::English => format!("{} category reassignment rule(s) were not used because the target category was not found ({})", names.len(), names.join(", ")),
        Lang::Chinese => format!("有{}条分类改派规则因找不到目标分类而未被使用（{}）", names.len(), names.join("、")),
    })
}

pub(crate) fn move_title() -> &'static str {
    i18n::tr("別のカテゴリの要素として登録…", "Register as an element of another category…", "登记为其他分类的元素…")
}
pub(crate) fn move_pick_label() -> &'static str {
    i18n::tr("登録先のカテゴリ", "Target category", "登记到的分类")
}
pub(crate) fn move_pick_placeholder() -> &'static str {
    i18n::tr("（選んでください）", "(choose one)", "（请选择）")
}
pub(crate) fn move_no_target() -> &'static str {
    i18n::tr("移し先にできるカテゴリがありません。先にタグ管理でカテゴリを作ってから、選んでください。", "There is no category to move to. Create one in Tag Manager first, then choose it.", "没有可以移入的分类。请先在标签管理中创建分类，再进行选择。")
}

/// 「別のカテゴリの要素として登録」を出す候補か。形式・メインは、種類が決まっているので出さない。
pub(crate) fn can_move_category(c: &Candidate) -> bool {
    !matches!(c.role, Role::Format | Role::Main)
}

/// 移す前の（抽出された種類の）カテゴリ。
fn base_category(c: &Candidate) -> CatRef {
    c.category_origin.unwrap_or(c.category)
}

/// 移し先にできる既存カテゴリ（番号, 表示名）。メインとシステムのカテゴリ、移す前のカテゴリは除く。
pub(crate) fn move_targets(c: &Candidate, master: &[TagManagerCategoryUi]) -> Vec<(usize, String)> {
    if !can_move_category(c) {
        return Vec::new();
    }
    let base = match base_category(c) {
        CatRef::Existing(i) => Some(i),
        CatRef::New => None,
    };
    master
        .iter()
        .enumerate()
        .filter(|(i, cat)| !cat.is_main && cat.system_kind.is_none() && Some(*i) != base)
        .map(|(i, cat)| (i, cat.display_name()))
        .collect()
}

/// 「別のカテゴリの要素として登録」の説明（1〜2文）。
pub(crate) fn move_desc(c: &Candidate, master: &[TagManagerCategoryUi]) -> String {
    i18n::render(|l| {
        let cat = match base_category(c) {
            CatRef::Existing(i) => category_name(master, i, c.role, l),
            CatRef::New => c.role.new_category_name(l).to_string(),
        };
        let n = c.files.len();
        let reg = c.registration_name();
        match l {
            Lang::Japanese => format!("『{reg}』を、選んだカテゴリの新しい要素として登録し、{n}ファイルにタグ付けします。抽出された『{cat}』が、実際の種類と違うときに選びます。"),
            Lang::English => format!("Registers ‘{reg}’ as a new element of the category you choose and tags {n} files. Choose this when the extracted ‘{cat}’ is not the actual kind."),
            Lang::Chinese => format!("将『{reg}』登记为所选分类的新元素，并为{n}个文件添加标签。当提取出的『{cat}』与实际种类不符时选择此项。"),
        }
    })
}

pub(crate) fn bulk_move_label() -> &'static str {
    i18n::tr("まとめて別のカテゴリへ移す…", "Move several to another category…", "批量移到其他分类…")
}
pub(crate) fn bulk_move_title() -> &'static str {
    i18n::tr("まとめて別のカテゴリの要素として登録", "Register several as elements of another category", "批量登记为其他分类的元素")
}
pub(crate) fn bulk_move_ok_label() -> &'static str {
    i18n::tr("移す", "Move", "移动")
}

/// 一括移動ダイアログの案内。
pub(crate) fn bulk_move_intro(role: Role) -> String {
    i18n::render(|l| {
        let kind = role.new_category_name(l);
        match l {
        Lang::Japanese => format!("「{kind}」として抽出された候補のうち、実際は別の種類のものを、まとめて、別のカテゴリの新しい要素として登録するか、すでにある要素に結びつけます。対象の候補にチェックを入れてください。"),
        Lang::English => format!("Among the candidates extracted as “{kind}”, register those that are actually another kind as new elements of another category in one go, or link them to existing elements. Check the target candidates."),
        Lang::Chinese => format!("在被提取为“{kind}”的候选中，将实际属于其他种类的候选批量登记为其他分类的新元素，或关联到已有元素。请勾选目标候选。"),
        }
    })
}

/// 一括移動の確認文（選んだ内容を、ダイアログの中で確かめる）。
pub(crate) fn bulk_move_preview(count: usize, files: usize, category: Option<&str>) -> String {
    i18n::render(|l| match (count, category) {
        (0, _) => pick_str(l, "移す候補を選んでください", "Select candidates to move", "请选择要移动的候选"),
        (_, None) => match l {
            Lang::Japanese => format!("選んだ{count}件（合計{files}ファイル）の登録先のカテゴリを選んでください"),
            Lang::English => format!("Choose the target category for the {count} selected ({files} files in total)"),
            Lang::Chinese => format!("请为所选的{count}项（共{files}个文件）选择登记到的分类"),
        },
        (_, Some(cat)) => match l {
            Lang::Japanese => format!("選んだ{count}件を、『{cat}』カテゴリの新しい要素として登録し、合計{files}ファイルにタグ付けする"),
            Lang::English => format!("Register the {count} selected as new elements of ‘{cat}’ and tag {files} files in total"),
            Lang::Chinese => format!("将所选的{count}项登记为『{cat}』分类的新元素，并为共{files}个文件添加标签"),
        },
    })
}

/// 移し先を選んだときの結論文（ダイアログの中で、選んだ内容を確かめる）。
pub(crate) fn move_preview(c: &Candidate, master: &[TagManagerCategoryUi], cat_idx: usize) -> String {
    i18n::render(|l| {
        let cat = category_name(master, cat_idx, c.role, l);
        let reg = c.registration_name();
        match l {
            Lang::Japanese => format!("『{cat}』カテゴリに『{reg}』を新しい要素として登録し、ファイルにタグ付けする"),
            Lang::English => format!("Register ‘{reg}’ as a new element of ‘{cat}’ and tag the files"),
            Lang::Chinese => format!("将『{reg}』登记为『{cat}』分类的新元素，并为文件添加标签"),
        }
    })
}

// ── 既存の要素に結びつける（別名を足す） ──────────────────────────────

pub(crate) fn link_title() -> &'static str {
    i18n::tr("既存の要素に結びつける…", "Link to an existing element…", "关联到已有元素…")
}
pub(crate) fn link_pick_category() -> &'static str {
    i18n::tr("カテゴリ", "Category", "分类")
}
pub(crate) fn link_pick_element() -> &'static str {
    i18n::tr("結びつける要素", "Element to link to", "要关联的元素")
}
pub(crate) fn link_filter_hint() -> &'static str {
    i18n::tr("要素名で絞り込み", "Filter by element name", "按元素名称筛选")
}
pub(crate) fn link_no_element() -> &'static str {
    i18n::tr("このカテゴリには、結びつけられる要素がありません。", "This category has no element to link to.", "此分类中没有可关联的元素。")
}
/// 結びつけの注意（元に戻せる範囲と、メインの扱い）。
pub(crate) fn link_note() -> &'static str {
    i18n::tr("要素の名前が書き換わります。適用の試用中なら、「破棄」で元に戻せます（確定したあとは戻せません）。", "The element’s name will be rewritten. During a trial application you can undo it with “Discard” (not after you confirm).", "元素的名称会被改写。应用试用期间可通过“放弃”恢复原状（确定之后无法恢复）。")
}
pub(crate) fn link_main_note() -> &'static str {
    i18n::tr("メインカテゴリは、1つしか選べません。すでに別のメインタグが付いているファイルには、付きません。", "Only one main category can be chosen. Files that already have a different main tag are not tagged.", "主分类只能选择一个。已有其他主标签的文件不会被添加。")
}
/// 要素の一覧に出す件数の上限（万単位のときに重くしない。絞り込みで探してもらう）。
pub(crate) const LINK_LIST_CAP: usize = 200;

/// 結びつけ先にできるカテゴリ（番号, 表示名）。メインを含み、システム（アーカイブ形式）は除く。
pub(crate) fn link_categories(master: &[TagManagerCategoryUi]) -> Vec<(usize, String)> {
    master.iter().enumerate().filter(|(_, cat)| cat.system_kind.is_none()).map(|(i, cat)| (i, cat.display_name())).collect()
}

/// 結びつけの要素1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkElement {
    pub tier_id: u64,
    /// 別名を含む要素名の全体（ホバー用）。
    pub full: String,
    /// 画面に出す主表記。
    pub label: String,
}

/// カテゴリの要素を、名前（別名を含む）で絞り込んで並べる。照合は、全角半角・大文字小文字・空白を区別しない部分一致。
/// 戻り値は（`cap`件までの要素, 絞り込みに合った全件数）。
pub(crate) fn link_elements(master: &[TagManagerCategoryUi], cat_idx: usize, query: &str, cap: usize) -> (Vec<LinkElement>, usize) {
    let Some(cat) = master.get(cat_idx) else { return (Vec::new(), 0) };
    let q = crate::tag_master::normalize_key(query);
    let mut total = 0;
    let mut out = Vec::new();
    for t in &cat.tiers {
        let Some(full) = t.element.as_deref().filter(|e| !e.trim().is_empty()) else { continue };
        if !q.is_empty() && !crate::tag_master::normalize_key(full).contains(&q) {
            continue;
        }
        total += 1;
        if out.len() < cap {
            out.push(LinkElement { tier_id: t.id, full: full.to_string(), label: display_name(full).to_string() });
        }
    }
    (out, total)
}

pub(crate) fn bulk_mode_move() -> &'static str {
    i18n::tr("別のカテゴリの新しい要素として登録", "Register as new elements of another category", "登记为其他分类的新元素")
}
pub(crate) fn bulk_mode_link() -> &'static str {
    i18n::tr("既存の要素に結びつける（別名を足す）", "Link to an existing element (add as alias)", "关联到已有元素（添加为别名）")
}
pub(crate) fn bulk_link_desc() -> &'static str {
    i18n::tr("選んだ候補の名前を、すでにある1つの要素の別名として足し、その要素をファイルにタグ付けします。以後の取得でも、同じ要素として一致します。", "Adds the selected candidates’ names as aliases of one existing element, and tags the files with that element. Later scans match the same element.", "将所选候选的名称作为某个已有元素的别名添加，并为文件添加该元素的标签。之后的获取也会匹配到同一元素。")
}
pub(crate) fn bulk_link_ok_label() -> &'static str {
    i18n::tr("結びつける", "Link", "关联")
}

/// 一括の結びつけの確認文。`target` は（カテゴリ名, 要素名の全体）。
pub(crate) fn bulk_link_preview(count: usize, files: usize, target: Option<(&str, &str)>, names: &[String]) -> String {
    i18n::render(|l| match (count, target) {
        (0, _) => pick_str(l, "結びつける候補を選んでください", "Select candidates to link", "请选择要关联的候选"),
        (_, None) => match l {
            Lang::Japanese => format!("選んだ{count}件（合計{files}ファイル）の結びつけ先の要素を選んでください"),
            Lang::English => format!("Choose the element to link the {count} selected ({files} files in total) to"),
            Lang::Chinese => format!("请为所选的{count}项（共{files}个文件）选择要关联的元素"),
        },
        (_, Some((cat, full))) => {
            let mut all: Vec<String> = crate::tag_master::split_aliases(full).into_iter().map(str::to_string).collect();
            for n in names {
                if !all.iter().any(|a| crate::tag_master::normalize_key(a) == crate::tag_master::normalize_key(n)) {
                    all.push(n.clone());
                }
            }
            let joined = all.join("|");
            match l {
                Lang::Japanese => format!("選んだ{count}件を、{cat}の要素の別名として足し、合計{files}ファイルにタグ付けする（要素名は『{joined}』になる）"),
                Lang::English => format!("Add the {count} selected as aliases of the element in {cat} and tag {files} files in total (the element name becomes ‘{joined}’)"),
                Lang::Chinese => format!("将所选的{count}项添加为{cat}中该元素的别名，并为共{files}个文件添加标签（元素名称变为『{joined}』）"),
            }
        }
    })
}

/// 要素の一覧が上限で切れたときの案内。
pub(crate) fn link_more(rest: usize) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("ほか{rest}件あります。名前で絞り込んでください"),
        Lang::English => format!("{rest} more. Filter by name to narrow down."),
        Lang::Chinese => format!("另有{rest}项。请按名称筛选。"),
    })
}

/// 結びつけの説明（カードの本文）。
pub(crate) fn link_desc(c: &Candidate) -> String {
    i18n::render(|l| {
        let name = &c.name;
        let aliases = c.alias_list().join("|");
        let each = c.alias_list().join("』『");
        let n = c.files.len();
        match l {
            Lang::Japanese => format!("抽出された『{name}』を、すでにある別の要素と同じものとして扱います。選んだ要素に別名『{aliases}』を足して、{n}ファイルにタグ付けします。以後の取得でも、『{each}』は同じ要素として一致します。"),
            Lang::English => format!("Treats the extracted ‘{name}’ as the same thing as another existing element. Adds the alias ‘{aliases}’ to the element you choose and tags {n} files. Later scans also match ‘{each}’ to the same element."),
            Lang::Chinese => format!("将提取出的『{name}』视为与已有的另一个元素相同。为所选元素添加别名『{aliases}』，并为{n}个文件添加标签。之后的获取中，『{each}』也会匹配到同一元素。"),
        }
    })
}

/// 結びつけを選んだときの結論文（ダイアログの中で、選んだ内容を確かめる）。
pub(crate) fn link_preview(c: &Candidate, master: &[TagManagerCategoryUi], cat_idx: usize, tier_id: u64) -> String {
    i18n::render(|l| {
        let aliases = c.alias_list().join("|");
        let el = element_label(master, tier_id);
        let cat = category_name(master, cat_idx, c.role, l);
        let full = element_full(master, tier_id);
        let n = c.files.len();
        match l {
            Lang::Japanese => format!("{cat}の要素『{el}』を『{full}|{aliases}』にして、{n}ファイルにタグ付けする"),
            Lang::English => format!("Change the element ‘{el}’ of {cat} to ‘{full}|{aliases}’ and tag {n} files"),
            Lang::Chinese => format!("将{cat}中的元素『{el}』改为『{full}|{aliases}』，并为{n}个文件添加标签"),
        }
    })
}

// ── 名前の取り出し ──────────────────────────────────────────────────

/// カテゴリの表示名。番号が範囲外のときは、種類の既定名。
fn category_name(master: &[TagManagerCategoryUi], cat_idx: usize, role: Role, l: Lang) -> String {
    master.get(cat_idx).map(|c| c.display_name()).unwrap_or_else(|| role.new_category_name(l).to_string())
}

/// 候補が入るカテゴリの表示名（まだ無いカテゴリなら、作るときの名前）。
fn candidate_category_name(c: &Candidate, master: &[TagManagerCategoryUi], l: Lang) -> String {
    match c.category {
        CatRef::Existing(i) => category_name(master, i, c.role, l),
        CatRef::New => c.role.new_category_name(l).to_string(),
    }
}

fn element_full(master: &[TagManagerCategoryUi], tier_id: u64) -> String {
    master
        .iter()
        .flat_map(|c| c.tiers.iter())
        .find(|t| t.id == tier_id)
        .and_then(|t| t.element.clone())
        .unwrap_or_else(|| format!("#{tier_id}"))
}

/// 要素の主表記（別名つきなら先頭の別名）。
fn element_label(master: &[TagManagerCategoryUi], tier_id: u64) -> String {
    display_name(&element_full(master, tier_id)).to_string()
}

// ── 行の結論文 ──────────────────────────────────────────────────────

/// 行の2段目に出す、「何を・どこに・どうするか」の1文。矢印は付けない。上位名を登録する設定なら、その旨を添える。
pub(crate) fn conclusion(c: &Candidate, master: &[TagManagerCategoryUi]) -> String {
    i18n::render(|l| conclusion_for(c, master, l))
}

fn conclusion_for(c: &Candidate, master: &[TagManagerCategoryUi], l: Lang) -> String {
    match (c.parent_name.as_deref(), c.parent_mode) {
        (Some(parent), ParentMode::Both) if c.is_adopted() => {
            let base = conclusion_base(c, master, l);
            match l {
                Lang::Japanese => format!("{base}（上位名『{parent}』も登録し、タグ付けする）"),
                Lang::English => format!("{base} (the parent name ‘{parent}’ is also registered and tagged)"),
                Lang::Chinese => format!("{base}（同时登记上位名『{parent}』并添加标签）"),
            }
        }
        (Some(parent), ParentMode::ParentOnly) if c.is_adopted() => match c.category {
            CatRef::New => {
                let newcat = c.role.new_category_name(l);
                let name = &c.name;
                match l {
                    Lang::Japanese => format!("新しいカテゴリ『{newcat}』を作成し、上位名『{parent}』をその要素として登録して、ファイルにタグ付けする（『{name}』は登録しない）"),
                    Lang::English => format!("Create the new category ‘{newcat}’, register the parent name ‘{parent}’ as its element and tag the files (‘{name}’ is not registered)"),
                    Lang::Chinese => format!("创建新分类『{newcat}』，将上位名『{parent}』登记为其元素并为文件添加标签（不登记『{name}』）"),
                }
            }
            CatRef::Existing(_) => {
                let cat = candidate_category_name(c, master, l);
                let name = &c.name;
                match l {
                    Lang::Japanese => format!("『{cat}』カテゴリに上位名『{parent}』を新しい要素として登録し、ファイルにタグ付けする（『{name}』は登録しない）"),
                    Lang::English => format!("Register the parent name ‘{parent}’ as a new element of ‘{cat}’ and tag the files (‘{name}’ is not registered)"),
                    Lang::Chinese => format!("将上位名『{parent}』登记为『{cat}』分类的新元素并为文件添加标签（不登记『{name}』）"),
                }
            }
        },
        _ => conclusion_base(c, master, l),
    }
}

/// 上位名を考えない、細かい名前の結論文。
fn conclusion_base(c: &Candidate, master: &[TagManagerCategoryUi], l: Lang) -> String {
    match &c.choice {
        Choice::Existing { cat_idx, tier_id } => {
            let cat = category_name(master, *cat_idx, c.role, l);
            let el = element_label(master, *tier_id);
            match l {
                Lang::Japanese => format!("{cat}『{el}』（既存の要素）を、ファイルにタグ付けする"),
                Lang::English => format!("Tag the files with {cat} ‘{el}’ (existing element)"),
                Lang::Chinese => format!("为文件添加{cat}『{el}』（已有元素）的标签"),
            }
        }
        Choice::Alias { cat_idx, tier_id } => {
            let cat = category_name(master, *cat_idx, c.role, l);
            let el = element_label(master, *tier_id);
            let aliases = c.alias_list().join("|");
            match l {
                Lang::Japanese => format!("既存の{cat}要素『{el}』に、別名『{aliases}』を足して、ファイルにタグ付けする"),
                Lang::English => format!("Add the alias ‘{aliases}’ to the existing {cat} element ‘{el}’ and tag the files"),
                Lang::Chinese => format!("为已有的{cat}元素『{el}』添加别名『{aliases}』，并为文件添加标签"),
            }
        }
        Choice::New => match c.category {
            CatRef::New => {
                let newcat = c.role.new_category_name(l);
                let reg = c.registration_name();
                match l {
                    Lang::Japanese => format!("新しいカテゴリ『{newcat}』を作成し、『{reg}』をその要素として登録して、ファイルにタグ付けする"),
                    Lang::English => format!("Create the new category ‘{newcat}’, register ‘{reg}’ as its element and tag the files"),
                    Lang::Chinese => format!("创建新分类『{newcat}』，将『{reg}』登记为其元素并为文件添加标签"),
                }
            }
            CatRef::Existing(_) => {
                let cat = candidate_category_name(c, master, l);
                let reg = c.registration_name();
                let suffix = if c.category_rule { rule_suffix_for(l) } else { "" };
                match l {
                    Lang::Japanese => format!("『{cat}』カテゴリに『{reg}』を新しい要素として登録し、ファイルにタグ付けする{suffix}"),
                    Lang::English => format!("Register ‘{reg}’ as a new element of ‘{cat}’ and tag the files{suffix}"),
                    Lang::Chinese => format!("将『{reg}』登记为『{cat}』分类的新元素，并为文件添加标签{suffix}"),
                }
            }
        },
        Choice::Skip => pick_str(l, "ファイルにタグ付けせず、マスターにも登録しない", "Do not tag the files and do not register in the master", "不为文件添加标签，也不登记到主表"),
    }
}

fn rule_suffix_for(l: Lang) -> &'static str {
    i18n::pick(l, "（保存したルールにより）", " (by a saved rule)", "（根据已保存的规则）")
}

// ── 変更ダイアログ ──────────────────────────────────────────────────

/// 変更ダイアログの選択肢1つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolveOption {
    /// 選ぶと `Candidate::choice` に入る値。
    pub choice: Choice,
    /// 選択肢の見出し（結論文と同じ形）。
    pub title: String,
    /// 2行目以降の説明（1〜2文）。
    pub desc: String,
}

/// 変更ダイアログの見出し。状態（採用・不採用）に依らない言い方にする。
pub(crate) fn resolve_heading(c: &Candidate) -> String {
    i18n::render(|l| match l {
        Lang::Japanese => format!("「{}」（{}ファイルにタグ付けする候補）をどうしますか", c.name, c.files.len()),
        Lang::English => format!("What should be done with “{}” (a candidate that tags {} files)?", c.name, c.files.len()),
        Lang::Chinese => format!("要如何处理“{}”（将为{}个文件添加标签的候选）？", c.name, c.files.len()),
    })
}

/// 類似提案があるときの、似ている度合いの注記。
pub(crate) fn similarity_note(c: &Candidate) -> Option<String> {
    if c.suggestions.is_empty() {
        return None;
    }
    let parts: Vec<String> = c.suggestions.iter().map(|s| format!("『{}』 {}%", primary_alias(&s.name), s.score)).collect();
    let joined = parts.join(" / ");
    Some(i18n::render(|l| match l {
        Lang::Japanese => format!("似ている度合い：{joined}"),
        Lang::English => format!("Similarity: {joined}"),
        Lang::Chinese => format!("相似度：{joined}"),
    }))
}

/// 変更ダイアログに並べる選択肢。類似提案ごとの「既存に付ける」「別名として足す」、新規、不採用の順。
/// いまの選択（類似提案に無い完全一致など）も、先頭に含める。同じ選択肢は1つにまとめる。
pub(crate) fn resolve_options(c: &Candidate, master: &[TagManagerCategoryUi]) -> Vec<ResolveOption> {
    let n = c.files.len();
    let mut targets: Vec<(usize, u64)> = Vec::new();
    let mut push_target = |cat_idx: usize, tier_id: u64| {
        if !targets.contains(&(cat_idx, tier_id)) {
            targets.push((cat_idx, tier_id));
        }
    };
    for ch in [&c.choice, &c.initial] {
        if let Choice::Existing { cat_idx, tier_id } | Choice::Alias { cat_idx, tier_id } = ch {
            push_target(*cat_idx, *tier_id);
        }
    }
    for s in &c.suggestions {
        push_target(s.cat_idx, s.tier_id);
    }

    let aliases = c.alias_list().join("|");
    let reg = c.registration_name();
    let name = &c.name;
    let mut out: Vec<ResolveOption> = Vec::new();
    for (cat_idx, tier_id) in targets {
        let el = element_label(master, tier_id);
        let full = element_full(master, tier_id);
        out.push(ResolveOption {
            choice: Choice::Existing { cat_idx, tier_id },
            title: i18n::render(|l| {
                let cat = category_name(master, cat_idx, c.role, l);
                match l {
                    Lang::Japanese => format!("{cat}『{el}』（既存の要素）をファイルにタグ付け"),
                    Lang::English => format!("Tag the files with {cat} ‘{el}’ (existing element)"),
                    Lang::Chinese => format!("为文件添加{cat}『{el}』（已有元素）的标签"),
                }
            }),
            desc: i18n::render(|l| {
                let cat = category_name(master, cat_idx, c.role, l);
                match l {
                    Lang::Japanese => format!("「{name}」の{n}ファイルに、{cat}の『{el}』が付きます。マスターは変わりません。同じ人（もの）と分かっているときに選びます。"),
                    Lang::English => format!("The {n} files of “{name}” get ‘{el}’ of {cat}. The master does not change. Choose this when you know they are the same person (thing)."),
                    Lang::Chinese => format!("“{name}”的{n}个文件会被添加{cat}的『{el}』。主表不会改变。确定是同一个人（事物）时选择此项。"),
                }
            }),
        });
        out.push(ResolveOption {
            choice: Choice::Alias { cat_idx, tier_id },
            title: i18n::render(|l| {
                let cat = category_name(master, cat_idx, c.role, l);
                match l {
                    Lang::Japanese => format!("既存の{cat}要素『{el}』に別名『{aliases}』を足して、ファイルにタグ付け"),
                    Lang::English => format!("Add the alias ‘{aliases}’ to the existing {cat} element ‘{el}’ and tag the files"),
                    Lang::Chinese => format!("为已有的{cat}元素『{el}』添加别名『{aliases}』，并为文件添加标签"),
                }
            }),
            desc: i18n::render(|l| match l {
                Lang::Japanese => format!("要素名が『{full}|{aliases}』に変わり、{n}ファイルに付きます。以後、『{aliases}』も同じタグとして扱います。"),
                Lang::English => format!("The element name becomes ‘{full}|{aliases}’ and is attached to {n} files. From now on ‘{aliases}’ is treated as the same tag."),
                Lang::Chinese => format!("元素名称会变为『{full}|{aliases}』，并添加到{n}个文件。之后『{aliases}』也会被视为同一标签。"),
            }),
        });
    }
    out.push(match base_category(c) {
        CatRef::New => ResolveOption {
            choice: Choice::New,
            title: i18n::render(|l| {
                let newcat = c.role.new_category_name(l);
                match l {
                    Lang::Japanese => format!("新しいカテゴリ『{newcat}』を作成し、『{reg}』をその要素として登録して、ファイルにタグ付け"),
                    Lang::English => format!("Create the new category ‘{newcat}’, register ‘{reg}’ as its element and tag the files"),
                    Lang::Chinese => format!("创建新分类『{newcat}』，将『{reg}』登记为其元素并为文件添加标签"),
                }
            }),
            desc: i18n::render(|l| {
                let newcat = c.role.new_category_name(l);
                match l {
                    Lang::Japanese => format!("カテゴリ『{newcat}』と要素『{reg}』が増え、{n}ファイルに付きます。"),
                    Lang::English => format!("The category ‘{newcat}’ and the element ‘{reg}’ are added and attached to {n} files."),
                    Lang::Chinese => format!("会新增分类『{newcat}』和元素『{reg}』，并添加到{n}个文件。"),
                }
            }),
        },
        CatRef::Existing(base_idx) => ResolveOption {
            choice: Choice::New,
            title: i18n::render(|l| {
                let cat = category_name(master, base_idx, c.role, l);
                match l {
                    Lang::Japanese => format!("『{reg}』を『{cat}』カテゴリの新しい要素として登録し、ファイルにタグ付け"),
                    Lang::English => format!("Register ‘{reg}’ as a new element of ‘{cat}’ and tag the files"),
                    Lang::Chinese => format!("将『{reg}』登记为『{cat}』分类的新元素，并为文件添加标签"),
                }
            }),
            desc: i18n::render(|l| {
                let cat = category_name(master, base_idx, c.role, l);
                let separate = if c.suggestions.is_empty() {
                    ""
                } else {
                    i18n::pick(l, "既存のタグとは別のものとして扱います。", " It is treated as separate from the existing tags.", "将其视为与已有标签不同的事物。")
                };
                match l {
                    Lang::Japanese => format!("『{cat}』に『{reg}』が増え、{n}ファイルに付きます。{separate}"),
                    Lang::English => format!("‘{reg}’ is added to ‘{cat}’ and attached to {n} files.{separate}"),
                    Lang::Chinese => format!("会在『{cat}』中新增『{reg}』，并添加到{n}个文件。{separate}"),
                }
            }),
        },
    });
    out.push(ResolveOption {
        choice: Choice::Skip,
        title: i18n::render(|l| match l {
            Lang::Japanese => format!("ファイルにタグ付けしない（{}）", reject_for(l)),
            Lang::English => format!("Do not tag the files ({})", reject_for(l)),
            Lang::Chinese => format!("不为文件添加标签（{}）", reject_for(l)),
        }),
        desc: i18n::render(|l| match l {
            Lang::Japanese => format!("{n}ファイルにはタグが付かず、マスターにも登録しません。次回の取得で、また候補に出ます。"),
            Lang::English => format!("The {n} files get no tag and nothing is registered in the master. The candidate appears again in the next scan."),
            Lang::Chinese => format!("这{n}个文件不会被添加标签，也不会登记到主表。下次获取时仍会作为候选出现。"),
        }),
    });
    out
}

// ── 要確認 ──────────────────────────────────────────────────────────

/// 「要確認」の種類。ユーザーに判断を求める候補だけが持つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReviewKind {
    /// タグ抽出ルールで無効にされた（ルール名）。
    Rule(String),
    /// 学習で「タグにしない」と判定された。
    Learned,
    /// 既存のタグに似ていて、アプリには同じかどうか判断できない。
    Similar { name: String, score: u8 },
    /// 作者・グループとしての確証が無い。
    Unconfirmed,
}

/// 候補の「要確認」の種類。要確認でなければ None。
pub(crate) fn review_kind(c: &Candidate) -> Option<ReviewKind> {
    if c.review {
        return Some(match &c.rule_reason {
            Some(rule) => ReviewKind::Rule(rule.clone()),
            None => ReviewKind::Learned,
        });
    }
    if let Some(s) = c.suggestions.first()
        && c.initial == Choice::Skip
    {
        return Some(ReviewKind::Similar { name: primary_alias(&s.name).to_string(), score: s.score });
    }
    if c.unconfirmed {
        return Some(ReviewKind::Unconfirmed);
    }
    None
}

/// 要確認か。
pub(crate) fn needs_review(c: &Candidate) -> bool {
    review_kind(c).is_some()
}

/// 行の2段目に出す、要確認の短い理由（「要確認：」は呼び出し側で付ける）。
#[cfg(test)]
pub(crate) fn review_short(kind: &ReviewKind) -> String {
    i18n::render(|l| review_short_for(kind, l))
}

fn review_short_for(kind: &ReviewKind, l: Lang) -> String {
    match kind {
        ReviewKind::Rule(rule) => match l {
            Lang::Japanese => format!("ルール「{rule}」で無効"),
            Lang::English => format!("disabled by rule “{rule}”"),
            Lang::Chinese => format!("被规则“{rule}”停用"),
        },
        ReviewKind::Learned => pick_str(l, "学習により「タグではない」と判定", "judged “not a tag” by learning", "经学习被判定为“不是标签”"),
        ReviewKind::Similar { .. } => pick_str(l, "似た名前が既存にあります", "a similar name already exists", "已存在相似的名称"),
        ReviewKind::Unconfirmed => pick_str(l, "作者かグループか未確定", "author or group is undetermined", "作者还是团体尚未确定"),
    }
}

/// 「理由を見る」ポップアップの長文。（見出し, 本文）の並び。
pub(crate) fn review_long(kind: &ReviewKind, c: &Candidate, master: &[TagManagerCategoryUi]) -> Vec<(String, String)> {
    let name = &c.name;
    // 見出し1つぶんを、3言語で作る。
    let head = |ja: &'static str, en: &'static str, zh: &'static str| i18n::tr(ja, en, zh).to_string();
    // 「タグ不採用」と、手動で採用にする案内。
    let reject = |l: Lang| match l {
        Lang::Japanese => format!("「{}」（ファイルにタグ付けしない）", reject_for(l)),
        Lang::English => format!("“{}” (do not tag the files)", reject_for(l)),
        Lang::Chinese => format!("“{}”（不为文件添加标签）", reject_for(l)),
    };
    let adopt_hint = |l: Lang| match l {
        Lang::Japanese => format!("ファイルにタグ付けしたいときは、「解決する…」から「{}」にします。", adopt_for(l)),
        Lang::English => format!("To tag the files, use “Resolve…” and set it to “{}”.", adopt_for(l)),
        Lang::Chinese => format!("想为文件添加标签时，请通过“解决…”将其设为“{}”。", adopt_for(l)),
    };
    match kind {
        ReviewKind::Rule(rule) => vec![
            (
                head("何が起きているか", "What is happening", "发生了什么"),
                i18n::render(|l| {
                    let role = c.role.new_category_name(l);
                    match l {
                        Lang::Japanese => format!("ルール『{rule}』が、このファイルに当てはまりました。このルールは、この種類（{role}）の名前をタグにしない設定です。『{name}』は、初期は{}にしてあります。", reject(l)),
                        Lang::English => format!("The rule ‘{rule}’ matched this file. This rule is set not to make names of this kind ({role}) into tags. ‘{name}’ is initially set to {}.", reject(l)),
                        Lang::Chinese => format!("规则『{rule}』适用于此文件。该规则的设定是不将此种类（{role}）的名称作为标签。『{name}』初始设为{}。", reject(l)),
                    }
                }),
            ),
            (
                head("なぜそうしているか", "Why it is set this way", "为什么这样设置"),
                i18n::tr(
                    "ルールで無効にされた種類は、ファイル名の中の名前が実際の内容を表さないことがあるため、自動ではタグ付けしません。",
                    "For kinds disabled by a rule, the name in the file name may not reflect the actual content, so they are not tagged automatically.",
                    "被规则停用的种类，其文件名中的名称有时并不能代表实际内容，因此不会自动添加标签。",
                )
                .to_string(),
            ),
            (
                head("どうすればよいか", "What to do", "该怎么办"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("{}ルールの設定そのものは変わりません。", adopt_hint(l)),
                    Lang::English => format!("{} The rule itself does not change.", adopt_hint(l)),
                    Lang::Chinese => format!("{}规则本身的设置不会改变。", adopt_hint(l)),
                }),
            ),
        ],
        ReviewKind::Learned => vec![
            (
                head("何が起きているか", "What is happening", "发生了什么"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("『{name}』は、あなたが過去に振り分けたファイル名のパターンと似ています。そのパターンでは、この位置の括弧は「タグにしない」ものとして振り分けられていました。初期は{}にしてあります。", reject(l)),
                    Lang::English => format!("‘{name}’ resembles a file-name pattern you sorted in the past. In that pattern, the brackets in this position were sorted as “not a tag”. It is initially set to {}.", reject(l)),
                    Lang::Chinese => format!("『{name}』与您过去分类过的文件名模式相似。在该模式中，此位置的括号被归为“不作为标签”。初始设为{}。", reject(l)),
                }),
            ),
            (head("どうすればよいか", "What to do", "该怎么办"), i18n::render(|l| adopt_hint(l))),
        ],
        ReviewKind::Similar { name: best, score } => vec![
            (
                head("何が起きているか", "What is happening", "发生了什么"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("『{name}』は、すでにマスターにある『{best}』と名前がよく似ています（{score}%）。"),
                    Lang::English => format!("‘{name}’ is very similar in name to ‘{best}’, which is already in the master ({score}%)."),
                    Lang::Chinese => format!("『{name}』与主表中已有的『{best}』名称非常相似（{score}%）。"),
                }),
            ),
            (
                head("なぜ確認が必要か", "Why a check is needed", "为什么需要确认"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("同じ人（もの）の表記ゆれかもしれませんし、別のものかもしれません。アプリには判断できないため、初期は{}にしてあります。", reject(l)),
                    Lang::English => format!("It may be a spelling variant of the same person (thing), or it may be something different. The app cannot tell, so it is initially set to {}.", reject(l)),
                    Lang::Chinese => format!("这可能是同一个人（事物）的写法差异，也可能是不同的事物。应用无法判断，因此初始设为{}。", reject(l)),
                }),
            ),
            (
                head("どうすればよいか", "What to do", "该怎么办"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("「解決する…」を押して、同じものなら「既存の要素をタグ付け」か「別名として足してタグ付け」、別のものなら「新しい要素として登録してタグ付け」を選びます。分からなければ{}のままで構いません。そのファイルにはタグが付きません。", reject(l)),
                    Lang::English => format!("Press “Resolve…” and, if they are the same, choose “Tag with the existing element” or “Add as alias and tag”; if they differ, choose “Register as a new element and tag”. If you are not sure, you can leave it as {}. The file then gets no tag.", reject(l)),
                    Lang::Chinese => format!("点击“解决…”：如果是同一事物，请选择“为已有元素添加标签”或“添加为别名并添加标签”；如果是不同的事物，请选择“登记为新元素并添加标签”。不确定时，保持{}即可，该文件不会被添加标签。", reject(l)),
                }),
            ),
        ],
        ReviewKind::Unconfirmed => vec![
            (
                head("何が起きているか", "What is happening", "发生了什么"),
                i18n::render(|l| {
                    let cat = candidate_category_name(c, master, l);
                    let state = if c.initial == Choice::Skip {
                        reject(l)
                    } else {
                        match l {
                            Lang::Japanese => format!("「{}」", adopt_for(l)),
                            Lang::English => format!("“{}”", adopt_for(l)),
                            Lang::Chinese => format!("“{}”", adopt_for(l)),
                        }
                    };
                    match l {
                        Lang::Japanese => format!("『{name}』は、ファイル名の先頭にある［ ］が1つだけで、作者名なのかグループ（サークル）名なのかを判断する手がかりがありませんでした。いまは{cat}として、{state}にしてあります。"),
                        Lang::English => format!("‘{name}’ is the only [ ] at the start of the file name, and there was no clue to tell whether it is an author name or a group (circle) name. It is currently treated as {cat} and set to {state}."),
                        Lang::Chinese => format!("『{name}』是文件名开头唯一的［ ］，没有线索可以判断它是作者名还是团体（社团）名。目前将其视为{cat}，并设为{state}。"),
                    }
                }),
            ),
            (
                head("どうすればよいか", "What to do", "该怎么办"),
                i18n::render(|l| match l {
                    Lang::Japanese => format!("そのままでよければ、何もしなくて構いません。違うと分かっているときは「{}」にして、あとでタグパネルから手動でタグ付けしてください。", reject_for(l)),
                    Lang::English => format!("If it is fine as it is, you do not need to do anything. If you know it is wrong, set it to “{}” and tag it by hand later from the tag panel.", reject_for(l)),
                    Lang::Chinese => format!("如果保持现状即可，无需任何操作。如果确定不对，请设为“{}”，之后再通过标签面板手动添加标签。", reject_for(l)),
                }),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_candidates::Suggestion;
    use crate::view_explorer::TagManagerTierUi;

    fn tier(id: u64, name: &str) -> TagManagerTierUi {
        TagManagerTierUi { id, tier_no: 1, negative: false, element: Some(name.to_string()) }
    }
    fn cat(name: &str, is_main: bool, tiers: Vec<TagManagerTierUi>) -> TagManagerCategoryUi {
        TagManagerCategoryUi { name: name.to_string(), tiers, color: egui::Color32::GRAY, is_main, single_select: false, system_kind: None }
    }
    /// 0=メイン、1=作者（Kouji）、2=イベント。
    fn master() -> Vec<TagManagerCategoryUi> {
        vec![
            cat("main", true, vec![tier(1, "同人誌")]),
            cat("作者", false, vec![tier(10, "Kouji"), tier(11, "Taro|Tarou")]),
            cat("イベント", false, vec![tier(20, "C104")]),
        ]
    }
    fn base(role: Role, name: &str, choice: Choice) -> Candidate {
        Candidate {
            role,
            name: name.to_string(),
            forms: vec![name.to_string()],
            key: name.to_lowercase(),
            files: vec![0, 1, 2, 3, 4],
            category: CatRef::Existing(1),
            initial: choice.clone(),
            choice,
            suggestions: Vec::new(),
            unconfirmed: false,
            cluster: 0,
            aliases: Vec::new(),
            members: Vec::new(),
            bundle_id: None,
            review: false,
            alt_forms: Vec::new(),
            rule_reason: None,
            category_origin: None,
            category_rule: false,
            parent_name: None,
            parent_mode: crate::tag_candidates::ParentMode::None,
        }
    }
    fn koji() -> Candidate {
        let mut c = base(Role::Author, "Koji", Choice::Skip);
        c.suggestions = vec![Suggestion { cat_idx: 1, tier_id: 10, name: "Kouji".to_string(), alias: "Kouji".to_string(), score: 92 }];
        c
    }

    #[test]
    fn conclusion_has_no_arrow_and_names_what_where() {
        let m = master();
        let existing = base(Role::Author, "Kouji", Choice::Existing { cat_idx: 1, tier_id: 10 });
        assert_eq!(conclusion(&existing, &m), "作者『Kouji』（既存の要素）を、ファイルにタグ付けする");
        let mut event = base(Role::Event, "C105", Choice::New);
        event.category = CatRef::Existing(2);
        assert_eq!(conclusion(&event, &m), "『イベント』カテゴリに『C105』を新しい要素として登録し、ファイルにタグ付けする");
        let alias = base(Role::Author, "Koji", Choice::Alias { cat_idx: 1, tier_id: 10 });
        assert_eq!(conclusion(&alias, &m), "既存の作者要素『Kouji』に、別名『Koji』を足して、ファイルにタグ付けする");
        let skip = base(Role::Author, "Koji", Choice::Skip);
        assert_eq!(conclusion(&skip, &m), "ファイルにタグ付けせず、マスターにも登録しない");
        for c in [&existing, &event, &alias, &skip] {
            assert!(!conclusion(c, &m).contains('→'), "矢印は付けない");
        }
    }

    #[test]
    fn conclusion_distinguishes_new_category_from_new_element() {
        let m = master();
        let mut p = base(Role::Performer, "Castorice", Choice::New);
        p.category = CatRef::New;
        p.alt_forms = vec!["カストリス".to_string()];
        assert_eq!(
            conclusion(&p, &m),
            "新しいカテゴリ『演者』を作成し、『Castorice|カストリス』をその要素として登録して、ファイルにタグ付けする"
        );
    }

    #[test]
    fn conclusion_uses_primary_alias_of_existing_element() {
        let m = master();
        let c = base(Role::Author, "Taro", Choice::Existing { cat_idx: 1, tier_id: 11 });
        assert!(conclusion(&c, &m).contains("『Taro』（既存の要素）"));
    }

    #[test]
    fn merge_texts_state_use_and_downsides() {
        let intro = merge_intro("doujinshi");
        assert!(intro.starts_with("『doujinshi』を、下記のタグと同じものとしてまとめます。"));
        assert!(intro.contains("同人誌|doujinshi"));
        let all = merge_consequences().join("");
        assert!(all.contains("単独に検索することはできなくなります"));
        assert!(all.contains("分離する機能はありません"));
        assert!(all.contains("必ず「タグ採用予定」"));
        assert_eq!(merge_preview(&["A"], 3), "2件以上を選んでください");
        assert_eq!(merge_preview(&["A", "B"], 7), "まとめる候補：A、B（合計7ファイル）");
        assert_eq!(cluster_header(3), "名前が似ている候補 3件");
    }

    #[test]
    fn summary_and_initial_view() {
        assert_eq!(summary(10, 4, 3), "タグ採用予定 10件 / マスターに新しく登録 4個 / タグ不採用 3件");
        assert_eq!(rule_hits_button(2), "ルールが働いた候補（2種）");
        assert_eq!(rule_hit_line("AI generated", 3), "AI generated：3ファイル");
        assert!(initial_view_is_review(true, 1));
        assert!(!initial_view_is_review(true, 0), "要確認が無ければ、すべてを表示");
        assert!(!initial_view_is_review(false, 5), "チェックを外すと、すべてを表示");
    }

    #[test]
    fn move_targets_exclude_main_system_and_origin_and_texts_use_the_target() {
        let mut m = master();
        m.push(cat("ジャンル", false, vec![tier(30, "青年コミック")]));
        let mut sys = cat("形式", false, vec![tier(40, "zip")]);
        sys.system_kind = Some(crate::tag_master::SystemKind::ArchiveFormat);
        m.push(sys);
        let mut c = base(Role::Event, "同人CG集", Choice::New);
        c.category = CatRef::Existing(2);
        // 移し先: 作者(1)・ジャンル(3)。メイン(0)・システム(4)・いまのカテゴリ(2)は除く。
        assert_eq!(move_targets(&c, &m), [(1, "作者".to_string()), (3, "ジャンル".to_string())]);
        assert_eq!(move_preview(&c, &m, 3), "『ジャンル』カテゴリに『同人CG集』を新しい要素として登録し、ファイルにタグ付けする");
        assert!(move_desc(&c, &m).contains("抽出された『イベント』が、実際の種類と違うときに選びます"));
        // 移したあとでも、移す前のカテゴリは除かれ、「新規」の選択肢は移す前のカテゴリで書く。
        c.move_to_category(3);
        assert_eq!(move_targets(&c, &m), [(1, "作者".to_string())].into_iter().chain([(3, "ジャンル".to_string())]).collect::<Vec<_>>());
        let plain = resolve_options(&c, &m).into_iter().find(|o| o.choice == Choice::New).unwrap();
        assert_eq!(plain.title, "『同人CG集』を『イベント』カテゴリの新しい要素として登録し、ファイルにタグ付け");
        assert_eq!(conclusion(&c, &m), "『ジャンル』カテゴリに『同人CG集』を新しい要素として登録し、ファイルにタグ付けする");
        // 形式・メインの種類には出さない。
        assert!(!can_move_category(&base(Role::Format, "zip", Choice::New)));
        assert!(move_targets(&base(Role::Main, "同人誌", Choice::New), &m).is_empty());
    }

    #[test]
    fn intro_faq_precheck_and_limit_texts() {
        assert!(intro_paras()[0].contains("「タグ採用予定」") && intro_paras()[0].contains("適用するまで、ファイルは変わりません"));
        assert!(intro_paras()[1].contains("登録もしません"));
        assert!(failed_paras()[2].contains("この自動抽出で足したタグだけ"));
        assert_eq!(faq_items().len(), 3);
        assert!(faq_items().iter().all(|(q, a)| q.ends_with('？') && !a.is_empty()));
        assert!(precheck_body(4).contains("4件"));
        let body = limit_body(5000, "イベント：新規6000個", "12.3 MB", 1000);
        assert!(body.contains("5000個") && body.contains("イベント：新規6000個") && body.contains("12.3 MB"));
        assert!(body.contains("1000個拡張する（おすすめ）") && body.contains("これ以上大きくなりません"));
    }

    #[test]
    fn link_categories_include_main_and_exclude_system() {
        let mut m = master();
        let mut sys = cat("形式", false, vec![tier(40, "zip")]);
        sys.system_kind = Some(crate::tag_master::SystemKind::ArchiveFormat);
        m.push(sys);
        let names: Vec<String> = link_categories(&m).into_iter().map(|c| c.1).collect();
        assert_eq!(names, ["メイン", "作者", "イベント"], "メインを含み、システムは除く");
    }

    #[test]
    fn link_elements_filter_by_normalized_name_including_aliases_and_cap() {
        let mut m = master();
        m[0].tiers.push(tier(2, "MANGA|漫画"));
        m[0].tiers.push(TagManagerTierUi { id: 3, tier_no: 2, negative: false, element: None });
        // 空の要素は出ない。絞り込みなしなら全件。
        let (all, total) = link_elements(&m, 0, "", 10);
        assert_eq!((all.len(), total), (2, 2));
        // 別名にも当たり、全角半角・大文字小文字を区別しない。
        let (hit, total) = link_elements(&m, 0, "ｍａｎ", 10);
        assert_eq!(total, 1);
        assert_eq!(hit[0], LinkElement { tier_id: 2, full: "MANGA|漫画".to_string(), label: "MANGA".to_string() });
        assert_eq!(link_elements(&m, 0, "漫画", 10).1, 1);
        assert_eq!(link_elements(&m, 0, "なし", 10).1, 0);
        // 上限は表示件数だけを切り、全件数は数える。
        let (cut, total) = link_elements(&m, 0, "", 1);
        assert_eq!((cut.len(), total), (1, 2));
        assert_eq!(link_elements(&m, 99, "", 10), (Vec::new(), 0), "範囲外のカテゴリは空");
    }

    fn input(name: &str) -> CandidateInput {
        CandidateInput { file_name: name.to_string(), format: None, info: None, meta: None }
    }

    #[test]
    fn source_file_prefers_a_file_containing_the_form_and_marks_it() {
        let inputs = vec![input("[Other] a.zip"), input("(C105) [Circle (Kouji)] b.zip"), input("[Kouji] c.zip")];
        let mut c = base(Role::Author, "Kouji", Choice::New);
        c.files = vec![0, 1, 2];
        // 先頭のファイルには表記が無い。表記を含む2番目を選び、範囲を返す。
        let s = source_file(&c, &inputs).unwrap();
        assert_eq!(s.name, "(C105) [Circle (Kouji)] b.zip");
        let (a, b) = s.range.unwrap();
        assert_eq!(&s.name[a..b], "Kouji");
        assert_eq!(s.others, 2, "代表の1件のほかに2件");
        // 大文字小文字は区別しない。
        let mut lower = base(Role::Author, "kouji", Choice::New);
        lower.forms = vec!["kouji".to_string()];
        lower.files = vec![1];
        let s = source_file(&lower, &inputs).unwrap();
        let (a, b) = s.range.unwrap();
        assert_eq!(&s.name[a..b], "Kouji");
        assert_eq!(s.others, 0);
    }

    #[test]
    fn source_file_falls_back_to_the_first_file_when_no_name_contains_the_form() {
        let inputs = vec![input("x.zip"), input("y.zip")];
        let mut c = base(Role::Language, "日本語", Choice::New);
        c.files = vec![1, 0];
        let s = source_file(&c, &inputs).unwrap();
        assert_eq!((s.name.as_str(), s.range, s.others), ("y.zip", None, 1), "候補のファイルの先頭");
        c.files.clear();
        assert_eq!(source_file(&c, &inputs), None);
        assert_eq!(source_others(3), "（他3件）");
    }

    #[test]
    fn source_file_scans_only_the_first_files_and_uses_merged_aliases() {
        // 表記を含むのが31件目以降なら、探さずに先頭のファイルにする（重くしない）。
        let mut inputs: Vec<CandidateInput> = (0..40).map(|i| input(&format!("f{i}.zip"))).collect();
        inputs[35] = input("[Kouji] z.zip");
        let mut c = base(Role::Author, "Kouji", Choice::New);
        c.files = (0..40).collect();
        assert_eq!(source_file(&c, &inputs).unwrap().range, None);
        // まとめた候補は、別名のどれかを含むファイルを選ぶ。
        let inputs = vec![input("a.zip"), input("[Koji] b.zip")];
        let mut m = base(Role::Author, "Kouji", Choice::New);
        m.aliases = vec!["Kouji".to_string(), "Koji".to_string()];
        m.files = vec![0, 1];
        let s = source_file(&m, &inputs).unwrap();
        assert_eq!(s.name, "[Koji] b.zip");
        // ホバーは先頭10件と、残りの件数。
        let (names, rest) = source_hover(&c, &inputs.iter().cloned().cycle().take(40).collect::<Vec<_>>());
        assert_eq!((names.len(), rest), (SOURCE_HOVER_CAP, 30));
    }

    #[test]
    fn footer_stage_labels_name_the_destination() {
        assert_eq!(footer_next_label(), "次へ：2/2 作者・グループの解決ページへ進む");
        assert_eq!(footer_back_label(), "戻る：1/2 形式・カテゴリ・言語・イベントの解決ページへ戻る");
    }

    #[test]
    fn file_count_short_text() {
        assert_eq!(file_count_short(3), "3件");
    }

    #[test]
    fn source_hover_line_is_numbered() {
        assert_eq!(source_hover_line(1, "a.zip"), "1. a.zip");
        assert_eq!(source_hover_heading(), "◆対象となるファイル");
    }

    #[test]
    fn bulk_link_preview_texts() {
        assert_eq!(bulk_link_preview(0, 0, None, &[]), "結びつける候補を選んでください");
        assert!(bulk_link_preview(2, 9, None, &[]).contains("結びつけ先の要素を選んでください"));
        let names = vec!["青年コミック".to_string(), "少女コミック".to_string(), "漫画".to_string()];
        // 要素にすでにある別名は、足さない。
        let p = bulk_link_preview(3, 9, Some(("メイン", "MANGA|漫画")), &names);
        assert!(p.contains("選んだ3件を、メインの要素の別名として足し、合計9ファイルにタグ付けする"));
        assert!(p.contains("『MANGA|漫画|青年コミック|少女コミック』になる"), "{p}");
    }

    #[test]
    fn link_texts_name_the_element_and_the_alias() {
        let mut m = master();
        m[0].tiers.push(tier(2, "MANGA"));
        let c = base(Role::Event, "青年コミック", Choice::New);
        assert_eq!(link_preview(&c, &m, 0, 2), "メインの要素『MANGA』を『MANGA|青年コミック』にして、5ファイルにタグ付けする");
        let d = link_desc(&c);
        assert!(d.contains("別名『青年コミック』を足して、5ファイルにタグ付けします"));
        assert!(link_note().contains("試用中") && link_main_note().contains("1つしか選べません"));
    }

    #[test]
    fn bulk_move_texts() {
        assert_eq!(bulk_move_preview(0, 0, None), "移す候補を選んでください");
        assert_eq!(bulk_move_preview(3, 7, None), "選んだ3件（合計7ファイル）の登録先のカテゴリを選んでください");
        assert_eq!(bulk_move_preview(3, 7, Some("ジャンル")), "選んだ3件を、『ジャンル』カテゴリの新しい要素として登録し、合計7ファイルにタグ付けする");
        assert!(bulk_move_intro(Role::Event).contains("「イベント」として抽出された候補"));
    }

    #[test]
    fn parent_texts_and_conclusions() {
        let m = master();
        let mut c = base(Role::Event, "COMIC1★15", Choice::New);
        c.category = CatRef::Existing(2);
        assert_eq!(parent_hint(&c), None, "上位名が無ければ出さない");
        c.parent_name = Some("COMIC1".to_string());
        assert_eq!(parent_hint(&c).as_deref(), Some("上位名の候補：『COMIC1』"));
        let opts = parent_options(&c).unwrap();
        assert_eq!(opts.iter().map(|o| o.mode).collect::<Vec<_>>(), [ParentMode::None, ParentMode::Both, ParentMode::ParentOnly]);
        assert_eq!(opts[0].title, "細かい名前『COMIC1★15』だけを登録する");
        assert_eq!(opts[1].title, "上位名『COMIC1』と、細かい名前『COMIC1★15』の両方を登録する");
        assert!(opts[1].desc.contains("『COMIC1』だけで全ての回"));
        assert!(opts[2].desc.contains("あとから、回ごとに分けることはできません"));
        // 結論文: 初期は従来どおり。
        let plain = conclusion(&c, &m);
        assert_eq!(plain, "『イベント』カテゴリに『COMIC1★15』を新しい要素として登録し、ファイルにタグ付けする");
        c.parent_mode = ParentMode::Both;
        assert_eq!(conclusion(&c, &m), format!("{plain}（上位名『COMIC1』も登録し、タグ付けする）"));
        assert_eq!(parent_hint(&c), None, "使っているときは知らせない");
        c.parent_mode = ParentMode::ParentOnly;
        assert_eq!(conclusion(&c, &m), "『イベント』カテゴリに上位名『COMIC1』を新しい要素として登録し、ファイルにタグ付けする（『COMIC1★15』は登録しない）");
        // 不採用なら、上位名の説明は添えない。まとめた候補には選択肢を出さない。
        c.unadopt();
        assert_eq!(conclusion(&c, &m), "ファイルにタグ付けせず、マスターにも登録しない");
        c.members = vec![base(Role::Event, "x", Choice::New)];
        assert!(parent_options(&c).is_none());
    }

    #[test]
    fn rule_texts() {
        assert_eq!(rule_line("イベント", "同人CG集", "ジャンル"), "【イベント】『同人CG集』→『ジャンル』");
        assert_eq!(
            missing_rules_notice(&["A".to_string(), "B".to_string()]),
            "カテゴリ振替ルール 2件は、移し先のカテゴリが見つからないため使いませんでした（A、B）"
        );
        let mut m = master();
        m.push(cat("ジャンル", false, vec![]));
        let mut c = base(Role::Event, "同人CG集", Choice::New);
        c.category = CatRef::Existing(2);
        c.move_to_category(3);
        assert!(!conclusion(&c, &m).contains("ルール"));
        c.category_rule = true;
        assert_eq!(conclusion(&c, &m), "『ジャンル』カテゴリに『同人CG集』を新しい要素として登録し、ファイルにタグ付けする（保存したルールにより）");
    }

    #[test]
    fn band_title_has_category_prefix() {
        assert_eq!(band_title(|_| "作者".to_string(), false), "カテゴリ：作者");
        assert_eq!(band_title(|_| "言語".to_string(), false), "カテゴリ：言語");
        assert_eq!(band_title(|_| "カテゴリ（メインタグ）".to_string(), true), "カテゴリ：メインタグ");
    }

    #[test]
    fn toggle_label_uses_the_two_terms() {
        assert_eq!(toggle_label(true), "✓ タグ採用予定");
        assert_eq!(toggle_label(false), "タグ不採用");
    }

    #[test]
    fn resolve_options_for_similar_candidate() {
        let m = master();
        let opts = resolve_options(&koji(), &m);
        let choices: Vec<&Choice> = opts.iter().map(|o| &o.choice).collect();
        assert_eq!(
            choices,
            [&Choice::Existing { cat_idx: 1, tier_id: 10 }, &Choice::Alias { cat_idx: 1, tier_id: 10 }, &Choice::New, &Choice::Skip]
        );
        assert_eq!(opts[0].title, "作者『Kouji』（既存の要素）をファイルにタグ付け");
        assert!(opts[0].desc.contains("5ファイルに、作者の『Kouji』が付きます"));
        assert_eq!(opts[1].title, "既存の作者要素『Kouji』に別名『Koji』を足して、ファイルにタグ付け");
        assert!(opts[1].desc.contains("『Kouji|Koji』に変わり"));
        assert_eq!(opts[2].title, "『Koji』を『作者』カテゴリの新しい要素として登録し、ファイルにタグ付け");
        assert!(opts[2].desc.contains("既存のタグとは別のものとして扱います"));
        assert_eq!(opts[3].title, "ファイルにタグ付けしない（タグ不採用）");
        assert!(opts[3].desc.contains("次回の取得で、また候補に出ます"));
    }

    #[test]
    fn resolve_options_includes_current_exact_match_without_duplicates() {
        let m = master();
        // 完全一致（類似提案に無い）の行でも、いまの選択が先頭の選択肢に入る。
        let c = base(Role::Author, "Kouji", Choice::Existing { cat_idx: 1, tier_id: 10 });
        let opts = resolve_options(&c, &m);
        assert_eq!(opts.first().map(|o| &o.choice), Some(&Choice::Existing { cat_idx: 1, tier_id: 10 }));
        let mut seen = Vec::new();
        for o in &opts {
            assert!(!seen.contains(&o.choice), "同じ選択肢は1つだけ");
            seen.push(o.choice.clone());
        }
        // 類似提案にも同じ要素があっても、重ならない。
        let mut c2 = koji();
        c2.choice = Choice::Existing { cat_idx: 1, tier_id: 10 };
        assert_eq!(resolve_options(&c2, &m).len(), 4);
    }

    #[test]
    fn resolve_options_for_new_category_and_plain_new() {
        let m = master();
        let mut p = base(Role::Performer, "Castorice", Choice::New);
        p.category = CatRef::New;
        let opts = resolve_options(&p, &m);
        assert_eq!(opts.len(), 2, "提案の無い新規カテゴリの候補は、新規と不採用だけ");
        assert_eq!(opts[0].title, "新しいカテゴリ『演者』を作成し、『Castorice』をその要素として登録して、ファイルにタグ付け");
        assert_eq!(opts[0].desc, "カテゴリ『演者』と要素『Castorice』が増え、5ファイルに付きます。");
        let r = base(Role::Event, "C105", Choice::New);
        let mut r = r;
        r.category = CatRef::Existing(2);
        let o = resolve_options(&r, &m);
        assert_eq!(o[0].desc, "『イベント』に『C105』が増え、5ファイルに付きます。");
    }

    #[test]
    fn resolve_heading_is_state_independent() {
        assert_eq!(resolve_heading(&koji()), "「Koji」（5ファイルにタグ付けする候補）をどうしますか");
        let mut adopted = koji();
        adopted.choice = Choice::New;
        assert_eq!(resolve_heading(&adopted), resolve_heading(&koji()));
    }

    #[test]
    fn similarity_note_lists_scores() {
        assert_eq!(similarity_note(&koji()).as_deref(), Some("似ている度合い：『Kouji』 92%"));
        assert_eq!(similarity_note(&base(Role::Author, "X", Choice::New)), None);
    }

    #[test]
    fn review_kind_classifies_each_reason() {
        let mut rule = base(Role::Author, "Mika", Choice::Skip);
        rule.review = true;
        rule.rule_reason = Some("AI generated".to_string());
        assert_eq!(review_kind(&rule), Some(ReviewKind::Rule("AI generated".to_string())));
        let mut learned = base(Role::Author, "Tiny Asa", Choice::Skip);
        learned.review = true;
        assert_eq!(review_kind(&learned), Some(ReviewKind::Learned));
        assert_eq!(review_kind(&koji()), Some(ReviewKind::Similar { name: "Kouji".to_string(), score: 92 }));
        let mut unc = base(Role::Author, "Hawk-Bit", Choice::New);
        unc.unconfirmed = true;
        assert_eq!(review_kind(&unc), Some(ReviewKind::Unconfirmed));
        assert_eq!(review_kind(&base(Role::Format, "zip", Choice::Existing { cat_idx: 0, tier_id: 1 })), None);
    }

    #[test]
    fn exact_match_with_other_suggestions_is_not_review() {
        // 完全一致（初期が既存）で、別の類似提案も並ぶ行は、判断を求めない。
        let mut c = koji();
        c.initial = Choice::Existing { cat_idx: 1, tier_id: 10 };
        c.choice = c.initial.clone();
        assert!(!needs_review(&c));
    }

    #[test]
    fn review_short_texts() {
        assert_eq!(review_short(&ReviewKind::Rule("AI generated".to_string())), "ルール「AI generated」で無効");
        assert_eq!(review_short(&ReviewKind::Learned), "学習により「タグではない」と判定");
        assert_eq!(review_short(&ReviewKind::Similar { name: "Kouji".to_string(), score: 92 }), "似た名前が既存にあります");
        assert_eq!(review_short(&ReviewKind::Unconfirmed), "作者かグループか未確定");
    }

    #[test]
    fn review_long_texts_use_full_words_and_terms() {
        let m = master();
        let mut rule = base(Role::Author, "Mika", Choice::Skip);
        rule.review = true;
        rule.rule_reason = Some("AI generated".to_string());
        let mut unc = base(Role::Author, "Hawk-Bit", Choice::New);
        unc.unconfirmed = true;
        let cases = [(review_kind(&rule).unwrap(), rule), (review_kind(&koji()).unwrap(), koji()), (review_kind(&unc).unwrap(), unc)];
        for (kind, c) in &cases {
            let sections = review_long(kind, c, &m);
            assert!(sections.len() >= 2);
            let all: String = sections.iter().map(|(h, b)| format!("{h}{b}")).collect();
            assert!(!all.contains("付ける予定"), "用語は「タグ採用予定」に揃える");
            assert!(all.contains("タグ"), "{all}");
            // 「付けない」「付ける」の単独表記を避け、「ファイルにタグ付け」とフルで書く。
            assert!(!all.contains("付けない") || all.contains("ファイルにタグ付けしない"), "{all}");
        }
        let rule_text = review_long(&cases[0].0, &cases[0].1, &m);
        assert!(rule_text[0].1.contains("ルール『AI generated』"));
        assert!(rule_text[2].1.contains("「タグ採用予定」にします"));
        let unc_text = review_long(&cases[2].0, &cases[2].1, &m);
        assert!(unc_text[0].1.contains("作者として、「タグ採用予定」にしてあります"), "{}", unc_text[0].1);
        let similar = review_long(&cases[1].0, &cases[1].1, &m);
        assert!(similar[0].1.contains("『Kouji』と名前がよく似ています（92%）"));
        assert!(similar[1].1.contains("「タグ不採用」（ファイルにタグ付けしない）"));
    }
}
