//! 一括タグ取得の確認ウィンドウに出す文面の組み立て（純関数）。画面（egui）には触れない。
//!
//! - 行の結論文、変更ダイアログの選択肢（見出しと説明）、「要確認」の種類と理由（短文・長文）を作る
//! - 用語は「タグ採用予定」「タグ不採用」に統一する。「付ける」「付けない」の単独表記は使わず、
//!   「ファイルにタグ付けする／しない」とフルで書く
//! - 日本語のみ。英・中への多言語化は、日本語UIの目視確認が終わってから、文言を `i18n.rs` へ移して行う
//!   （文面はこのモジュールに集約してあるので、移すときの対象はここだけ）
//! - 呼び出し側は、確認ウィンドウの行描画・変更ダイアログ・理由ポップアップ（フェーズB以降）

use crate::i18n::Lang;
use crate::tag_candidates::{Candidate, CatRef, Choice, ParentMode, Role};
use crate::tag_master::{display_name, primary_alias};
use crate::view_explorer::TagManagerCategoryUi;

/// 採用する（ファイルにタグ付けする）状態の名前。
pub(crate) const ADOPT_LABEL: &str = "タグ採用予定";
/// 採用しない（ファイルにタグ付けしない）状態の名前。
pub(crate) const REJECT_LABEL: &str = "タグ不採用";
/// 変更ダイアログの副題。
pub(crate) const RESOLVE_SUBTITLE: &str = "適用するまで、ファイルは変わりません。";

/// 行の採否トグルに出す文字。
pub(crate) fn toggle_label(adopted: bool) -> String {
    if adopted {
        format!("✓ {ADOPT_LABEL}")
    } else {
        REJECT_LABEL.to_string()
    }
}

// ── 「他の要素と同一としてまとめる」 ────────────────────────────────

/// 行の「⋯」メニュー・類似品の見出しのボタン名。
pub(crate) const MERGE_LABEL: &str = "他の要素と同一としてまとめる…";
/// まとめた行の「⋯」メニューの項目。
pub(crate) const SPLIT_LABEL: &str = "まとめを解除（適用前のみ）";
/// ダイアログのタイトル。
pub(crate) const MERGE_TITLE: &str = "他の要素と同一としてまとめる";
/// ダイアログの実行ボタン。
pub(crate) const MERGE_OK_LABEL: &str = "まとめる";
/// まとめた行に付ける、同じものの目印の見出し。
pub(crate) const MERGED_LABEL: &str = "同一";

/// ダイアログの冒頭の説明（用途）。
pub(crate) fn merge_intro(name: &str) -> String {
    format!(
        "『{name}』を、下記のタグと同じものとしてまとめます。「同人誌|doujinshi」のように、英語と日本語を同一視したいときや、カテゴリの数を増やさずに、検索結果を同じにしたいときに使ってください。"
    )
}

/// ダイアログの「まとめる前に知っておくこと」（デメリット）。
pub(crate) const MERGE_CONSEQUENCES: [&str; 4] = [
    "まとめたタグは1つのタグとして扱われます。片方の名前だけで、単独に検索することはできなくなります。",
    "まとめを、あとから分離する機能はありません。適用前なら「まとめを解除」で戻せますが、適用後は、タグ管理で名前の別名を消すことしかできず、すでに付いたタグが別々のタグに戻ることはありません。",
    "すでに別々に登録されているタグどうしは、まとめられません。",
    "まとめた結果は、必ず「タグ採用予定」になります。",
];

/// 選んだ候補の、まとめる前の確認文（件数つき）。2件未満のときは促す文。
pub(crate) fn merge_preview(names: &[&str], files: usize) -> String {
    if names.len() < 2 {
        return "2件以上を選んでください".to_string();
    }
    format!("まとめる候補：{}（合計{files}ファイル）", names.join("、"))
}

/// 類似品の束の見出し。
pub(crate) fn cluster_header(size: usize) -> String {
    format!("名前が似ている候補 {size}件")
}

/// 類似品の束の見出しのボタン。
pub(crate) const CLUSTER_ADOPT_LABEL: &str = "まとめて「タグ採用予定」へ";
pub(crate) const CLUSTER_REJECT_LABEL: &str = "まとめて「タグ不採用」へ";

/// 採否トグルのホバー文。
pub(crate) const TOGGLE_HINT: &str = "押すと「タグ採用予定」と「タグ不採用」が切り替わります";

/// 行に出す対象ファイル数。
pub(crate) fn file_count(n: usize) -> String {
    format!("{n}ファイル")
}

/// 行の1段目に出す、要確認の印（短い理由つき）。
pub(crate) fn review_badge(kind: &ReviewKind) -> String {
    format!("要確認：{}", review_short(kind))
}

// ── 一覧（表）の見出しと表示密度 ───────────────────────────────────

pub(crate) const COL_TOGGLE: &str = "採否";
pub(crate) const COL_NAME: &str = "名前";
pub(crate) const COL_COUNT: &str = "ファイル数";
/// 要確認の印と、何をどこにどうするかの結論文が入る列の見出し。
pub(crate) const COL_STATE: &str = "内容・要確認";

/// 種類の帯（行をまたいで中央に出す見出し）の前置き。
pub(crate) const BAND_PREFIX: &str = "カテゴリ：";

/// 種類の帯の文字。メインタグの種類は「カテゴリ（メインタグ）」という名前なので、「カテゴリ：メインタグ」にする。
pub(crate) fn band_title(role_label: &str, is_main: bool) -> String {
    if is_main {
        format!("{BAND_PREFIX}メインタグ")
    } else {
        format!("{BAND_PREFIX}{role_label}")
    }
}
pub(crate) const DENSITY_STANDARD: &str = "表示：標準（2段）";
pub(crate) const DENSITY_COMPACT: &str = "表示：コンパクト（1段）";
pub(crate) const DENSITY_HINT: &str = "一覧の行の密度を切り替えます。コンパクトは1行で、行数が多いときに便利です";

// ── 窓冒頭の説明・よくある質問・適用前の確認・上限ダイアログ ─────────────

pub(crate) const INTRO_TITLE: &str = "この窓で求めること";
pub(crate) const INTRO_FOLD: &str = "説明をたたむ";
pub(crate) const INTRO_UNFOLD: &str = "説明を開く";
/// 冒頭の説明（段落ごと）。
pub(crate) const INTRO_PARAS: [&str; 3] = [
    "各候補は、ファイルから自動抽出したタグです。検出元のファイルに付けるか（「タグ採用予定」）、付けないか（「タグ不採用」）のどちらかを選びます。「タグ採用予定」は、最終確認で適用した時点で、ファイルのタグDBに書き込まれます。適用するまで、ファイルは変わりません。",
    "「タグ不採用」の候補は、登録もしません。付けたくないものは、「タグ不採用」にしてください（不採用は保存されないので、次の自動抽出で、また候補に出ます）。",
    "迷ったら、「変更…」で、どう扱うかを選べます。「要確認」と出ている候補は、内容を見て、扱いを決めてください。",
];
pub(crate) const FAILED_TITLE: &str = "失敗したと思ったら";
/// 「失敗したと思ったら」の説明（段落ごと）。
pub(crate) const FAILED_PARAS: [&str; 3] = [
    "適用の前に、データベースのバックアップを自動で取ります。",
    "この窓の操作を最後まで終えると、「試用中」になります。このとき、タグパネルの「確定」か「破棄」を選べます。",
    "「破棄」を選ぶと、この自動抽出で足したタグだけをファイルから外し、操作する前に戻します（適用のあとに手で付けた・外したタグには触りません）。データベースのバックアップからの復元は、別に、設定のバックアップ画面でできます。",
];
pub(crate) const FAQ_BUTTON: &str = "よくある質問";
pub(crate) const FAQ_TITLE: &str = "よくある質問（見出しをクリックして展開）";
/// よくある質問（質問, 答え）。
pub(crate) const FAQ_ITEMS: [(&str, &str); 3] = [
    (
        "自動で付けたタグを、手動で消すと、どうなりますか？",
        "消したタグは、外れたままです。ただし、「消した」ことは記憶されません。次の自動抽出で同じ候補が出れば、また付きます。付けたくないときは、この窓で「タグ不採用」にしてください。",
    ),
    (
        "手動のタグと、自動のタグは、どう混ざりますか？",
        "手動で付けたタグは、自動抽出で消えません。自動は、足すだけです。形式・メインなど、1つしか選べないカテゴリは、手動のタグが優先されます（メインは、未設定のときだけ付きます）。同じタグがすでにあるときは、何も起きません。逆に、自動で付けたあとに手動で付け直したタグは、自動付与の記録から外れます。「付け直し」や「破棄」で、外れることはありません。",
    ),
    (
        "旧バージョンの手動タグも含めて、全部やり直したいときは？",
        "自動付与の分だけなら、試用中は「破棄」、確定後は、適用のときの「前回の自動付与分を外して付け直す」を使います（手動のタグは外れません）。手動のタグも含めて全部消すなら、タグ管理の「危険な設定」の「タグのクリア」です。全ファイルのタグが消え、タグマスターも初期状態に戻ります（評価・お気に入り・学習データは残ります）。直前に、データベースとマスターを自動でバックアップします。心配なときは、先にリカバリポイントを作ってください。",
    ),
];

pub(crate) const FOOTER_BACK: &str = "戻る";
pub(crate) const FOOTER_NEXT: &str = "次へ";
pub(crate) const FOOTER_APPLY: &str = "今までの選択を適用";

pub(crate) const PRECHECK_TITLE: &str = "要確認の候補が残っています";
pub(crate) const PRECHECK_BACK: &str = "戻って確認する";
pub(crate) const PRECHECK_GO: &str = "このまま適用する";

/// 適用前の確認の本文（未確認の要確認が残っているとき）。
pub(crate) fn precheck_body(count: usize) -> String {
    format!("まだ確認していない「要確認」の候補が{count}件あります。このまま適用すると、いまの「タグ採用予定」「タグ不採用」の選択どおりに、ファイルにタグが付きます。確認してから適用しますか？")
}

pub(crate) const LIMIT_TITLE: &str = "新規に登録する要素の数が、上限を超えています";

/// 上限ダイアログの本文。`rows` は種類ごとの超過の行、`db_size` はいまのDBのサイズ、`step` は拡張の幅。
pub(crate) fn limit_body(limit: usize, rows: &str, db_size: &str, step: usize) -> String {
    format!(
        "カテゴリごとの、新規に登録する要素の上限（{limit}個）を超えています。\n\n{rows}\n\n現在のデータベースのサイズ：{db_size}\n\n【{step}個拡張する（おすすめ）】上限を{step}個増やし、すべての要素を登録します。要素が増えたぶん、データベースは大きくなります。ストレージに余裕があるなら、こちらをおすすめします。\n【上限を維持する】データベースは、これ以上大きくなりません。ただし、ファイル数の多い要素から{limit}個までしか登録されません。残りの要素は新しく取り込まれず、その要素が付くはずだったファイルには、タグが付きません。"
    )
}

// ── 変更・理由ダイアログのボタン ────────────────────────────────────

/// 要確認のままの行のボタン。
pub(crate) const RESOLVE_BUTTON_LABEL: &str = "解決する…";
/// 確認済み（または要確認でない）行のボタン。
pub(crate) const CHANGE_BUTTON_LABEL: &str = "変更…";
/// 要確認の印のとなりのボタン。
pub(crate) const WHY_BUTTON_LABEL: &str = "理由を見る";
/// 解決した行の印。
pub(crate) const CONFIRMED_LABEL: &str = "✓ 確認済み";
/// 変更ダイアログの実行ボタン。
pub(crate) const RESOLVE_OK_LABEL: &str = "この内容にする";
/// 理由ダイアログの閉じるボタン。
pub(crate) const CLOSE_LABEL: &str = "閉じる";

/// 理由ダイアログのタイトル。
pub(crate) fn why_title(name: &str) -> String {
    format!("要確認の理由：{name}")
}

// ── 一覧の見出し帯・下端のサマリー・照合の設定 ───────────────────────

pub(crate) const VIEW_ALL: &str = "すべて";
pub(crate) const VIEW_NEEDS_REVIEW: &str = "要確認";
pub(crate) const DEFAULT_REVIEW_LABEL: &str = "デフォルト表示を要確認項目にする";
pub(crate) const DEFAULT_REVIEW_HINT: &str = "オンのとき、確認窓を開いたら要確認の候補だけを先に表示します（要確認が無いときは、すべてを表示します）";
pub(crate) const NEEDS_REVIEW_EMPTY: &str = "要確認の候補はありません。";

pub(crate) const BULK_LABEL: &str = "一括操作 ▾";
pub(crate) const BULK_ADOPT_ALL: &str = "すべて「タグ採用予定」にする";
pub(crate) const BULK_REJECT_ALL: &str = "すべて「タグ不採用」にする";
pub(crate) const BULK_REJECT_UNCONFIRMED: &str = "未確定の候補を「タグ不採用」にする";

pub(crate) const SETTINGS_BUTTON: &str = "⚙ 照合の設定";
pub(crate) const SETTINGS_HINT: &str = "普通は変更不要です";
pub(crate) const SETTINGS_TITLE: &str = "照合の設定";
pub(crate) const SETTINGS_NOTE: &str = "普通は変更不要です。表記のゆれを、どこまで同じものとみなすかの設定です。変えると、候補が作り直されます。";
pub(crate) const THRESHOLD_NOTE: &str = "低いほど、似ている候補が多く出ます。";

/// 一覧の下端のサマリー（この窓の分）。
pub(crate) fn summary(planned: usize, new_elements: usize, rejected: usize) -> String {
    format!("{ADOPT_LABEL} {planned}件 / マスターに新しく登録 {new_elements}個 / {REJECT_LABEL} {rejected}件")
}

/// ルールが働いたことを知らせるボタン（ルールの種類の数）。
pub(crate) fn rule_hits_button(kinds: usize) -> String {
    format!("ルールが働いた候補（{kinds}種）")
}

pub(crate) const RULE_HITS_TITLE: &str = "ルールが働いた候補";
pub(crate) const RULE_HITS_NOTE: &str = "ルールで「タグ不採用」になった候補には、行に「要確認」が付きます。ファイルにタグ付けしたい場合は、その行で「解決する…」を押してください。";

/// ルールが成立したファイル数の1行。
pub(crate) fn rule_hit_line(rule: &str, files: usize) -> String {
    format!("{rule}：{files}ファイル")
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
        (Some(p), ParentMode::None) if c.members.is_empty() => Some(format!("上位名の候補：『{p}』")),
        _ => None,
    }
}

/// 変更ダイアログの、上位名の見出し。
pub(crate) fn parent_heading(parent: &str) -> String {
    format!("上位名『{parent}』の扱い")
}

/// 上位名の扱いの選択肢（細かい名前だけ／両方／上位名だけ）。上位名の候補が無い・まとめた候補なら None。
pub(crate) fn parent_options(c: &Candidate) -> Option<Vec<ParentOption>> {
    let parent = c.parent_name.as_deref().filter(|_| c.members.is_empty())?;
    let name = &c.name;
    Some(vec![
        ParentOption {
            mode: ParentMode::None,
            title: format!("細かい名前『{name}』だけを登録する"),
            desc: format!("『{parent}』は登録しません。回ごとに、別のタグになります。"),
        },
        ParentOption {
            mode: ParentMode::Both,
            title: format!("上位名『{parent}』と、細かい名前『{name}』の両方を登録する"),
            desc: format!("同じファイルに、両方のタグが付きます。検索では、『{parent}』だけで全ての回、2つを選ぶと特定の回を探せます。"),
        },
        ParentOption {
            mode: ParentMode::ParentOnly,
            title: format!("上位名『{parent}』だけを登録する"),
            desc: format!("『{name}』のような回ごとの名前は登録せず、全ての回を『{parent}』として扱います。あとから、回ごとに分けることはできません。"),
        },
    ])
}

/// 照合の設定ポップアップ内の、区切り記号の管理。
pub(crate) const SEPARATORS_TITLE: &str = "上位名の区切り記号";
pub(crate) const SEPARATORS_NOTE: &str = "名前の最後が「区切り記号＋番号」のとき（例：COMIC1★15）、区切り記号の前を、上位名の候補にします。変えると、候補の上位名が探し直されます。";
pub(crate) const SEPARATORS_ADD_LABEL: &str = "追加";
pub(crate) const SEPARATORS_ADD_HINT: &str = "例：＠";
pub(crate) const SEPARATORS_RESET_LABEL: &str = "既定に戻す";
pub(crate) const SEPARATORS_EMPTY: &str = "区切り記号がありません。区切り記号が無い名前でも、同じ上位名の候補が2件以上あれば、上位名を提案します。";

// ── 登録先のカテゴリを選び直す ─────────────────────────────────────

/// カテゴリ振替ルールで決まった行の、結論文の添え書き。
pub(crate) const RULE_SUFFIX: &str = "（保存したルールにより）";
/// 変更ダイアログのチェック（次回以降も同じ扱いにする）。
pub(crate) const REMEMBER_LABEL: &str = "次回以降も、この名前はこのカテゴリとして扱う";
pub(crate) const REMEMBER_HINT: &str = "ルールとして保存します。管理と削除は、「⚙ 照合の設定」の中でできます。照合は名前の完全一致です（全角半角・大文字小文字・空白の違いは同じ名前として扱います）";
/// 変更ダイアログで、保存済みのルールがある候補に出す注意。
pub(crate) const RULE_EXISTS_NOTE: &str = "この名前には、保存したカテゴリ振替ルールがあります。";

/// 照合の設定ポップアップ内の、ルール管理の見出し・説明。
pub(crate) const RULES_SECTION_TITLE: &str = "カテゴリ振替ルール";
pub(crate) const RULES_SECTION_NOTE: &str = "「この名前は、このカテゴリとして扱う」と保存したルールです。次回の取得から有効で、いまの候補は変わりません。";
pub(crate) const RULES_EMPTY: &str = "保存したルールはありません。";
pub(crate) const RULES_DELETE_LABEL: &str = "削除";
pub(crate) const RULES_READ_ONLY: &str = "ルールのファイルを読めなかったため、ルールは保存できません（元のファイルは変更していません）。";

/// 一覧の1行。
pub(crate) fn rule_line(role: &str, name: &str, category: &str) -> String {
    format!("【{role}】『{name}』→『{category}』")
}

/// 使えなかったルールの知らせ（一覧の下端）。
pub(crate) fn missing_rules_notice(names: &[String]) -> String {
    format!("カテゴリ振替ルール {}件は、移し先のカテゴリが見つからないため使いませんでした（{}）", names.len(), names.join("、"))
}

pub(crate) const MOVE_TITLE: &str = "別のカテゴリの要素として登録…";
pub(crate) const MOVE_PICK_LABEL: &str = "登録先のカテゴリ";
pub(crate) const MOVE_PICK_PLACEHOLDER: &str = "（選んでください）";
pub(crate) const MOVE_NO_TARGET: &str = "移し先にできるカテゴリがありません。先にタグ管理でカテゴリを作ってから、選んでください。";

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
    let cat = match base_category(c) {
        CatRef::Existing(i) => category_name(master, i, c.role),
        CatRef::New => c.role.new_category_name(Lang::Japanese).to_string(),
    };
    format!(
        "『{}』を、選んだカテゴリの新しい要素として登録し、{}ファイルにタグ付けします。抽出された『{cat}』が、実際の種類と違うときに選びます。",
        c.registration_name(),
        c.files.len()
    )
}

pub(crate) const BULK_MOVE_LABEL: &str = "まとめて別のカテゴリへ移す…";
pub(crate) const BULK_MOVE_TITLE: &str = "まとめて別のカテゴリの要素として登録";
pub(crate) const BULK_MOVE_OK_LABEL: &str = "移す";

/// 一括移動ダイアログの案内。
pub(crate) fn bulk_move_intro(kind: &str) -> String {
    format!("「{kind}」として抽出された候補のうち、実際は別の種類のものを、まとめて、別のカテゴリの新しい要素として登録するか、すでにある要素に結びつけます。対象の候補にチェックを入れてください。")
}

/// 一括移動の確認文（選んだ内容を、ダイアログの中で確かめる）。
pub(crate) fn bulk_move_preview(count: usize, files: usize, category: Option<&str>) -> String {
    match (count, category) {
        (0, _) => "移す候補を選んでください".to_string(),
        (_, None) => format!("選んだ{count}件（合計{files}ファイル）の登録先のカテゴリを選んでください"),
        (_, Some(cat)) => format!("選んだ{count}件を、『{cat}』カテゴリの新しい要素として登録し、合計{files}ファイルにタグ付けする"),
    }
}

/// 移し先を選んだときの結論文（ダイアログの中で、選んだ内容を確かめる）。
pub(crate) fn move_preview(c: &Candidate, master: &[TagManagerCategoryUi], cat_idx: usize) -> String {
    format!(
        "『{}』カテゴリに『{}』を新しい要素として登録し、ファイルにタグ付けする",
        category_name(master, cat_idx, c.role),
        c.registration_name()
    )
}

// ── 既存の要素に結びつける（別名を足す） ──────────────────────────────

pub(crate) const LINK_TITLE: &str = "既存の要素に結びつける…";
pub(crate) const LINK_PICK_CATEGORY: &str = "カテゴリ";
pub(crate) const LINK_PICK_ELEMENT: &str = "結びつける要素";
pub(crate) const LINK_FILTER_HINT: &str = "要素名で絞り込み";
pub(crate) const LINK_NO_ELEMENT: &str = "このカテゴリには、結びつけられる要素がありません。";
/// 結びつけの注意（元に戻せる範囲と、メインの扱い）。
pub(crate) const LINK_NOTE: &str = "要素の名前が書き換わります。適用の試用中なら、「破棄」で元に戻せます（確定したあとは戻せません）。";
pub(crate) const LINK_MAIN_NOTE: &str = "メインカテゴリは、1つしか選べません。すでに別のメインタグが付いているファイルには、付きません。";
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

pub(crate) const BULK_MODE_MOVE: &str = "別のカテゴリの新しい要素として登録";
pub(crate) const BULK_MODE_LINK: &str = "既存の要素に結びつける（別名を足す）";
pub(crate) const BULK_LINK_DESC: &str = "選んだ候補の名前を、すでにある1つの要素の別名として足し、その要素をファイルにタグ付けします。以後の取得でも、同じ要素として一致します。";
pub(crate) const BULK_LINK_OK_LABEL: &str = "結びつける";

/// 一括の結びつけの確認文。`target` は（カテゴリ名, 要素名の全体）。
pub(crate) fn bulk_link_preview(count: usize, files: usize, target: Option<(&str, &str)>, names: &[String]) -> String {
    match (count, target) {
        (0, _) => "結びつける候補を選んでください".to_string(),
        (_, None) => format!("選んだ{count}件（合計{files}ファイル）の結びつけ先の要素を選んでください"),
        (_, Some((cat, full))) => {
            let mut all: Vec<String> = crate::tag_master::split_aliases(full).into_iter().map(str::to_string).collect();
            for n in names {
                if !all.iter().any(|a| crate::tag_master::normalize_key(a) == crate::tag_master::normalize_key(n)) {
                    all.push(n.clone());
                }
            }
            format!("選んだ{count}件を、{cat}の要素の別名として足し、合計{files}ファイルにタグ付けする（要素名は『{}』になる）", all.join("|"))
        }
    }
}

/// 要素の一覧が上限で切れたときの案内。
pub(crate) fn link_more(rest: usize) -> String {
    format!("ほか{rest}件あります。名前で絞り込んでください")
}

/// 結びつけの説明（カードの本文）。
pub(crate) fn link_desc(c: &Candidate) -> String {
    format!(
        "抽出された『{}』を、すでにある別の要素と同じものとして扱います。選んだ要素に別名『{}』を足して、{}ファイルにタグ付けします。以後の取得でも、『{}』は同じ要素として一致します。",
        c.name,
        c.alias_list().join("|"),
        c.files.len(),
        c.alias_list().join("』『")
    )
}

/// 結びつけを選んだときの結論文（ダイアログの中で、選んだ内容を確かめる）。
pub(crate) fn link_preview(c: &Candidate, master: &[TagManagerCategoryUi], cat_idx: usize, tier_id: u64) -> String {
    let aliases = c.alias_list().join("|");
    let el = element_label(master, tier_id);
    let cat = category_name(master, cat_idx, c.role);
    format!("{cat}の要素『{}』を『{}|{aliases}』にして、{}ファイルにタグ付けする", el, element_full(master, tier_id), c.files.len())
}

// ── 名前の取り出し ──────────────────────────────────────────────────

/// カテゴリの表示名。番号が範囲外のときは、種類の既定名。
fn category_name(master: &[TagManagerCategoryUi], cat_idx: usize, role: Role) -> String {
    master.get(cat_idx).map(|c| c.display_name()).unwrap_or_else(|| role.new_category_name(Lang::Japanese).to_string())
}

/// 候補が入るカテゴリの表示名（まだ無いカテゴリなら、作るときの名前）。
fn candidate_category_name(c: &Candidate, master: &[TagManagerCategoryUi]) -> String {
    match c.category {
        CatRef::Existing(i) => category_name(master, i, c.role),
        CatRef::New => c.role.new_category_name(Lang::Japanese).to_string(),
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
    match (c.parent_name.as_deref(), c.parent_mode) {
        (Some(parent), ParentMode::Both) if c.is_adopted() => format!("{}（上位名『{parent}』も登録し、タグ付けする）", conclusion_base(c, master)),
        (Some(parent), ParentMode::ParentOnly) if c.is_adopted() => match c.category {
            CatRef::New => format!(
                "新しいカテゴリ『{}』を作成し、上位名『{parent}』をその要素として登録して、ファイルにタグ付けする（『{}』は登録しない）",
                c.role.new_category_name(Lang::Japanese),
                c.name
            ),
            CatRef::Existing(_) => format!(
                "『{}』カテゴリに上位名『{parent}』を新しい要素として登録し、ファイルにタグ付けする（『{}』は登録しない）",
                candidate_category_name(c, master),
                c.name
            ),
        },
        _ => conclusion_base(c, master),
    }
}

/// 上位名を考えない、細かい名前の結論文。
fn conclusion_base(c: &Candidate, master: &[TagManagerCategoryUi]) -> String {
    match &c.choice {
        Choice::Existing { cat_idx, tier_id } => {
            format!("{}『{}』（既存の要素）を、ファイルにタグ付けする", category_name(master, *cat_idx, c.role), element_label(master, *tier_id))
        }
        Choice::Alias { cat_idx, tier_id } => format!(
            "既存の{}要素『{}』に、別名『{}』を足して、ファイルにタグ付けする",
            category_name(master, *cat_idx, c.role),
            element_label(master, *tier_id),
            c.alias_list().join("|")
        ),
        Choice::New => match c.category {
            CatRef::New => format!(
                "新しいカテゴリ『{}』を作成し、『{}』をその要素として登録して、ファイルにタグ付けする",
                c.role.new_category_name(Lang::Japanese),
                c.registration_name()
            ),
            CatRef::Existing(_) => format!(
                "『{}』カテゴリに『{}』を新しい要素として登録し、ファイルにタグ付けする{}",
                candidate_category_name(c, master),
                c.registration_name(),
                if c.category_rule { RULE_SUFFIX } else { "" }
            ),
        },
        Choice::Skip => "ファイルにタグ付けせず、マスターにも登録しない".to_string(),
    }
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
    format!("「{}」（{}ファイルにタグ付けする候補）をどうしますか", c.name, c.files.len())
}

/// 類似提案があるときの、似ている度合いの注記。
pub(crate) fn similarity_note(c: &Candidate) -> Option<String> {
    if c.suggestions.is_empty() {
        return None;
    }
    let parts: Vec<String> = c.suggestions.iter().map(|s| format!("『{}』 {}%", primary_alias(&s.name), s.score)).collect();
    Some(format!("似ている度合い：{}", parts.join(" / ")))
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
    let mut out: Vec<ResolveOption> = Vec::new();
    for (cat_idx, tier_id) in targets {
        let cat = category_name(master, cat_idx, c.role);
        let el = element_label(master, tier_id);
        out.push(ResolveOption {
            choice: Choice::Existing { cat_idx, tier_id },
            title: format!("{cat}『{el}』（既存の要素）をファイルにタグ付け"),
            desc: format!("「{}」の{n}ファイルに、{cat}の『{el}』が付きます。マスターは変わりません。同じ人（もの）と分かっているときに選びます。", c.name),
        });
        out.push(ResolveOption {
            choice: Choice::Alias { cat_idx, tier_id },
            title: format!("既存の{cat}要素『{el}』に別名『{aliases}』を足して、ファイルにタグ付け"),
            desc: format!(
                "要素名が『{}|{aliases}』に変わり、{n}ファイルに付きます。以後、『{aliases}』も同じタグとして扱います。",
                element_full(master, tier_id)
            ),
        });
    }
    out.push(match base_category(c) {
        CatRef::New => {
            let newcat = c.role.new_category_name(Lang::Japanese);
            ResolveOption {
                choice: Choice::New,
                title: format!("新しいカテゴリ『{newcat}』を作成し、『{reg}』をその要素として登録して、ファイルにタグ付け"),
                desc: format!("カテゴリ『{newcat}』と要素『{reg}』が増え、{n}ファイルに付きます。"),
            }
        }
        CatRef::Existing(base_idx) => {
            let cat = category_name(master, base_idx, c.role);
            let separate = if c.suggestions.is_empty() { "" } else { "既存のタグとは別のものとして扱います。" };
            ResolveOption {
                choice: Choice::New,
                title: format!("『{reg}』を『{cat}』カテゴリの新しい要素として登録し、ファイルにタグ付け"),
                desc: format!("『{cat}』に『{reg}』が増え、{n}ファイルに付きます。{separate}"),
            }
        }
    });
    out.push(ResolveOption {
        choice: Choice::Skip,
        title: format!("ファイルにタグ付けしない（{REJECT_LABEL}）"),
        desc: format!("{n}ファイルにはタグが付かず、マスターにも登録しません。次回の取得で、また候補に出ます。"),
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
pub(crate) fn review_short(kind: &ReviewKind) -> String {
    match kind {
        ReviewKind::Rule(rule) => format!("ルール「{rule}」で無効"),
        ReviewKind::Learned => "学習により「タグではない」と判定".to_string(),
        ReviewKind::Similar { .. } => "似た名前が既存にあります".to_string(),
        ReviewKind::Unconfirmed => "作者かグループか未確定".to_string(),
    }
}

/// 「理由を見る」ポップアップの長文。（見出し, 本文）の並び。
pub(crate) fn review_long(kind: &ReviewKind, c: &Candidate, master: &[TagManagerCategoryUi]) -> Vec<(String, String)> {
    let name = &c.name;
    let reject = format!("「{REJECT_LABEL}」（ファイルにタグ付けしない）");
    let adopt_hint = format!("ファイルにタグ付けしたいときは、「解決する…」から「{ADOPT_LABEL}」にします。");
    match kind {
        ReviewKind::Rule(rule) => vec![
            (
                "何が起きているか".to_string(),
                format!(
                    "ルール『{rule}』が、このファイルに当てはまりました。このルールは、この種類（{}）の名前をタグにしない設定です。『{name}』は、初期は{reject}にしてあります。",
                    c.role.new_category_name(Lang::Japanese)
                ),
            ),
            (
                "なぜそうしているか".to_string(),
                "ルールで無効にされた種類は、ファイル名の中の名前が実際の内容を表さないことがあるため、自動ではタグ付けしません。".to_string(),
            ),
            ("どうすればよいか".to_string(), format!("{adopt_hint}ルールの設定そのものは変わりません。")),
        ],
        ReviewKind::Learned => vec![
            (
                "何が起きているか".to_string(),
                format!("『{name}』は、あなたが過去に振り分けたファイル名のパターンと似ています。そのパターンでは、この位置の括弧は「タグにしない」ものとして振り分けられていました。初期は{reject}にしてあります。"),
            ),
            ("どうすればよいか".to_string(), adopt_hint),
        ],
        ReviewKind::Similar { name: best, score } => vec![
            ("何が起きているか".to_string(), format!("『{name}』は、すでにマスターにある『{best}』と名前がよく似ています（{score}%）。")),
            (
                "なぜ確認が必要か".to_string(),
                format!("同じ人（もの）の表記ゆれかもしれませんし、別のものかもしれません。アプリには判断できないため、初期は{reject}にしてあります。"),
            ),
            (
                "どうすればよいか".to_string(),
                format!(
                    "「解決する…」を押して、同じものなら「既存の要素をタグ付け」か「別名として足してタグ付け」、別のものなら「新しい要素として登録してタグ付け」を選びます。分からなければ{reject}のままで構いません。そのファイルにはタグが付きません。"
                ),
            ),
        ],
        ReviewKind::Unconfirmed => {
            let state = if c.initial == Choice::Skip { format!("{reject}") } else { format!("「{ADOPT_LABEL}」") };
            vec![
                (
                    "何が起きているか".to_string(),
                    format!(
                        "『{name}』は、ファイル名の先頭にある［ ］が1つだけで、作者名なのかグループ（サークル）名なのかを判断する手がかりがありませんでした。いまは{}として、{state}にしてあります。",
                        candidate_category_name(c, master)
                    ),
                ),
                (
                    "どうすればよいか".to_string(),
                    format!("そのままでよければ、何もしなくて構いません。違うと分かっているときは「{REJECT_LABEL}」にして、あとでタグパネルから手動でタグ付けしてください。"),
                ),
            ]
        }
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
        let all = MERGE_CONSEQUENCES.join("");
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
        assert!(INTRO_PARAS[0].contains("「タグ採用予定」") && INTRO_PARAS[0].contains("適用するまで、ファイルは変わりません"));
        assert!(INTRO_PARAS[1].contains("登録もしません"));
        assert!(FAILED_PARAS[2].contains("この自動抽出で足したタグだけ"));
        assert_eq!(FAQ_ITEMS.len(), 3);
        assert!(FAQ_ITEMS.iter().all(|(q, a)| q.ends_with('？') && !a.is_empty()));
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
        assert!(LINK_NOTE.contains("試用中") && LINK_MAIN_NOTE.contains("1つしか選べません"));
    }

    #[test]
    fn bulk_move_texts() {
        assert_eq!(bulk_move_preview(0, 0, None), "移す候補を選んでください");
        assert_eq!(bulk_move_preview(3, 7, None), "選んだ3件（合計7ファイル）の登録先のカテゴリを選んでください");
        assert_eq!(bulk_move_preview(3, 7, Some("ジャンル")), "選んだ3件を、『ジャンル』カテゴリの新しい要素として登録し、合計7ファイルにタグ付けする");
        assert!(bulk_move_intro("イベント").contains("「イベント」として抽出された候補"));
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
        assert_eq!(band_title("作者", false), "カテゴリ：作者");
        assert_eq!(band_title("言語", false), "カテゴリ：言語");
        assert_eq!(band_title("カテゴリ（メインタグ）", true), "カテゴリ：メインタグ");
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
