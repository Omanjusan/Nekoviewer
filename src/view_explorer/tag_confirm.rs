//! タグ候補の確認ウィンドウ（2枚）。走査・候補生成が終わったあとに開く。
//!
//! - 1枚目: 作者・グループ名を除く項目（形式・メイン・言語・イベント）。「次へ」で2枚目へ
//! - 2枚目: 作者・グループ名。「戻る」「適用」「キャンセル」。キャンセルは全部を捨てる（ディスクには何も書いていない）
//! - 上部で、照合の切替項目（正規化5項目と類似のしきい値）を切り替えられる。切り替えると別スレッドで候補を作り直し、
//!   ユーザーが変えた採否は、同じ候補に引き継ぐ
//! - 行は2段。1段目は、採否のトグル（タグ採用予定／タグ不採用）・名前・対象のファイル数・要確認の印・（類似提案があれば）選択欄・「⋯」。
//!   2段目は、何をどこにどうするかの1文（`tag_confirm_text::conclusion`）
//! - 他の要素と同一としてまとめるのは、行の「⋯」から開くダイアログで行う（用途とデメリットを案内する）。結果は必ず「タグ採用予定」
//! - 類似品の束は見出しの行を出し、まとめて「タグ採用予定」「タグ不採用」にできる
//!
//! 「適用」は、DBのバックアップ → マスターへの新規カテゴリ・要素の追加 → 各ファイルへのタグ追加、を別スレッドで行う
//! （`tag_apply`）。既存のタグは消さない。バックアップに失敗したら、何も変更せずに中止する。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crate::i18n::{self, Lang};
use crate::tag_candidates::{
    build_candidates_full, over_limit, Bundle, Candidate, CandidateInput, CandidateSet, CatRef, Choice, MergeError, ParentMode, Role, Window,
    LIMIT_EXPAND_STEP,
};
use crate::candidate_rules::{rule_role_name, MoveRule};
use crate::tag_confirm_text as ctext;
use crate::tag_master::split_aliases;
use crate::tag_model::Model;
use crate::tag_rules::TagRule;
use crate::tag_apply::{plan_application, run_apply, ApplyError, ApplyReport};
use crate::tag_similarity::{keys, MatchOptions};

use super::{NekoviewApp, TagManagerCategoryUi};

/// 一覧の1行（種類の見出し・類似品の見出し・候補）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Section(Role),
    Cluster { id: usize, size: usize },
    Cand(usize),
}

/// 一覧の表示の絞り込み（見出し帯のチップ）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListView {
    /// すべて。
    All,
    /// 要確認のうち、まだ確認していないもの。
    NeedsReview,
    /// タグ採用予定のもの。
    Planned,
    /// タグ不採用のもの。
    Rejected,
}

/// 窓の候補の件数（チップに出す）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ViewCounts {
    pub(super) all: usize,
    pub(super) needs_review: usize,
    pub(super) planned: usize,
    pub(super) rejected: usize,
}

/// 要確認で、まだ確認していない候補か。
fn pending_review(c: &Candidate, confirmed: &HashSet<(Role, String)>) -> bool {
    ctext::needs_review(c) && !confirmed.contains(&(c.role, c.key.clone()))
}

/// 未確認の要確認が残っている候補の数（窓をまたいで数える）。
fn pending_review_total(set: &CandidateSet, confirmed: &HashSet<(Role, String)>) -> usize {
    set.candidates.iter().filter(|c| pending_review(c, confirmed)).count()
}

/// 「既存の要素に結びつける」の選択部品（カテゴリ → 絞り込み → 要素の一覧）。選び直したら true。
/// 変更ダイアログと一括移動ダイアログで共用する。プレビューや注意書きは、呼び出し側が足す。
fn draw_link_picker(ui: &mut egui::Ui, id: &str, master: &[TagManagerCategoryUi], cat: &mut Option<usize>, tier: &mut Option<u64>, filter: &mut String) -> bool {
    let mut touched = false;
    let link_cats = ctext::link_categories(master);
    ui.horizontal(|ui| {
        ui.label(ctext::link_pick_category());
        let current = cat.and_then(|i| link_cats.iter().find(|c| c.0 == i)).map_or_else(|| ctext::move_pick_placeholder().to_string(), |c| c.1.clone());
        egui::ComboBox::from_id_salt((id, "cat")).selected_text(current).show_ui(ui, |ui| {
            for (i, name) in &link_cats {
                if ui.selectable_label(*cat == Some(*i), name).clicked() {
                    if *cat != Some(*i) {
                        *cat = Some(*i);
                        *tier = None;
                        filter.clear();
                    }
                    touched = true;
                }
            }
        });
    });
    let Some(cat_idx) = *cat else { return touched };
    ui.horizontal(|ui| {
        ui.label(ctext::link_pick_element());
        ui.add(egui::TextEdit::singleline(filter).desired_width(200.0).hint_text(ctext::link_filter_hint()));
    });
    let (elements, total) = ctext::link_elements(master, cat_idx, filter, ctext::LINK_LIST_CAP);
    if elements.is_empty() {
        ui.weak(ctext::link_no_element());
    } else {
        egui::ScrollArea::vertical().id_salt((id, "elems")).max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
            for e in &elements {
                if ui.selectable_label(*tier == Some(e.tier_id), &e.label).on_hover_text(&e.full).clicked() {
                    *tier = Some(e.tier_id);
                    touched = true;
                }
            }
            if total > elements.len() {
                ui.weak(ctext::link_more(total - elements.len()));
            }
        });
    }
    if master.get(cat_idx).is_some_and(|c| c.is_main) {
        ui.colored_label(egui::Color32::from_rgb(230, 160, 40), ctext::link_main_note());
    }
    touched
}

/// 窓冒頭の説明。たたむ・開くボタンと、よくある質問のボタンを持つ。
fn draw_intro(ui: &mut egui::Ui, st: &mut TagConfirm) {
    // お知らせ帯: 操作の帯・表と区別するため、背景を薄い青にし、左端に太い線を引く。
    let accent = ui.visuals().hyperlink_color;
    let fill = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 30);
    let out = egui::Frame::new().fill(fill).corner_radius(4.0).inner_margin(egui::Margin { left: 14, right: 8, top: 6, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("ℹ").color(accent).strong());
            ui.strong(ctext::intro_title());
            if ui.small_button(if st.intro_open { ctext::intro_fold() } else { ctext::intro_unfold() }).clicked() {
                st.intro_open = !st.intro_open;
            }
            if ui.small_button(ctext::faq_button()).clicked() {
                st.faq_open = true;
            }
        });
        if st.intro_open {
            egui::ScrollArea::vertical().id_salt("tag_confirm_intro").max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                for p in ctext::intro_paras() {
                    ui.label(p);
                }
                ui.add_space(4.0);
                ui.strong(ctext::failed_title());
                for p in ctext::failed_paras() {
                    ui.label(format!("・{p}"));
                }
            });
        }
    });
    let r = out.response.rect;
    ui.painter().rect_filled(egui::Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 4.0, r.bottom())), egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 }, accent);
    ui.add_space(8.0);
}

/// 窓ごとのチップの件数を数える。
pub(super) fn view_counts(set: &CandidateSet, window: Window, confirmed: &HashSet<(Role, String)>) -> ViewCounts {
    let mut n = ViewCounts::default();
    for c in set.candidates.iter().filter(|c| c.role.window() == window) {
        n.all += 1;
        if pending_review(c, confirmed) {
            n.needs_review += 1;
        }
        if c.is_adopted() {
            n.planned += 1;
        } else {
            n.rejected += 1;
        }
    }
    n
}

/// 絞り込み欄の文字列に合う候補か（名前・別名・見つかった表記のどれかに、正規化した部分一致。空なら全部）。
fn matches_query(c: &Candidate, query: &str, opts: &MatchOptions) -> bool {
    let q = keys(query, opts).exact;
    if q.is_empty() {
        return true;
    }
    let hit = |s: &str| keys(s, opts).exact.contains(&q);
    split_aliases(&c.name).into_iter().any(hit) || c.forms.iter().any(|f| hit(f))
}

/// 窓に出す行を作る。`view` は表示の絞り込み、`query` は名前の絞り込み（別名を含む部分一致）。
pub(super) fn rows_for(
    set: &CandidateSet,
    window: Window,
    view: ListView,
    confirmed: &HashSet<(Role, String)>,
    query: &str,
    opts: &MatchOptions,
) -> Vec<Row> {
    let idx: Vec<usize> = set
        .indices_in_window(window)
        .into_iter()
        .filter(|&i| {
            let c = &set.candidates[i];
            match view {
                ListView::All => true,
                ListView::NeedsReview => pending_review(c, confirmed),
                ListView::Planned => c.is_adopted(),
                ListView::Rejected => !c.is_adopted(),
            }
        })
        .filter(|&i| matches_query(&set.candidates[i], query, opts))
        .collect();
    let mut cluster_sizes: HashMap<usize, usize> = HashMap::new();
    for &i in &idx {
        *cluster_sizes.entry(set.candidates[i].cluster).or_default() += 1;
    }
    let mut rows = Vec::new();
    let (mut last_role, mut last_cluster) = (None, None);
    for i in idx {
        let c = &set.candidates[i];
        if last_role != Some(c.role) {
            rows.push(Row::Section(c.role));
            last_role = Some(c.role);
            last_cluster = None;
        }
        let size = cluster_sizes[&c.cluster];
        if size > 1 && last_cluster != Some(c.cluster) {
            rows.push(Row::Cluster { id: c.cluster, size });
        }
        last_cluster = Some(c.cluster);
        rows.push(Row::Cand(i));
    }
    rows
}

/// 候補を「付ける」件数（採用済み）。テストで使う。
#[cfg(test)]
pub(super) fn adopted_count(set: &CandidateSet, window: Window) -> usize {
    set.candidates.iter().filter(|c| c.role.window() == window && c.is_adopted()).count()
}

pub(super) struct TagConfirm {
    set: CandidateSet,
    inputs: Arc<Vec<CandidateInput>>,
    /// 入力と同じ順のファイルのパス（適用でタグを書く先）。
    paths: Vec<PathBuf>,
    master: Arc<Vec<TagManagerCategoryUi>>,
    /// 候補生成に使った学習モデル（切替項目を変えた作り直しでも同じモデルを使う）。
    model: Option<Arc<Model>>,
    /// 候補生成に使ったタグ抽出ルール（作り直しでも同じルールを使う）。
    rules: Arc<Vec<TagRule>>,
    stage: Window,
    /// 保存してあるカテゴリ振替ルール（作り直しでも、同じルールを当てる）。
    move_rules: Arc<Vec<MoveRule>>,
    /// 上位名を切り出す区切り記号（作り直しでも、同じ一覧を使う）。
    separators: Arc<Vec<String>>,
    /// 区切り記号の入力欄。
    sep_input: String,
    /// 区切り記号を足せなかった理由。
    sep_error: Option<String>,
    /// 表示の絞り込み（見出し帯のチップ）。
    view: ListView,
    /// 照合の設定ポップアップを開いている。
    settings_open: bool,
    /// 「ルールが働いた候補」ポップアップを開いている。
    rules_dialog_open: bool,
    rebuild: Option<mpsc::Receiver<CandidateSet>>,
    /// しきい値スライダーをドラッグしている間の仮の値（離したときに確定して作り直す）。
    threshold_draft: Option<u8>,
    /// 「他の要素と同一としてまとめる」ダイアログ（開いている間だけ Some）。
    merge_dialog: Option<MergeDialog>,
    bulk_move_dialog: Option<BulkMoveDialog>,
    /// 窓冒頭の説明を開いているか（毎回、開いた状態から始める）。
    intro_open: bool,
    /// よくある質問のポップアップを開いているか。
    faq_open: bool,
    /// 「キャンセル」を押したあとの、確認のポップアップを開いているか。
    cancel_confirm: bool,
    /// 適用前の確認（要確認が残っているとき）を出しているか。
    precheck: bool,
    /// 名前の絞り込み欄。
    filter: String,
    next_bundle_id: u64,
    /// 適用前の確認（既存要素の名前が書き換わるとき）。Some の間は確認の窓を出す。
    rename_review: Option<RenameReview>,
    /// 1カテゴリあたりの新規要素数の上限（設定の既定値から始まり、「拡張する」で増える）。
    limit: usize,
    /// 「上限を維持する」を選んだ。作り直したあとも、上限を超えた分を切り捨てる。
    limit_keep: bool,
    /// 切り捨てて候補から除いた数（種類ごと）。窓に注意書きを出す。
    truncated: Vec<(Role, usize)>,
    /// 「前回の自動付与分を外して付け直す」（既定はOFF）。
    reattach: bool,
    /// 一覧の表示密度。true ならコンパクト（1段）、false なら標準（2段）。この確認窓の間だけ覚える。
    compact: bool,
    /// ユーザーが確認した（採否を切り替えた・変更ダイアログで決めた）要確認の候補（種類, 候補のキー）。
    confirmed: HashSet<(Role, String)>,
    /// 変更ダイアログ（開いている間だけ Some）。
    resolve_dialog: Option<ResolveDialog>,
    /// 理由ダイアログ（開いている間だけ Some）。（種類, 候補のキー）
    why_dialog: Option<(Role, String)>,
}

/// 適用前に見せる、書き換わる既存要素の一覧。
struct RenameReview {
    /// （書き換え前, 書き換え後）。
    renames: Vec<(String, String)>,
    /// 別名が重なって書き換えない候補の数。
    conflicts: usize,
}

/// 「他の要素と同一としてまとめる」ダイアログの状態。
struct MergeDialog {
    /// まとめる候補の種類（同じ種類の中からだけ選べる）。
    role: Role,
    /// ダイアログを開いた候補の名前（説明文に出す）。
    name: String,
    /// 選んだ候補のキー。
    selected: HashSet<String>,
    /// 相手を探す絞り込み欄。
    filter: String,
    /// まとめを拒否したときの理由。
    msg: Option<String>,
}

/// 「まとめて別のカテゴリへ移す」ダイアログの状態。
struct BulkMoveDialog {
    /// 移す候補の種類（同じ種類の中からだけ選べる）。
    role: Role,
    /// 選んだ候補のキー。
    selected: HashSet<String>,
    /// 候補を探す絞り込み欄。
    filter: String,
    /// 登録先のカテゴリ（番号）。
    target: Option<usize>,
    /// 次回以降も、これらの名前はこのカテゴリとして扱う（ルールとして保存する）。
    remember: bool,
    /// 「既存の要素に結びつける」を選んでいるか（false なら「別のカテゴリの新しい要素として登録」）。
    link_on: bool,
    link_cat: Option<usize>,
    link_tier: Option<u64>,
    link_filter: String,
}

/// 変更ダイアログの状態。
struct ResolveDialog {
    role: Role,
    key: String,
    /// いま選んでいる選択肢（移し先を選んでいないとき）。
    sel: Choice,
    /// 「別のカテゴリの要素として登録」を選んでいるときの、移し先のカテゴリ（番号）。
    move_cat: Option<usize>,
    /// 「次回以降も、この名前はこのカテゴリとして扱う」（ルールとして保存する）。
    remember: bool,
    /// 上位名の扱い。
    parent_mode: ParentMode,
    /// 「既存の要素に結びつける」を選んでいるか（選んでいる間は、他の選択肢は選ばれていない扱い）。
    link_on: bool,
    /// 結びつけ先のカテゴリ（番号）。
    link_cat: Option<usize>,
    /// 結びつけ先の要素（tier_id）。
    link_tier: Option<u64>,
    /// 要素を探す絞り込み欄。
    link_filter: String,
}

/// 照合の設定ポップアップでの、区切り記号の操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SepAction {
    Remove(usize),
    Add,
    Reset,
}

/// 一覧の行から出す操作（描画中は候補の並びを変えられないので、描画後にまとめて行う）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    /// 束（類似品の同じ番号）をまとめるダイアログを開く。
    MergeCluster(usize),
    /// 候補（番号）をまとめるダイアログを開く。
    OpenMerge(usize),
    /// 候補（番号）の変更ダイアログを開く。
    OpenResolve(usize),
    /// 候補（番号）を最初に選んだ、一括移動ダイアログを開く。
    OpenBulkMove(usize),
    /// 候補（番号）の理由ダイアログを開く。
    OpenWhy(usize),
    /// まとめた候補を分解する。
    Split(usize),
    /// まとめた候補の、先頭見出しにする別名（候補の番号, 別名の位置）。
    SetPrimary(usize, usize),
}

impl TagConfirm {
    pub(super) fn new(
        set: CandidateSet,
        inputs: Vec<CandidateInput>,
        paths: Vec<PathBuf>,
        master: Vec<TagManagerCategoryUi>,
        model: Option<Arc<Model>>,
        rules: Arc<Vec<TagRule>>,
        limit: usize,
        move_rules: Arc<Vec<MoveRule>>,
        separators: Arc<Vec<String>>,
    ) -> Option<Self> {
        let has = |w: Window| set.candidates.iter().any(|c| c.role.window() == w);
        let stage = if has(Window::One) {
            Window::One
        } else if has(Window::Two) {
            Window::Two
        } else {
            return None;
        };
        Some(Self { set, inputs: Arc::new(inputs), paths, master: Arc::new(master), model, rules, stage, move_rules, separators, sep_input: String::new(), sep_error: None, view: ListView::All, settings_open: false, rules_dialog_open: false, rebuild: None, threshold_draft: None, merge_dialog: None, bulk_move_dialog: None, intro_open: true, faq_open: false, cancel_confirm: false, precheck: false, filter: String::new(), next_bundle_id: 1, rename_review: None, limit, limit_keep: false, truncated: Vec::new(), reattach: false, compact: false, confirmed: HashSet::new(), resolve_dialog: None, why_dialog: None })
    }

    /// 切替項目が変わったので、別スレッドで候補を作り直す。進行中の作り直しは捨てる。
    fn start_rebuild(&mut self, opts: MatchOptions, lang: Lang, name_lang: Lang) {
        let (tx, rx) = mpsc::channel();
        let (inputs, master, model, rules) = (Arc::clone(&self.inputs), Arc::clone(&self.master), self.model.clone(), Arc::clone(&self.rules));
        // 手動でまとめた候補は、作り直したあとも同じまとめを復元する。
        let bundles: Vec<Bundle> = self.set.snapshot_bundles();
        let move_rules = Arc::clone(&self.move_rules);
        let separators = Arc::clone(&self.separators);
        std::thread::spawn(move || {
            let mut fresh = build_candidates_full(&inputs, &master, &opts, lang, name_lang, model.as_deref(), &rules);
            fresh.apply_bundles(&bundles, &master);
            fresh.apply_category_rules(&move_rules, &master);
            fresh.detect_parents(&separators);
            let _ = tx.send(fresh);
        });
        self.rebuild = Some(rx);
        self.merge_dialog = None;
        self.bulk_move_dialog = None;
    }

    /// 最初の表示を決める（窓を開いたとき・窓を切り替えたとき）。要確認を既定にする設定がオンで、要確認が残っていれば「要確認」。
    fn pick_initial_view(&mut self, default_review: bool) {
        let pending = view_counts(&self.set, self.stage, &self.confirmed).needs_review;
        self.view = if ctext::initial_view_is_review(default_review, pending) { ListView::NeedsReview } else { ListView::All };
    }

    /// 変更ダイアログを開く。いまの選択から始める。
    fn open_resolve_dialog(&mut self, idx: usize) {
        let Some(c) = self.set.candidates.get(idx) else { return };
        // 移し先を選んである候補は、その移し先から始める。
        let move_cat = match (c.category_origin, c.category) {
            (Some(_), CatRef::Existing(i)) => Some(i),
            _ => None,
        };
        self.resolve_dialog = Some(ResolveDialog { role: c.role, key: c.key.clone(), sel: c.choice.clone(), move_cat, remember: c.category_rule, parent_mode: c.parent_mode, link_on: false, link_cat: None, link_tier: None, link_filter: String::new() });
    }

    /// 理由ダイアログを開く。
    fn open_why_dialog(&mut self, idx: usize) {
        let Some(c) = self.set.candidates.get(idx) else { return };
        self.why_dialog = Some((c.role, c.key.clone()));
    }

    /// まとめるダイアログを開く。`idxs` は最初から選んでおく候補（同じ種類のものだけ）。
    fn open_merge_dialog(&mut self, idxs: &[usize]) {
        let Some(first) = idxs.first().and_then(|&i| self.set.candidates.get(i)) else { return };
        let role = first.role;
        let name = first.name.clone();
        let selected: HashSet<String> = idxs
            .iter()
            .filter_map(|&i| self.set.candidates.get(i))
            .filter(|c| c.role == role)
            .map(|c| c.key.clone())
            .collect();
        self.merge_dialog = Some(MergeDialog { role, name, selected, filter: String::new(), msg: None });
    }

    /// 一括移動ダイアログを開く。`idx` の候補を最初から選んでおく。
    fn open_bulk_move_dialog(&mut self, idx: usize) {
        let Some(c) = self.set.candidates.get(idx) else { return };
        if !ctext::can_move_category(c) {
            return;
        }
        let selected = std::iter::once(c.key.clone()).collect();
        self.bulk_move_dialog = Some(BulkMoveDialog { role: c.role, selected, filter: String::new(), target: None, remember: false, link_on: false, link_cat: None, link_tier: None, link_filter: String::new() });
    }

    /// 作り直しの完了を取り込む。ユーザーが変えた採否は引き継ぐ。
    fn poll_rebuild(&mut self) -> bool {
        let Some(rx) = &self.rebuild else { return false };
        match rx.try_recv() {
            Ok(mut fresh) => {
                self.set.carry_over_choices(&mut fresh);
                self.set = fresh;
                // 「上限を維持する」を選んだあとは、作り直した候補にも同じ切り捨てをかける。
                if self.limit_keep {
                    self.truncated = self.set.truncate_to_limit(self.limit);
                }
                self.rebuild = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rebuild = None;
                false
            }
        }
    }
}

/// 種類の帯の文字（「カテゴリ：作者」の形）。
fn band_title(role: Role) -> String {
    ctext::band_title(|l| role_label_for(l, role), role == Role::Main)
}

/// 種類の名前（いまの表示言語。日中（テスト）では併記）。
fn role_label(role: Role) -> String {
    i18n::render(|l| role_label_for(l, role))
}

/// 種類の名前（言語を指定）。
fn role_label_for(t: Lang, role: Role) -> String {
    match role {
        Role::Format => t.tag_confirm_role_format(),
        Role::Main => t.tag_confirm_role_main(),
        Role::Language => t.tag_confirm_role_language(),
        Role::Event => t.tag_confirm_role_event(),
        Role::Author => t.tag_confirm_role_author(),
        Role::Group => t.tag_confirm_role_group(),
        Role::Performer => t.tag_confirm_role_performer(),
        Role::Character => t.tag_confirm_role_character(),
        Role::Parody => t.tag_confirm_role_parody(),
        Role::Attribute => t.tag_confirm_role_attribute(),
        Role::YearMonth => t.tag_confirm_role_year_month(),
    }
    .to_string()
}

impl NekoviewApp {
    /// 走査と候補生成の完了後に、確認ウィンドウを開く（候補が1つも無ければ開かない）。
    pub(super) fn open_tag_confirm(
        &mut self,
        set: CandidateSet,
        inputs: Vec<CandidateInput>,
        paths: Vec<PathBuf>,
        master: Vec<TagManagerCategoryUi>,
    ) {
        // 保存してあるカテゴリ振替ルールを、最初の候補に当てる。
        let mut set = set;
        set.apply_category_rules(&self.candidate_rules.moves, &master);
        let separators = Arc::new(self.candidate_rules.effective_separators());
        set.detect_parents(&separators);
        let move_rules = Arc::new(self.candidate_rules.moves.clone());
        match TagConfirm::new(
            set,
            inputs,
            paths,
            master,
            self.active_tag_model(),
            Arc::new(self.tag_rules.rules.clone()),
            self.config.tag_new_element_limit as usize,
            move_rules,
            separators,
        ) {
            Some(mut c) => {
                c.pick_initial_view(self.config.tag_confirm_default_review);
                c.intro_open = self.config.tag_confirm_intro_open;
                self.tag_confirm = Some(c);
            }
            None => self.set_toast(i18n::t().tag_confirm_no_candidates()),
        }
    }

    pub(super) fn draw_tag_confirm(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.tag_confirm.take() else { return };
        if st.poll_rebuild() || st.rebuild.is_some() {
            ctx.request_repaint();
        }
        let t = i18n::t();
        let stage = st.stage;
        let mut open = true;
        let mut next_stage: Option<Window> = None;
        let mut apply_clicked = false;
        let mut row_action: Option<RowAction> = None;
        let mut opts = self.config.tag_match;
        let before = opts;
        let mut default_review = self.config.tag_confirm_default_review;

        let center = self.explorer_center(ctx);
        egui::Window::new(match stage {
            Window::One => i18n::render(|t| t.tag_confirm_title_one().to_string()),
            Window::Two => i18n::render(|t| t.tag_confirm_title_two().to_string()),
        })
        .id(egui::Id::new("tag_confirm_window"))
        // 右タグパネル（Foreground の Area）より前面に出す。窓の重なり順は Order が先で、同じ Order の中でだけ、触った順になる。
        .order(egui::Order::Foreground)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([1000.0, 600.0])
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(center)
        .show(ctx, |ui| {
            // 冒頭の説明。たたむ・開くの状態は、設定に記憶する。
            draw_intro(ui, &mut st);
            // 見出し帯: 表示の絞り込み（チップ）・要確認を既定にするチェック・名前の絞り込み・表示密度・照合の設定・一括操作。
            let counts = view_counts(&st.set, stage, &st.confirmed);
            let window_idx: Vec<usize> = st.set.indices_in_window(stage);
            // 1行目: 絞り込み（表示の切り替えのボタン・名前の絞り込み）と、右端寄せの「オプション」。
            ui.horizontal_wrapped(|ui| {
                ui.strong(ctext::filter_heading());
                for (v, label, n) in [
                    (ListView::All, ctext::view_all(), counts.all),
                    (ListView::NeedsReview, ctext::view_needs_review(), counts.needs_review),
                    (ListView::Planned, ctext::adopt_label(), counts.planned),
                    (ListView::Rejected, ctext::reject_label(), counts.rejected),
                ] {
                    if ui.add(egui::Button::new(format!("{label} {n}")).selected(st.view == v)).clicked() {
                        st.view = v;
                    }
                }
                ui.separator();
                ui.label(i18n::render(|t| t.tag_confirm_filter_label().to_string()));
                ui.add(egui::TextEdit::singleline(&mut st.filter).desired_width(180.0).hint_text(i18n::render(|t| t.tag_confirm_filter_hint().to_string())));
                if !st.filter.is_empty() && ui.small_button("✕").clicked() {
                    st.filter.clear();
                }
                if st.rebuild.is_some() {
                    ui.add(egui::Spinner::new());
                }
                // 「オプション」は右端に寄せる。この行に入りきらないときは、次の行の右端に回す。
                if ui.available_size_before_wrap().x < OPTION_BTN_W + 8.0 {
                    ui.end_row();
                }
                let w = ui.available_size_before_wrap().x.max(OPTION_BTN_W);
                ui.allocate_ui_with_layout(egui::vec2(w, ui.spacing().interact_size.y), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    egui::containers::menu::MenuButton::new(ctext::option_button()).config(config).ui(ui, |ui| {
                        ui.label(egui::RichText::new(ctext::option_density_heading()).strong());
                        ui.radio_value(&mut st.compact, false, ctext::density_standard()).on_hover_text(i18n::as_ja(|| ctext::density_hint()));
                        ui.radio_value(&mut st.compact, true, ctext::density_compact()).on_hover_text(i18n::as_ja(|| ctext::density_hint()));
                        ui.separator();
                        ui.checkbox(&mut default_review, ctext::default_review_label()).on_hover_text(i18n::as_ja(|| ctext::default_review_hint()));
                        ui.separator();
                        if ui.button(ctext::settings_button()).on_hover_text(i18n::as_ja(|| ctext::settings_hint())).clicked() {
                            st.settings_open = true;
                            ui.close();
                        }
                    });
                });
            });
            // 2行目: 一括操作（3つを、最初から並べる）。
            ui.horizontal_wrapped(|ui| {
                ui.strong(ctext::bulk_heading());
                if ui.button(ctext::bulk_adopt_all()).clicked() {
                    for &i in &window_idx {
                        st.set.candidates[i].adopt();
                        confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                    }
                }
                if ui.button(ctext::bulk_reject_all()).clicked() {
                    for &i in &window_idx {
                        st.set.candidates[i].unadopt();
                        confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                    }
                }
                if ui.button(ctext::bulk_reject_unconfirmed()).clicked() {
                    for &i in &window_idx {
                        if st.set.candidates[i].unconfirmed {
                            st.set.candidates[i].unadopt();
                            confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                        }
                    }
                }
            });
            ui.separator();

            // 一覧（表）。列見出しは固定で、行は固定の高さで仮想スクロールする。
            let rows = rows_for(&st.set, stage, st.view, &st.confirmed, &st.filter, &opts);
            let compact = st.compact;
            let row_h = table_row_height(ui, compact);
            let footer_h = 110.0;
            let header_h = ui.text_style_height(&egui::TextStyle::Body) + 8.0 + ui.spacing().item_spacing.y;
            let list_h = (ui.available_height() - footer_h - header_h).max(80.0);
            draw_table_header(ui, ui.available_width() - ui.spacing().scroll.allocated_width());
            ui.scope(|ui| {
                // 行どうしの隙間は無くし、罫線で区切る（仮想スクロールの行高と一致させる）。
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical().id_salt(("tag_confirm_list", stage as u8)).max_height(list_h).auto_shrink([false, false]).show_rows(
                    ui,
                    row_h,
                    rows.len(),
                    |ui, range| {
                        let first = range.start;
                        for (k, row) in rows[range].iter().enumerate() {
                            match *row {
                                Row::Section(role) => band_row(ui, row_h, true, |ui| {
                                    ui.strong(band_title(role));
                                }),
                                Row::Cluster { id, size } => band_row(ui, row_h, false, |ui| {
                                    ui.weak(ctext::cluster_header(size));
                                    if ui.small_button(ctext::cluster_adopt_label()).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.adopt();
                                        }
                                    }
                                    if ui.small_button(ctext::cluster_reject_label()).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.unadopt();
                                        }
                                    }
                                    if ui.small_button(ctext::merge_label()).clicked() {
                                        row_action = Some(RowAction::MergeCluster(id));
                                    }
                                }),
                                Row::Cand(i) => draw_candidate_row(ui, &mut st.set, i, &st.master, &st.inputs, &mut row_action, &mut st.confirmed, t, first + k, compact),
                            }
                        }
                        if rows.is_empty() {
                            ui.weak(if st.view == ListView::NeedsReview && st.filter.is_empty() { ctext::needs_review_empty().to_string() } else { i18n::render(|t| t.tag_confirm_empty().to_string()) });
                        }
                    },
                );
            });

            // 下端のサマリー（この窓の分）と、ルールが働いたことの知らせ。
            ui.separator();
            let limits = crate::tag_candidates::recount_limits(&st.set.candidates);
            let new_count = match stage {
                Window::One => limits.window1_new,
                Window::Two => limits.window2_new,
            } + st.set.parent_new_elements(stage, &st.master);
            ui.horizontal_wrapped(|ui| {
                ui.label(ctext::summary(counts.planned, new_count, counts.rejected));
                if !st.set.rule_hits.is_empty() {
                    ui.separator();
                    if ui.small_button(ctext::rule_hits_button(st.set.rule_hits.len())).clicked() {
                        st.rules_dialog_open = true;
                    }
                }
                // 移し先が見つからず使えなかったカテゴリ振替ルールがあれば知らせる。
                if !st.set.category_rule_missing.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), ctext::missing_rules_notice(&st.set.category_rule_missing));
                }
                // 上限のため切り捨てた分があれば知らせる。
                if !st.truncated.is_empty() {
                    let rows: Vec<String> = st.truncated.iter().map(|&(r, n)| format!("{} {n}", role_label(r))).collect();
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), i18n::render(|t| t.tag_confirm_truncated(&rows.join(" / ")).to_string()));
                }
            });

            // 操作ボタン（2段）。1段目=窓の行き来、2段目=キャンセル（左）と適用（右）。適用は、最後の窓（作者・グループの窓。無ければ1枚目）で押せる。
            ui.separator();
            let any_adopted = st.set.candidates.iter().any(|c| c.is_adopted());
            // 上限を超えたまま（拡張も維持も選んでいない）の間は適用できない。
            let blocked = !st.limit_keep && !over_limit(&st.set.candidates, st.limit).is_empty();
            let apply_enabled = any_adopted && !blocked && self.spread_db.is_some() && self.tag_apply_job.is_none();
            let apply_hint = i18n::as_ja(|| if blocked {
                i18n::render(|t| t.tag_confirm_limit_pending().to_string()).to_string()
            } else if !any_adopted {
                i18n::render(|t| t.tag_confirm_apply_none().to_string()).to_string()
            } else {
                i18n::render(|t| t.tag_confirm_apply_pending().to_string()).to_string()
            });
            let has_two = st.set.candidates.iter().any(|c| c.role.window() == Window::Two);
            let last_stage = stage == Window::Two || !has_two;
            if has_two {
                ui.horizontal(|ui| {
                    if stage == Window::Two {
                        let back_enabled = st.set.candidates.iter().any(|c| c.role.window() == Window::One);
                        if ui.add_enabled(back_enabled, egui::Button::new(ctext::footer_back_label())).clicked() {
                            next_stage = Some(Window::One);
                        }
                    } else if ui.button(ctext::footer_next_label()).clicked() {
                        next_stage = Some(Window::Two);
                    }
                });
            }
            ui.horizontal(|ui| {
                // 左: 適用のできる窓では、前回の自動付与分を外して付け直すかを選べる（既定はOFF）。
                if last_stage {
                    ui.checkbox(&mut st.reattach, i18n::render(|t| t.tag_confirm_reattach().to_string())).on_hover_text(i18n::as_ja(|| i18n::render(|t| t.tag_confirm_reattach_hover().to_string())));
                }
                // 右: キャンセルを右端に置き、適用とは間隔を空ける（押し間違い対策）。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(i18n::render(|t| t.archive_open_cancel().to_string())).clicked() {
                        st.cancel_confirm = true;
                    }
                    if last_stage {
                        ui.add_space(32.0);
                        let btn = ui.add_enabled(apply_enabled, egui::Button::new(ctext::footer_apply()));
                        let btn = if apply_enabled { btn } else { btn.on_disabled_hover_text(apply_hint) };
                        if btn.clicked() {
                            apply_clicked = true;
                        }
                    }
                });
            });
        });

        // 冒頭の説明の「たたむ・開く」の変更を保存する（次回の窓も同じ状態で開く）。
        if st.intro_open != self.config.tag_confirm_intro_open {
            self.config.tag_confirm_intro_open = st.intro_open;
            self.persist_state();
        }
        // 「要確認を既定にする」チェックの変更を保存する。
        if default_review != self.config.tag_confirm_default_review {
            self.config.tag_confirm_default_review = default_review;
            self.persist_state();
        }
        // 照合の設定（別ポップアップ）。切り替えたら、下の `opts != before` で候補を作り直す。
        if open && st.settings_open {
            let mut close = false;
            let mut delete_rule: Option<usize> = None;
            let mut sep_action: Option<SepAction> = None;
            let modal = self.explorer_modal(ctx, "tag_match_settings").show(ctx, |ui| {
                ui.set_width(520.0);
                ui.heading(ctext::settings_title());
                ui.weak(ctext::settings_note());
                ui.add_space(6.0);
                ui.checkbox(&mut opts.width, i18n::render(|t| t.tag_confirm_opt_width().to_string()));
                ui.checkbox(&mut opts.space, i18n::render(|t| t.tag_confirm_opt_space().to_string()));
                ui.checkbox(&mut opts.hyphen_underscore, i18n::render(|t| t.tag_confirm_opt_hyphen().to_string()));
                ui.checkbox(&mut opts.symbols, i18n::render(|t| t.tag_confirm_opt_symbols().to_string()));
                ui.checkbox(&mut opts.romaji, i18n::render(|t| t.tag_confirm_opt_romaji().to_string()));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(i18n::render(|t| t.tag_confirm_threshold().to_string()));
                    let mut shown = st.threshold_draft.unwrap_or(opts.threshold);
                    let resp = ui.add(egui::Slider::new(&mut shown, 50..=100).suffix("%"));
                    if resp.dragged() {
                        // ドラッグ中は仮の値だけ動かし、離したときに確定する（毎フレームの作り直し・保存を避ける）。
                        st.threshold_draft = Some(shown);
                    } else {
                        if shown != opts.threshold {
                            opts.threshold = shown;
                        }
                        st.threshold_draft = None;
                    }
                    if st.rebuild.is_some() {
                        ui.add(egui::Spinner::new());
                    }
                });
                ui.weak(ctext::threshold_note());
                // 保存したカテゴリ振替ルールの一覧と削除。
                ui.add_space(8.0);
                ui.separator();
                ui.strong(ctext::rules_section_title());
                ui.weak(ctext::rules_section_note());
                if self.candidate_rules.is_read_only() {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), ctext::rules_read_only());
                }
                if self.candidate_rules.moves.is_empty() {
                    ui.weak(ctext::rules_empty());
                }
                egui::ScrollArea::vertical().id_salt("tag_move_rules").max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, r) in self.candidate_rules.moves.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(ctext::rule_line(rule_role_name(r.role), &r.name, &r.category));
                            if ui.small_button(ctext::rules_delete_label()).clicked() {
                                delete_rule = Some(i);
                            }
                        });
                    }
                });
                // 上位名の区切り記号。
                ui.add_space(8.0);
                ui.separator();
                ui.strong(ctext::separators_title());
                ui.weak(ctext::separators_note());
                let seps = self.candidate_rules.effective_separators();
                if seps.is_empty() {
                    ui.weak(ctext::separators_empty());
                }
                ui.horizontal_wrapped(|ui| {
                    for (i, sep) in seps.iter().enumerate() {
                        ui.label(egui::RichText::new(sep).strong());
                        if ui.small_button("×").on_hover_text(i18n::as_ja(|| ctext::rules_delete_label())).clicked() {
                            sep_action = Some(SepAction::Remove(i));
                        }
                        ui.add_space(6.0);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut st.sep_input).desired_width(80.0).hint_text(ctext::separators_add_hint()));
                    if ui.button(ctext::separators_add_label()).clicked() {
                        sep_action = Some(SepAction::Add);
                    }
                    if !self.candidate_rules.separators_are_default() && ui.button(ctext::separators_reset_label()).clicked() {
                        sep_action = Some(SepAction::Reset);
                    }
                });
                if let Some(err) = &st.sep_error {
                    ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
                }
                ui.add_space(8.0);
                if ui.button(ctext::close_label()).clicked() {
                    close = true;
                }
            });
            if let Some(action) = sep_action {
                st.sep_error = None;
                let changed = match action {
                    SepAction::Remove(i) => self.candidate_rules.remove_separator(i),
                    SepAction::Reset => {
                        self.candidate_rules.reset_separators();
                        true
                    }
                    SepAction::Add => match self.candidate_rules.add_separator(&st.sep_input) {
                        Ok(()) => {
                            st.sep_input.clear();
                            true
                        }
                        Err(e) => {
                            st.sep_error = Some(e.to_string());
                            false
                        }
                    },
                };
                if changed {
                    self.candidate_rules.save(&self.config.config_root);
                    st.separators = Arc::new(self.candidate_rules.effective_separators());
                    // いまの候補の上位名も、すぐ探し直す（扱いは変えない）。
                    st.set.detect_parents(&st.separators);
                }
            }
            if let Some(i) = delete_rule {
                if self.candidate_rules.remove_move_at(i) {
                    self.candidate_rules.save(&self.config.config_root);
                    st.move_rules = Arc::new(self.candidate_rules.moves.clone());
                }
            }
            if close || modal.should_close() {
                st.settings_open = false;
            }
        }

        // まとめるダイアログを開く・分解・先頭の別名の変更（描画中は並びを変えられないので、ここで行う）。
        match row_action {
            Some(RowAction::OpenMerge(i)) => st.open_merge_dialog(&[i]),
            Some(RowAction::OpenResolve(i)) => st.open_resolve_dialog(i),
            Some(RowAction::OpenBulkMove(i)) => st.open_bulk_move_dialog(i),
            Some(RowAction::OpenWhy(i)) => st.open_why_dialog(i),
            Some(RowAction::MergeCluster(id)) => {
                let idxs: Vec<usize> = (0..st.set.candidates.len())
                    .filter(|&i| st.set.candidates[i].cluster == id && st.set.candidates[i].role.window() == stage)
                    .collect();
                st.open_merge_dialog(&idxs);
            }
            Some(RowAction::Split(i)) => {
                st.set.split_candidate(i);
            }
            Some(RowAction::SetPrimary(i, pos)) => st.set.set_primary_alias(i, pos),
            None => {}
        }
        if opts != before {
            let clamped = opts.clamped();
            self.config.tag_match = clamped;
            self.persist_state();
            st.start_rebuild(clamped, i18n::t(), self.config.tag_register_name.name_lang(i18n::t()));
        }
        if let Some(s) = next_stage {
            st.stage = s;
            st.pick_initial_view(self.config.tag_confirm_default_review);
        }
        if apply_clicked && pending_review_total(&st.set, &st.confirmed) > 0 {
            // 要確認が残っているときだけ、適用の前に確認する。
            st.precheck = true;
            self.tag_confirm = Some(st);
        } else if apply_clicked {
            self.begin_apply(st);
        } else if open {
            // 上限を超えているとき: 拡張するか、維持して切り捨てるかを選ぶ（モーダル）。作り直し中は待つ。
            if !st.limit_keep && st.rebuild.is_none() {
                let over = over_limit(&st.set.candidates, st.limit);
                if !over.is_empty() {
                    self.draw_tag_limit_dialog(ctx, &mut st, &over);
                }
            }
            // 書き換えの確認（モーダル）。
            let mut proceed = false;
            if let Some(review) = &st.rename_review {
                let mut back = false;
                let modal = self.explorer_modal(ctx, "tag_rename_review").show(ctx, |ui| {
                    ui.set_width(460.0);
                    ui.heading(i18n::render(|t| t.tag_rename_review_title().to_string()));
                    ui.label(i18n::render(|t| t.tag_rename_review_intro(review.renames.len()).to_string()));
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (old, new) in &review.renames {
                            ui.label(format!("{old}  →  {new}"));
                        }
                    });
                    if review.conflicts > 0 {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::from_rgb(230, 160, 40), i18n::render(|t| t.tag_rename_review_conflicts(review.conflicts).to_string()));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(i18n::render(|t| t.tag_confirm_back().to_string())).clicked() {
                            back = true;
                        }
                        if ui.button(i18n::render(|t| t.tag_rename_review_apply().to_string())).clicked() {
                            proceed = true;
                        }
                    });
                });
                if back || modal.should_close() {
                    st.rename_review = None;
                }
            }
            // 適用前の確認（要確認が残っているとき）。
            let mut precheck_go = false;
            if st.precheck {
                let mut back = false;
                let pending = pending_review_total(&st.set, &st.confirmed);
                let modal = self.explorer_modal(ctx, "tag_precheck").show(ctx, |ui| {
                    ui.set_width(460.0);
                    ui.heading(ctext::precheck_title());
                    ui.add_space(4.0);
                    ui.label(ctext::precheck_body(pending));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(ctext::precheck_back()).clicked() {
                            back = true;
                        }
                        if ui.button(ctext::precheck_go()).clicked() {
                            precheck_go = true;
                        }
                    });
                });
                if back || modal.should_close() {
                    st.precheck = false;
                    // 「要確認」の表示に切り替えて、確認を始めやすくする。
                    st.view = ListView::NeedsReview;
                }
            }
            // キャンセルの確認（選択を全部捨てる操作なので、一手間かける）。
            let mut cancel_go = false;
            if st.cancel_confirm {
                let mut keep = false;
                let modal = self.explorer_modal(ctx, "tag_cancel_confirm").show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading(ctext::cancel_confirm_title());
                    ui.add_space(4.0);
                    ui.label(ctext::cancel_confirm_body());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(ctext::cancel_confirm_continue()).clicked() {
                            keep = true;
                        }
                        if ui.button(ctext::cancel_confirm_go()).clicked() {
                            cancel_go = true;
                        }
                    });
                });
                if keep || modal.should_close() {
                    st.cancel_confirm = false;
                }
            }
            self.draw_tag_merge_dialog(ctx, &mut st);
            self.draw_tag_bulk_move_dialog(ctx, &mut st);
            self.draw_tag_resolve_dialog(ctx, &mut st);
            self.draw_tag_why_dialog(ctx, &mut st);
            self.draw_tag_rule_hits_dialog(ctx, &mut st);
            self.draw_tag_faq_dialog(ctx, &mut st);
            if cancel_go {
                // 確認窓を閉じる（st を戻さない）。
            } else if precheck_go {
                st.precheck = false;
                self.begin_apply(st);
            } else if proceed {
                self.start_tag_apply(&st);
            } else {
                self.tag_confirm = Some(st);
            }
        }
    }
}

impl NekoviewApp {
    /// 適用を始める。既存要素の名前が書き換わるなら、先に一覧を見せて確認する。
    fn begin_apply(&mut self, mut st: TagConfirm) {
        let mut master = self.tag_manager_categories.clone();
        let mut next = self.tag_manager_next_tier_id;
        let plan = plan_application(&st.set, &mut master, &mut next, i18n::t());
        if plan.renames.is_empty() {
            self.start_tag_apply(&st);
        } else {
            st.rename_review = Some(RenameReview {
                renames: plan.renames.iter().map(|(_, _, old, new)| (old.clone(), new.clone())).collect(),
                conflicts: plan.alias_conflicts,
            });
            self.tag_confirm = Some(st);
        }
    }
}

impl NekoviewApp {
    /// 上限超過のダイアログ。「拡張する」は上限を増やす（まだ超えていれば次のフレームでまた出る）。
    /// 「上限を維持する」は、該当ファイル数の多い要素から上限まで残して、あとを切り捨てる。
    fn draw_tag_limit_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm, over: &[(Role, usize)]) {
        let rows: Vec<String> = over.iter().map(|&(r, n)| i18n::render(|t| t.tag_limit_row(&role_label_for(t, r), n, n - st.limit).to_string())).collect();
        let db_size = std::fs::metadata(crate::dev_db_backup::db_path(&self.config.config_root))
            .map(|m| super::db_backup_ui::format_size(m.len()))
            .unwrap_or_else(|_| "-".to_owned());
        let (mut expand, mut keep) = (false, false);
        self.explorer_modal(ctx, "tag_limit_dialog").show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(ctext::limit_title());
            ui.add_space(4.0);
            ui.label(ctext::limit_body(st.limit, &rows.join("\n"), &db_size, LIMIT_EXPAND_STEP));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(format!("{}{}", i18n::render(|t| t.tag_limit_expand(LIMIT_EXPAND_STEP).to_string()), ctext::recommended_suffix())).clicked() {
                    expand = true;
                }
                if ui.button(i18n::render(|t| t.tag_limit_keep().to_string())).clicked() {
                    keep = true;
                }
            });
        });
        if expand {
            st.limit += LIMIT_EXPAND_STEP;
        } else if keep {
            st.limit_keep = true;
            st.truncated = st.set.truncate_to_limit(st.limit);
        }
    }
}

impl NekoviewApp {
    /// 「他の要素と同一としてまとめる」ダイアログ。用途とデメリットを案内し、同じ種類の候補から相手を選ばせる。
    /// 実行すると、選んだ候補を別名つきの1要素にまとめ、その行を「タグ採用予定」にする。
    fn draw_tag_merge_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        let Some(mut md) = st.merge_dialog.take() else { return };
        let t = i18n::t();
        let opts = self.config.tag_match;
        let role = md.role;
        let (mut ok, mut cancel) = (false, false);
        let modal = self.explorer_modal(ctx, "tag_merge_dialog").show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(ctext::merge_title());
            ui.add_space(4.0);
            ui.label(ctext::merge_intro(&md.name));
            ui.add_space(6.0);
            ui.weak(i18n::t().tag_confirm_filter_label());
            ui.add(egui::TextEdit::singleline(&mut md.filter).desired_width(240.0).hint_text(i18n::render(|t| t.tag_confirm_filter_hint().to_string())));
            // 同じ種類の候補。選んだものは、絞り込みに合わなくても残す。
            let idx: Vec<usize> = (0..st.set.candidates.len())
                .filter(|&i| {
                    let c = &st.set.candidates[i];
                    c.role == role && (md.selected.contains(&c.key) || matches_query(c, &md.filter, &opts))
                })
                .collect();
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().id_salt("tag_merge_list").max_height(200.0).auto_shrink([false, true]).show_rows(ui, row_h, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let c = &st.set.candidates[i];
                    let mut on = md.selected.contains(&c.key);
                    if ui.checkbox(&mut on, format!("{}　{}", c.name, ctext::file_count(c.files.len()))).changed() {
                        if on {
                            md.selected.insert(c.key.clone());
                        } else {
                            md.selected.remove(&c.key);
                        }
                    }
                }
            });
            let chosen: Vec<&Candidate> = st.set.candidates.iter().filter(|c| c.role == role && md.selected.contains(&c.key)).collect();
            let names: Vec<&str> = chosen.iter().map(|c| c.name.as_str()).collect();
            let files: HashSet<usize> = chosen.iter().flat_map(|c| c.files.iter().copied()).collect();
            ui.add_space(4.0);
            ui.label(ctext::merge_preview(&names, files.len()));
            ui.add_space(6.0);
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), ctext::merge_warning_heading());
            for line in ctext::merge_consequences() {
                ui.label(format!("・{line}"));
            }
            if let Some(msg) = &md.msg {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(220, 60, 60), msg);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(i18n::render(|t| t.archive_open_cancel().to_string())).clicked() {
                    cancel = true;
                }
                if ui.add_enabled(chosen.len() >= 2, egui::Button::new(ctext::merge_ok_label())).clicked() {
                    ok = true;
                }
            });
        });
        if cancel || modal.should_close() {
            return;
        }
        if ok {
            let idxs: Vec<usize> = (0..st.set.candidates.len())
                .filter(|&i| st.set.candidates[i].role == role && md.selected.contains(&st.set.candidates[i].key))
                .collect();
            let id = st.next_bundle_id;
            match st.set.merge_candidates(&idxs, None, id, &st.master) {
                Ok(merged) => {
                    st.next_bundle_id += 1;
                    // まとめた結果は、必ず「タグ採用予定」にする。
                    st.set.candidates[merged].adopt();
                    return;
                }
                Err(e) => md.msg = Some(merge_error_text(&e, t)),
            }
        }
        st.merge_dialog = Some(md);
    }
}

impl NekoviewApp {
    /// 「まとめて別のカテゴリへ移す」ダイアログ。同じ種類の候補を複数選び、登録先のカテゴリを1つ選ぶ。
    /// 移した候補は「新規に登録」になる。「次回以降も」を選ぶと、まとめていない候補の名前をルールとして保存する。
    fn draw_tag_bulk_move_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        let Some(mut bd) = st.bulk_move_dialog.take() else { return };
        let opts = self.config.tag_match;
        let role = bd.role;
        let (mut ok, mut cancel) = (false, false);
        // 移し先にできるカテゴリ（同じ種類は、抽出元のカテゴリも同じ）。
        let targets: Vec<(usize, String)> = st.set.candidates.iter().find(|c| c.role == role).map(|c| ctext::move_targets(c, &st.master)).unwrap_or_default();
        let modal = self.explorer_modal(ctx, "tag_bulk_move_dialog").show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(ctext::bulk_move_title());
            ui.add_space(4.0);
            ui.label(ctext::bulk_move_intro(role));
            ui.add_space(6.0);
            ui.weak(i18n::render(|t| t.tag_confirm_filter_label().to_string()));
            ui.add(egui::TextEdit::singleline(&mut bd.filter).desired_width(240.0).hint_text(i18n::render(|t| t.tag_confirm_filter_hint().to_string())));
            let idx: Vec<usize> = (0..st.set.candidates.len())
                .filter(|&i| {
                    let c = &st.set.candidates[i];
                    c.role == role && (bd.selected.contains(&c.key) || matches_query(c, &bd.filter, &opts))
                })
                .collect();
            // 表示中の候補を、まとめて選ぶ・外す。
            ui.horizontal(|ui| {
                if ui.small_button(ctext::select_shown_label()).clicked() {
                    for &i in &idx {
                        bd.selected.insert(st.set.candidates[i].key.clone());
                    }
                }
                if ui.small_button(ctext::deselect_all_label()).clicked() {
                    bd.selected.clear();
                }
            });
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().id_salt("tag_bulk_move_list").max_height(220.0).auto_shrink([false, true]).show_rows(ui, row_h, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let c = &st.set.candidates[i];
                    let mut on = bd.selected.contains(&c.key);
                    if ui.checkbox(&mut on, format!("{}　{}", c.name, ctext::file_count(c.files.len()))).changed() {
                        if on {
                            bd.selected.insert(c.key.clone());
                        } else {
                            bd.selected.remove(&c.key);
                        }
                    }
                }
            });
            ui.add_space(6.0);
            // 扱い方: 別のカテゴリの新しい要素として登録する／既存の要素に結びつける（別名を足す）。
            ui.horizontal(|ui| {
                ui.radio_value(&mut bd.link_on, false, ctext::bulk_mode_move());
                ui.radio_value(&mut bd.link_on, true, ctext::bulk_mode_link());
            });
            if bd.link_on {
                ui.weak(ctext::bulk_link_desc());
                draw_link_picker(ui, "tag_bulk_link", &st.master, &mut bd.link_cat, &mut bd.link_tier, &mut bd.link_filter);
                ui.weak(ctext::link_note());
            } else if targets.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(200, 140, 0), ctext::move_no_target());
            } else {
                ui.horizontal(|ui| {
                    ui.label(ctext::move_pick_label());
                    let current = bd.target.and_then(|i| targets.iter().find(|t| t.0 == i)).map_or_else(|| ctext::move_pick_placeholder().to_string(), |t| t.1.clone());
                    egui::ComboBox::from_id_salt("tag_bulk_move_cat").selected_text(current).show_ui(ui, |ui| {
                        for (i, name) in &targets {
                            if ui.selectable_label(bd.target == Some(*i), name).clicked() {
                                bd.target = Some(*i);
                            }
                        }
                    });
                });
                ui.checkbox(&mut bd.remember, ctext::remember_label()).on_hover_text(i18n::as_ja(|| ctext::remember_hint()));
            }
            let chosen: Vec<&Candidate> = st.set.candidates.iter().filter(|c| c.role == role && bd.selected.contains(&c.key)).collect();
            let files: HashSet<usize> = chosen.iter().flat_map(|c| c.files.iter().copied()).collect();
            let cat_name = bd.target.and_then(|i| targets.iter().find(|t| t.0 == i)).map(|t| t.1.as_str());
            ui.add_space(4.0);
            if bd.link_on {
                let names: Vec<String> = chosen.iter().flat_map(|c| c.alias_list()).collect();
                let element = bd.link_cat.zip(bd.link_tier).and_then(|(ci, tid)| st.master.get(ci).map(|cat| (cat.display_name(), tid)));
                let full = element.as_ref().and_then(|(_, tid)| st.master.iter().flat_map(|c| c.tiers.iter()).find(|t| t.id == *tid)).and_then(|t| t.element.clone());
                ui.label(ctext::bulk_link_preview(chosen.len(), files.len(), element.as_ref().map(|e| e.0.as_str()).zip(full.as_deref()), &names));
            } else {
                ui.label(ctext::bulk_move_preview(chosen.len(), files.len(), cat_name));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(i18n::render(|t| t.archive_open_cancel().to_string())).clicked() {
                    cancel = true;
                }
                let ready = !chosen.is_empty() && if bd.link_on { bd.link_cat.is_some() && bd.link_tier.is_some() } else { bd.target.is_some() };
                let label = if bd.link_on { ctext::bulk_link_ok_label() } else { ctext::bulk_move_ok_label() };
                if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                    ok = true;
                }
            });
        });
        if cancel || modal.should_close() {
            return;
        }
        if ok && bd.link_on {
            // 選んだ候補すべてに、同じ既存要素への別名として結びつける（登録先のカテゴリは、もとのままに戻す）。
            let (Some(cat_idx), Some(tier_id)) = (bd.link_cat, bd.link_tier) else { return };
            for c in st.set.candidates.iter_mut().filter(|c| c.role == role && bd.selected.contains(&c.key)) {
                c.restore_category();
                c.choice = Choice::Alias { cat_idx, tier_id };
                st.confirmed.insert((c.role, c.key.clone()));
            }
            return;
        }
        if ok {
            let Some(cat) = bd.target else { return };
            let mut rules_changed = false;
            let category = st.master.get(cat).map(|m| m.display_name());
            let rule_role = role.rule_role();
            for c in st.set.candidates.iter_mut().filter(|c| c.role == role && bd.selected.contains(&c.key)) {
                c.move_to_category(cat);
                // まとめた候補は、名前が1つに決まらないのでルールにしない。
                if c.members.is_empty() {
                    if bd.remember {
                        if let Some(category) = &category {
                            self.candidate_rules.upsert_move(rule_role, &c.name, category);
                            rules_changed = true;
                        }
                    } else if self.candidate_rules.remove_move(rule_role, &c.name) {
                        rules_changed = true;
                    }
                }
                st.confirmed.insert((c.role, c.key.clone()));
            }
            if rules_changed {
                self.candidate_rules.save(&self.config.config_root);
                st.move_rules = Arc::new(self.candidate_rules.moves.clone());
            }
            return;
        }
        st.bulk_move_dialog = Some(bd);
    }
}

impl NekoviewApp {
    /// 変更ダイアログ。選択肢ごとに、何を・どこに・どうなるかを説明する。選ぶと、その行の扱いが変わり、確認済みになる。
    fn draw_tag_resolve_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        let Some(mut rd) = st.resolve_dialog.take() else { return };
        let Some(idx) = st.set.candidates.iter().position(|c| c.role == rd.role && c.key == rd.key) else { return };
        let (heading, note, options, movable, targets, move_desc, previews, can_remember, rule_exists, parent_opts, parent_title) = {
            let c = &st.set.candidates[idx];
            let rule_exists = self.candidate_rules.find_move(c.role.rule_role(), &c.name).is_some();
            let targets = ctext::move_targets(c, &st.master);
            let previews: Vec<(usize, String)> = targets.iter().map(|(i, _)| (*i, ctext::move_preview(c, &st.master, *i))).collect();
            (
                ctext::resolve_heading(c),
                ctext::similarity_note(c),
                ctext::resolve_options(c, &st.master),
                ctext::can_move_category(c),
                targets,
                ctext::move_desc(c, &st.master),
                previews,
                c.members.is_empty(),
                rule_exists,
                ctext::parent_options(c),
                c.parent_name.as_deref().map(ctext::parent_heading),
            )
        };
        let amber = egui::Color32::from_rgb(230, 160, 40);
        let (mut ok, mut cancel) = (false, false);
        let modal = self.explorer_modal(ctx, "tag_resolve_dialog").show(ctx, |ui| {
            ui.set_width(620.0);
            ui.heading(heading);
            ui.weak(ctext::resolve_subtitle());
            if let Some(n) = note {
                ui.add_space(4.0);
                ui.colored_label(amber, n);
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("tag_resolve_options").max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                for o in &options {
                    let selected = !rd.link_on && rd.move_cat.is_none() && rd.sel == o.choice;
                    let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                    let framed = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.radio(selected, "").clicked() {
                                rd.sel = o.choice.clone();
                                rd.move_cat = None;
                                rd.link_on = false;
                            }
                            ui.label(egui::RichText::new(&o.title).strong());
                        });
                        ui.label(egui::RichText::new(&o.desc).weak());
                    });
                    if framed.response.interact(egui::Sense::click()).clicked() {
                        rd.sel = o.choice.clone();
                        rd.move_cat = None;
                        rd.link_on = false;
                    }
                }
                // 登録先のカテゴリを選び直す（抽出された種類が実際と違うとき）。
                if movable {
                    let selected = !rd.link_on && rd.move_cat.is_some();
                    let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                    egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!targets.is_empty(), egui::RadioButton::new(selected, "")).clicked() {
                                rd.move_cat = rd.move_cat.or(targets.first().map(|t| t.0));
                                rd.sel = Choice::New; rd.link_on = false;
                            }
                            // 枠全体にクリックを付けると、内側のプルダウン・チェックのクリックを奪うので、見出しだけを押せるようにする。
                            if ui.add_enabled(!targets.is_empty(), egui::Label::new(egui::RichText::new(ctext::move_title()).strong()).sense(egui::Sense::click())).clicked() {
                                rd.move_cat = rd.move_cat.or(targets.first().map(|t| t.0));
                                rd.sel = Choice::New; rd.link_on = false;
                            }
                        });
                        ui.label(egui::RichText::new(&move_desc).weak());
                        if targets.is_empty() {
                            ui.colored_label(amber, ctext::move_no_target());
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(ctext::move_pick_label());
                                let current = rd
                                    .move_cat
                                    .and_then(|i| targets.iter().find(|t| t.0 == i))
                                    .map_or_else(|| ctext::move_pick_placeholder().to_string(), |t| t.1.clone());
                                egui::ComboBox::from_id_salt("tag_resolve_move_cat").selected_text(current).show_ui(ui, |ui| {
                                    for (i, name) in &targets {
                                        if ui.selectable_label(rd.move_cat == Some(*i), name).clicked() {
                                            rd.move_cat = Some(*i);
                                            rd.sel = Choice::New; rd.link_on = false;
                                        }
                                    }
                                });
                            });
                            if let Some(preview) = rd.move_cat.and_then(|i| previews.iter().find(|p| p.0 == i)) {
                                ui.label(&preview.1);
                            }
                            // 次回以降も同じ扱いにする（ルールとして保存）。まとめた候補には出さない。
                            if can_remember && rd.move_cat.is_some() {
                                ui.checkbox(&mut rd.remember, ctext::remember_label()).on_hover_text(i18n::as_ja(|| ctext::remember_hint()));
                            }
                            if rule_exists {
                                ui.weak(ctext::rule_exists_note());
                            }
                        }
                    });
                }
                // 既存の要素に結びつける（別名を足す）。メインを含む任意のカテゴリの、任意の要素を選べる。
                {
                    let selected = rd.link_on;
                    let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                    let link_cats = ctext::link_categories(&st.master);
                    egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!link_cats.is_empty(), egui::RadioButton::new(selected, "")).clicked() {
                                rd.link_on = true;
                            }
                            // 枠全体にクリックを付けると、内側の部品のクリックを奪うので、見出しだけを押せるようにする。
                            if ui.add_enabled(!link_cats.is_empty(), egui::Label::new(egui::RichText::new(ctext::link_title()).strong()).sense(egui::Sense::click())).clicked() {
                                rd.link_on = true;
                            }
                        });
                        ui.label(egui::RichText::new(ctext::link_desc(&st.set.candidates[idx])).weak());
                        if draw_link_picker(ui, "tag_resolve_link", &st.master, &mut rd.link_cat, &mut rd.link_tier, &mut rd.link_filter) {
                            rd.link_on = true;
                        }
                        if let (Some(cat_idx), Some(tier)) = (rd.link_cat, rd.link_tier.filter(|_| rd.link_on)) {
                            ui.label(ctext::link_preview(&st.set.candidates[idx], &st.master, cat_idx, tier));
                        }
                        ui.weak(ctext::link_note());
                    });
                }
                // 上位名（COMIC1★15 に対する COMIC1）の扱い。
                if let (Some(opts), Some(title)) = (&parent_opts, &parent_title) {
                    ui.add_space(6.0);
                    ui.strong(title);
                    for o in opts {
                        let selected = rd.parent_mode == o.mode;
                        let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                        let framed = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if ui.radio(selected, "").clicked() {
                                    rd.parent_mode = o.mode;
                                }
                                ui.label(egui::RichText::new(&o.title).strong());
                            });
                            ui.label(egui::RichText::new(&o.desc).weak());
                        });
                        if framed.response.interact(egui::Sense::click()).clicked() {
                            rd.parent_mode = o.mode;
                        }
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(i18n::render(|t| t.archive_open_cancel().to_string())).clicked() {
                    cancel = true;
                }
                // 結びつけを選んでいるときは、要素を選ぶまで押せない。
                let link_ready = !rd.link_on || (rd.link_cat.is_some() && rd.link_tier.is_some());
                if ui.add_enabled(link_ready, egui::Button::new(ctext::resolve_ok_label())).clicked() {
                    ok = true;
                }
            });
        });
        if cancel || modal.should_close() {
            return;
        }
        if ok {
            let mut rules_changed = false;
            {
                let c = &mut st.set.candidates[idx];
                // 上位名の選択肢を出した候補だけ、扱いを反映する。
                if parent_opts.is_some() {
                    c.parent_mode = rd.parent_mode;
                }
                match (rd.link_on, rd.link_cat, rd.link_tier, rd.move_cat) {
                    // 既存の要素に別名を足して結びつける。登録先のカテゴリは、もとのままに戻す。
                    (true, Some(cat_idx), Some(tier_id), _) => {
                        c.restore_category();
                        c.choice = Choice::Alias { cat_idx, tier_id };
                    }
                    (_, _, _, Some(cat)) => {
                        c.move_to_category(cat);
                        // 次回以降も同じ扱いにするなら保存し、外したら、保存してあったルールを消す。
                        if c.members.is_empty() {
                            let role = c.role.rule_role();
                            if rd.remember {
                                if let Some(category) = st.master.get(cat).map(|m| m.display_name()) {
                                    self.candidate_rules.upsert_move(role, &c.name, &category);
                                    rules_changed = true;
                                }
                            } else if self.candidate_rules.remove_move(role, &c.name) {
                                rules_changed = true;
                            }
                        }
                    }
                    _ => {
                        c.restore_category();
                        c.choice = rd.sel.clone();
                    }
                }
            }
            if rules_changed {
                self.candidate_rules.save(&self.config.config_root);
                st.move_rules = Arc::new(self.candidate_rules.moves.clone());
            }
            st.confirmed.insert((rd.role, rd.key.clone()));
            return;
        }
        st.resolve_dialog = Some(rd);
    }

    /// 理由ダイアログ。要確認になった理由と、どうすればよいかを長文で見せる。「解決する…」で変更ダイアログへ進める。
    fn draw_tag_why_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        let Some((role, key)) = st.why_dialog.take() else { return };
        let Some(idx) = st.set.candidates.iter().position(|c| c.role == role && c.key == key) else { return };
        let c = &st.set.candidates[idx];
        let Some(kind) = ctext::review_kind(c) else { return };
        let title = ctext::why_title(&c.name);
        let sections = ctext::review_long(&kind, c, &st.master);
        let (mut close, mut resolve) = (false, false);
        let modal = self.explorer_modal(ctx, "tag_why_dialog").show(ctx, |ui| {
            ui.set_width(580.0);
            ui.heading(title);
            for (head, body) in &sections {
                ui.add_space(6.0);
                ui.strong(head);
                ui.label(body);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(ctext::close_label()).clicked() {
                    close = true;
                }
                if ui.button(ctext::resolve_button_label()).clicked() {
                    resolve = true;
                }
            });
        });
        if resolve {
            st.open_resolve_dialog(idx);
            return;
        }
        if close || modal.should_close() {
            return;
        }
        st.why_dialog = Some((role, key));
    }
}

impl NekoviewApp {
    /// よくある質問のポップアップ。各質問は、見出しをクリックして開閉する。
    fn draw_tag_faq_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        if !st.faq_open {
            return;
        }
        let mut close = false;
        let modal = self.explorer_modal(ctx, "tag_faq_dialog").show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(ctext::faq_title());
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("tag_faq_list").max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                for (q, a) in ctext::faq_items() {
                    egui::CollapsingHeader::new(egui::RichText::new(q).strong()).id_salt(q).show(ui, |ui| {
                        ui.label(a);
                    });
                }
            });
            ui.add_space(8.0);
            if ui.button(ctext::close_label()).clicked() {
                close = true;
            }
        });
        if close || modal.should_close() {
            st.faq_open = false;
        }
    }
}

impl NekoviewApp {
    /// 「ルールが働いた候補」ポップアップ。成立したルールの名前とファイル数を並べる。
    fn draw_tag_rule_hits_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        if !st.rules_dialog_open {
            return;
        }
        let hits: Vec<String> = st.set.rule_hits.iter().map(|(name, n)| ctext::rule_hit_line(name, *n)).collect();
        let mut close = false;
        let modal = self.explorer_modal(ctx, "tag_rule_hits_dialog").show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(ctext::rule_hits_title());
            ui.add_space(4.0);
            for line in &hits {
                ui.label(format!("・{line}"));
            }
            ui.add_space(6.0);
            ui.weak(ctext::rule_hits_note());
            ui.add_space(8.0);
            if ui.button(ctext::close_label()).clicked() {
                close = true;
            }
        });
        if close || modal.should_close() {
            st.rules_dialog_open = false;
        }
    }
}

/// 要確認の行なら、確認済みにする（一括操作で採否を決めたとき）。
fn confirm_if_flagged(confirmed: &mut HashSet<(Role, String)>, c: &Candidate) {
    if ctext::needs_review(c) {
        confirmed.insert((c.role, c.key.clone()));
    }
}

fn merge_error_text(e: &MergeError, _t: Lang) -> String {
    match e {
        MergeError::TooFew => i18n::render(|t| t.tag_confirm_merge_err_too_few().to_string()).to_string(),
        MergeError::MixedRoles => i18n::render(|t| t.tag_confirm_merge_err_mixed().to_string()).to_string(),
        MergeError::MultipleExisting { first, second } => i18n::render(|t| t.tag_confirm_merge_err_multiple(first, second).to_string()),
        MergeError::AliasOverlap { alias, element } => i18n::render(|t| t.tag_confirm_merge_err_overlap(alias, element).to_string()),
    }
}

// ── 一覧（表） ──────────────────────────────────────────────────────

/// 採否トグルの列の幅。
const TOGGLE_COL_W: f32 = 124.0;
/// 「⋯」の列の幅。
const OPS_COL_W: f32 = 150.0;
/// 「オプション」ボタンのおおよその幅（右端寄せの判定用）。
const OPTION_BTN_W: f32 = 110.0;
/// セルの左右の余白。
const CELL_PAD: f32 = 6.0;

/// 表の列の境目（行の左端からの距離）。名前の列だけ、窓の幅に対する比率（約24%）で決める。
/// 要確認の列は、残りの幅を全部使う。長い文字は折り返さず、切り詰めてホバーで全文を出す。
#[derive(Clone, Copy)]
struct TableCols {
    total: f32,
    name: f32,
}

impl TableCols {
    fn new(total: f32) -> Self {
        let rest = (total - TOGGLE_COL_W - OPS_COL_W - 120.0).max(100.0);
        Self { total, name: (total * 0.27).clamp(150.0, 320.0).min(rest) }
    }
    fn x_name(self) -> f32 {
        TOGGLE_COL_W
    }
    fn x_state(self) -> f32 {
        TOGGLE_COL_W + self.name
    }
    fn x_ops(self) -> f32 {
        (self.total - OPS_COL_W).max(self.x_state())
    }
}

/// 行の高さ。標準は2段、コンパクトは1段。一覧は固定の行高で仮想スクロールするので、全ての行をこの高さにそろえる。
fn table_row_height(ui: &egui::Ui, compact: bool) -> f32 {
    if compact {
        ui.spacing().interact_size.y + 6.0
    } else {
        ui.spacing().interact_size.y + ui.text_style_height(&egui::TextStyle::Body) + 12.0
    }
}

/// 行の1段目の高さ（コンパクトでは行全体）。
fn table_line1_height(ui: &egui::Ui, compact: bool) -> f32 {
    if compact {
        table_row_height(ui, compact)
    } else {
        ui.spacing().interact_size.y + 4.0
    }
}

/// 矩形 `r` の中にだけ描くセル。はみ出した分は切り取る。
/// 親の `Ui` には、セルの大きさを返さない（`new_child`）。中身がセルより広くても、行の幅や、次の行の位置が動かない
/// （`scope_builder` だと、はみ出した幅が親に伝わり、行を重ねるたびに列がじわじわ右へずれる）。
fn in_cell(ui: &mut egui::Ui, r: egui::Rect, layout: egui::Layout, add: impl FnOnce(&mut egui::Ui)) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(layout));
    child.set_clip_rect(r.intersect(ui.clip_rect()));
    add(&mut child);
}

/// 種類の見出し・類似品の見出しの行（表の行と同じ高さの帯）。
fn band_row(ui: &mut egui::Ui, row_h: f32, center: bool, add: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);
    let layout = if center {
        egui::Layout::centered_and_justified(egui::Direction::LeftToRight)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    in_cell(ui, rect.shrink2(egui::vec2(CELL_PAD, 0.0)), layout, add);
}

/// 一覧の列見出し（スクロールしない）。
fn draw_table_header(ui: &mut egui::Ui, total: f32) {
    let h = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, h), egui::Sense::hover());
    let cols = TableCols::new(total);
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    let color = ui.visuals().weak_text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    let y = rect.center().y;
    let put = |x: f32, align: egui::Align2, text: &str| {
        painter.text(egui::pos2(rect.left() + x, y), align, text, font.clone(), color);
    };
    put(CELL_PAD, egui::Align2::LEFT_CENTER, ctext::col_toggle());
    put(cols.x_name() + CELL_PAD, egui::Align2::LEFT_CENTER, ctext::col_name());
    put(cols.x_state() + CELL_PAD, egui::Align2::LEFT_CENTER, ctext::col_state());
    for x in [cols.x_name(), cols.x_state(), cols.x_ops()] {
        painter.vline(rect.left() + x, rect.y_range(), stroke);
    }
    painter.hline(rect.x_range(), rect.top(), stroke);
    painter.hline(rect.x_range(), rect.bottom(), stroke);
}

/// 名前列に、元のファイル名を足す。候補の元になった部分は色つき・下線つきで強調し、ファイル名に無い表記には薄い注記、他にも付くファイルがあれば「（他N件）」を添える。
fn append_source(job: &mut egui::text::LayoutJob, ui: &egui::Ui, src: &ctext::SourceFile) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let weak = ui.visuals().weak_text_color();
    let plain = egui::TextFormat { font_id: font.clone(), color: weak, ..Default::default() };
    match src.range {
        Some((a, b)) if src.name.is_char_boundary(a) && src.name.is_char_boundary(b) => {
            let hit = egui::TextFormat { font_id: font, color: ui.visuals().hyperlink_color, underline: egui::Stroke::new(1.0, ui.visuals().hyperlink_color), ..Default::default() };
            job.append(&src.name[..a], 0.0, plain.clone());
            job.append(&src.name[a..b], 0.0, hit);
            job.append(&src.name[b..], 0.0, plain.clone());
        }
        _ => {
            job.append(&src.name, 0.0, plain.clone());
            if src.range.is_none() {
                job.append(ctext::source_not_in_name(), 4.0, plain.clone());
            }
        }
    }
    if src.others > 0 {
        job.append(&ctext::source_others(src.others), 4.0, plain);
    }
}

/// 名前列の先頭に出す、対象のファイル数「N件」。
fn append_count(job: &mut egui::text::LayoutJob, ui: &egui::Ui, n: usize) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    job.append(&ctext::file_count_short(n), 0.0, egui::TextFormat { font_id: font, color: ui.visuals().text_color(), ..Default::default() });
    job.append("", 8.0, egui::TextFormat::default());
}

/// 名前列のホバー: 見出し「◆対象となるファイル」と、この候補が付くファイル名の番号つき一覧。
fn show_name_hover(resp: egui::Response, list: &(Vec<String>, usize)) -> egui::Response {
    if list.0.is_empty() {
        return resp;
    }
    resp.on_hover_ui(|ui| {
        ui.label(egui::RichText::new(i18n::as_ja(|| ctext::source_hover_heading())).strong());
        for (n, name) in list.0.iter().enumerate() {
            ui.label(ctext::source_hover_line(n + 1, name));
        }
        if list.1 > 0 {
            ui.weak(i18n::as_ja(|| ctext::source_others(list.1)));
        }
    })
}

/// 候補1行。列は、採否・名前・ファイル数・要確認（選択欄）・「⋯」の固定位置。
/// 標準は2段で、「内容・要確認」の列の1段目に要確認の印・選択欄、2段目に何をどこにどうするかの1文を出す。コンパクトは1段で、その列に両方を並べる。
/// 長い文字は折り返さず、切り詰めてホバーで全文を出す。
fn draw_candidate_row(
    ui: &mut egui::Ui,
    set: &mut CandidateSet,
    i: usize,
    master: &[TagManagerCategoryUi],
    inputs: &[CandidateInput],
    action: &mut Option<RowAction>,
    confirmed: &mut HashSet<(Role, String)>,
    _t: Lang,
    row_no: usize,
    compact: bool,
) {
    let reason = ctext::review_kind(&set.candidates[i]);
    let id = (set.candidates[i].role, set.candidates[i].key.clone());
    // 要確認のうち、ユーザーが確認済みのもの。
    let is_confirmed = reason.is_some() && confirmed.contains(&id);
    let conclusion = ctext::conclusion(&set.candidates[i], master);
    // ホバーは、日中（テスト）でも日本語で出す。
    let conclusion_ja = i18n::as_ja(|| ctext::conclusion(&set.candidates[i], master));
    let parent_hint = ctext::parent_hint(&set.candidates[i]);
    let source = ctext::source_file(&set.candidates[i], inputs);
    let source_hover = ctext::source_hover(&set.candidates[i], inputs);
    let row_h = table_row_height(ui, compact);
    let line1_h = table_line1_height(ui, compact);
    let total = ui.available_width();
    let cols = TableCols::new(total);
    let amber = egui::Color32::from_rgb(230, 160, 40);
    let c = &mut set.candidates[i];
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, row_h), egui::Sense::hover());
    // 縞（1行おき）。
    if row_no % 2 == 1 {
        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    }
    let at = |x0: f32, x1: f32, y0: f32, y1: f32| {
        egui::Rect::from_min_max(egui::pos2(rect.left() + x0, rect.top() + y0), egui::pos2(rect.left() + x1, rect.top() + y1))
    };
    let left = egui::Layout::left_to_right(egui::Align::Center);

    // 採否は、この1つのトグルだけ（「まとめる」は「⋯」から）。
    in_cell(ui, at(CELL_PAD, TOGGLE_COL_W - CELL_PAD, 0.0, row_h), left, |ui| {
        let adopted = c.is_adopted();
        let label = egui::RichText::new(ctext::toggle_label(adopted));
        let toggle = if adopted {
            egui::Button::new(label.strong().color(ui.visuals().selection.stroke.color)).fill(ui.visuals().selection.bg_fill)
        } else {
            egui::Button::new(label.weak())
        };
        if ui.add(toggle.min_size(egui::vec2(TOGGLE_COL_W - CELL_PAD * 2.0, 0.0))).on_hover_text(i18n::as_ja(|| ctext::toggle_hint())).clicked() {
            if adopted {
                c.unadopt();
            } else {
                c.adopt();
            }
            // 要確認の行は、採否を決めたら確認済みにする。
            if reason.is_some() {
                confirmed.insert(id.clone());
            }
        }
    });

    // 名前（まとめた行は先頭の別名）。新規に登録するものは太字。同一の別名・対の表記は、後ろに足す。
    let mut name_full = c.aliases.first().cloned().unwrap_or_else(|| c.name.clone());
    if !c.aliases.is_empty() {
        name_full.push_str(&format!("（{}: {}）", ctext::merged_label(), c.aliases.join(" ＝ ")));
    }
    if !c.alt_forms.is_empty() {
        name_full.push_str(&format!("（{}）", c.alt_forms.join(" / ")));
    }
    let name_is_new = matches!(c.choice, Choice::New);
    let name_cell = at(cols.x_name() + CELL_PAD, cols.x_state() - CELL_PAD, 0.0, if compact { line1_h } else { row_h });
    if compact {
        // コンパクト: 「タグ候補名 ｜ 元のファイル名」を1行に。長いときは、後ろのファイル名から切り詰める。
        in_cell(ui, name_cell, left, |ui| {
            let mut job = egui::text::LayoutJob::default();
            append_count(&mut job, ui, c.files.len());
            let name_color = if name_is_new { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            job.append(&name_full, 0.0, egui::TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color: name_color, ..Default::default() });
            if let Some(src) = &source {
                job.append(ctext::SOURCE_SEPARATOR, 0.0, egui::TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color: ui.visuals().weak_text_color(), ..Default::default() });
                append_source(&mut job, ui, src);
            }
            let resp = ui.add(egui::Label::new(job).truncate().show_tooltip_when_elided(false));
            show_name_hover(resp, &source_hover);
        });
    } else {
        // 標準: 1段目に元のファイル名（候補の元になった部分を強調）、2段目にタグ候補名。
        in_cell(ui, at(cols.x_name() + CELL_PAD, cols.x_state() - CELL_PAD, 0.0, line1_h), left, |ui| {
            let mut job = egui::text::LayoutJob::default();
            append_count(&mut job, ui, c.files.len());
            if let Some(src) = &source {
                append_source(&mut job, ui, src);
            }
            let resp = ui.add(egui::Label::new(job).truncate().show_tooltip_when_elided(false));
            show_name_hover(resp, &source_hover);
        });
        in_cell(ui, at(name_cell.left() - rect.left(), name_cell.right() - rect.left(), line1_h, row_h), left, |ui| {
            let label = egui::RichText::new(&name_full);
            let label = if name_is_new { label.strong() } else { label };
            ui.add(egui::Label::new(label).truncate().show_tooltip_when_elided(false)).on_hover_text(&name_full);
        });
    }


    // 要確認の印と理由ボタン（・コンパクトでは結論文）。
    in_cell(ui, at(cols.x_state() + CELL_PAD, cols.x_ops() - CELL_PAD, 0.0, line1_h), left, |ui| {
        if let Some(kind) = &reason {
            if is_confirmed {
                ui.label(egui::RichText::new(ctext::confirmed_label()).color(ui.visuals().selection.stroke.color));
            } else {
                let badge = ctext::review_badge(kind);
                // 理由ボタンを先に確保して、印の文字は残りの幅で切り詰める。
                let why_w = 72.0;
                let badge_w = (ui.available_width() - why_w).max(40.0);
                // 高さは文字に任せ、幅だけを制限する（高さ0の枠に置くと、文字が上下に切れる）。
                ui.scope(|ui| {
                    ui.set_max_width(badge_w);
                    ui.add(egui::Label::new(egui::RichText::new(&badge).color(amber)).truncate().show_tooltip_when_elided(false)).on_hover_text(i18n::as_ja(|| ctext::review_badge(kind)));
                });
                if ui.small_button(ctext::why_button_label()).clicked() {
                    *action = Some(RowAction::OpenWhy(i));
                }
            }
        }
        if let Some(hint) = &parent_hint {
            ui.weak(hint).on_hover_text(i18n::as_ja(|| ctext::parent_hint_hover()));
        }
        if compact {
            ui.add(egui::Label::new(conclusion.as_str()).truncate().show_tooltip_when_elided(false)).on_hover_text(&conclusion_ja);
        }
    });

    // 「変更…／解決する…」と「⋯」。
    in_cell(ui, at(cols.x_ops() + CELL_PAD, total - CELL_PAD, 0.0, line1_h), left, |ui| {
        let unresolved = reason.is_some() && !is_confirmed;
        let label = if unresolved { ctext::resolve_button_label() } else { ctext::change_button_label() };
        let button = if unresolved { egui::Button::new(label).small().fill(ui.visuals().selection.bg_fill) } else { egui::Button::new(label).small() };
        if ui.add(button).clicked() {
            *action = Some(RowAction::OpenResolve(i));
        }
        ui.menu_button("⋯", |ui| {
            if ui.button(ctext::merge_label()).clicked() {
                *action = Some(RowAction::OpenMerge(i));
                ui.close();
            }
            if ctext::can_move_category(c) && ui.button(ctext::bulk_move_label()).clicked() {
                *action = Some(RowAction::OpenBulkMove(i));
                ui.close();
            }
            if !c.aliases.is_empty() {
                ui.separator();
                ui.weak(i18n::render(|t| t.tag_confirm_primary_alias().to_string()));
                for (pos, a) in c.aliases.iter().enumerate() {
                    if ui.radio(pos == 0, a).clicked() {
                        *action = Some(RowAction::SetPrimary(i, pos));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(ctext::split_label()).clicked() {
                    *action = Some(RowAction::Split(i));
                    ui.close();
                }
            }
        });
    });

    // 2段目（標準のみ）: 何をどこにどうするかの1文を、「内容・要確認」の列の中に出す（列をまたがない）。
    if !compact {
        in_cell(ui, at(cols.x_state() + CELL_PAD, cols.x_ops() - CELL_PAD, line1_h, row_h), left, |ui| {
            ui.add(egui::Label::new(conclusion.as_str()).truncate().show_tooltip_when_elided(false)).on_hover_text(&conclusion_ja);
        });
    }

    // 罫線: 列の境目（行の全高）と、行の下の全幅の区切り。
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    for x in [cols.x_name(), cols.x_state(), cols.x_ops()] {
        ui.painter().vline(rect.left() + x, rect.y_range(), stroke);
    }
    ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
}

/// 適用中のジョブ。
pub(super) struct TagApplyJob {
    rx: mpsc::Receiver<Result<ApplyReport, ApplyError>>,
    /// 成功したときに画面側へ取り込む、新しいマスター。
    master: Vec<TagManagerCategoryUi>,
    next_tier_id: u64,
}

impl NekoviewApp {
    /// 採用された候補を適用する。別スレッドで、バックアップ → マスター保存 → タグ書き込みを行う。
    fn start_tag_apply(&mut self, st: &TagConfirm) {
        let Some(db) = self.spread_db.clone() else { return };
        // 試用中の再適用は禁止（先に確定か破棄をしてもらう）。ボタン側でも押せなくしている。
        if self.tag_trial_active {
            self.set_toast(i18n::t().tag_scan_trial_hint());
            return;
        }
        let mut master = self.tag_manager_categories.clone();
        let mut next = self.tag_manager_next_tier_id;
        let plan = plan_application(&st.set, &mut master, &mut next, i18n::t());
        if plan.file_additions.is_empty() {
            self.set_toast(i18n::t().tag_confirm_apply_none());
            return;
        }
        let paths = st.paths.clone();
        let detach = st.reattach;
        let root = self.config.config_root.clone();
        let keep = self.config.backup_generations as usize;
        let (worker_master, worker_next) = (master.clone(), next);
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let backup = || match crate::db_backup::create_generation(&db, &root, keep) {
                Ok(g) => {
                    crate::log_common!("[tag-apply] 適用前のバックアップを作成: {}", g.name);
                    Some(g.name)
                }
                Err(e) => {
                    crate::log_common!("[tag-apply] 適用前のバックアップに失敗（適用を中止）: {:?}", e);
                    None
                }
            };
            // 適用は仮マスターに書く。本物のマスターは、確定するまで変えない。
            let save_master =
                || crate::tag_manager::save_slot(&root, crate::tag_manager::Slot::Trial, &worker_master, worker_next);
            let save_record = |r: &crate::tag_trial::TrialRecord| r.save(&root);
            let out = run_apply(&db, &paths, &plan, &worker_master, detach, &backup, &save_master, &save_record);
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.tag_apply_job = Some(TagApplyJob { rx, master, next_tier_id: next });
    }

    /// 適用の完了を取り込む（毎フレーム呼んでよい）。
    pub(super) fn poll_tag_apply(&mut self) {
        let Some(job) = self.tag_apply_job.as_ref() else { return };
        let out = match job.rx.try_recv() {
            Ok(o) => o,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.tag_apply_job = None;
                return;
            }
        };
        let job = self.tag_apply_job.take().expect("job just polled");
        match out {
            Ok(report) => {
                // 新しいマスターを取り込み、タグの選択肢と表示を作り直す。
                // 以降は試用中。仮マスターを使い、保存も仮マスターへ行く。
                self.tag_trial_active = true;
                self.tag_trial_notified = true;
                self.tag_manager_categories = job.master;
                self.tag_manager_next_tier_id = job.next_tier_id;
                self.save_tag_manager();
                // 選択中のファイルのタグ表示を読み直させる。
                self.tag_binding_path = None;
                self.set_toast(i18n::render(|t| t.tag_apply_done(&report).to_string()));
            }
            Err(ApplyError::BackupFailed) => self.set_toast(i18n::render(|t| t.tag_apply_backup_failed().to_string())),
            Err(ApplyError::RecordFailed) => self.set_toast(i18n::render(|t| t.tag_apply_record_failed().to_string())),
        }
    }

    /// 適用中の進捗オーバーレイ（背後のマウス入力を遮断する）。
    pub(super) fn draw_tag_apply_overlay(&mut self, ctx: &egui::Context) {
        if self.tag_apply_job.is_none() {
            return;
        }
        self.explorer_modal(ctx, "tag_apply_progress").show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(24.0));
                ui.add_space(6.0);
                ui.label(i18n::t().tag_apply_progress());
            });
        });
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_candidates::build_candidates;
    use crate::tag_source::FormatTag;

    fn input(name: &str, info: Option<&str>) -> CandidateInput {
        crate::tag_candidates::input_from_info_text(name, Some(FormatTag::Extension("zip".into())), info)
    }

    #[test]
    fn rows_have_sections_cluster_headers_and_respect_the_unconfirmed_filter() {
        let inputs = [input("[Aaa (Bbbbbbb)] t.zip", Some("Category: Manga\n")), input("[Aaa (Bbbbbbc)] u.zip", None)];
        let set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::English);
        let w1 = rows_for(&set, Window::One, ListView::All, &HashSet::new(), "", &MatchOptions::default());
        assert_eq!(w1.first(), Some(&Row::Section(Role::Format)));
        assert!(w1.contains(&Row::Section(Role::Main)));
        assert!(!w1.iter().any(|r| matches!(r, Row::Section(Role::Author))));
        let w2 = rows_for(&set, Window::Two, ListView::All, &HashSet::new(), "", &MatchOptions::default());
        assert_eq!(w2.first(), Some(&Row::Section(Role::Author)), "種類順は作者→グループ");
        // Bbbbbbb と Bbbbbbc は類似で、同じ束の見出しが付く。
        assert!(w2.iter().any(|r| matches!(r, Row::Cluster { size: 2, .. })), "{w2:?}");
        let cand_rows = w2.iter().filter(|r| matches!(r, Row::Cand(_))).count();
        assert_eq!(cand_rows, 3, "Aaa / Bbbbbbb / Bbbbbbc");
        // 未確定だけ: すべて未確定（既存タグが無い）。何も無いマスターでは変わらない。
        assert_eq!(rows_for(&set, Window::Two, ListView::NeedsReview, &HashSet::new(), "", &MatchOptions::default()).len(), w2.len());
        // 窓1の候補は未確定ではないので、フィルタで消える。
        assert!(rows_for(&set, Window::One, ListView::NeedsReview, &HashSet::new(), "", &MatchOptions::default()).is_empty());
    }

    #[test]
    fn views_filter_by_review_and_adoption_and_counts_match() {
        let inputs = [input("[Aaa (Bbbbbbb)] t.zip", Some("Category: Manga\n")), input("[Aaa (Bbbbbbc)] u.zip", None)];
        let mut set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::English);
        let opts = MatchOptions::default();
        let none = HashSet::new();
        let cands = |set: &CandidateSet, view: ListView, confirmed: &HashSet<(Role, String)>| -> usize {
            rows_for(set, Window::Two, view, confirmed, "", &opts).iter().filter(|r| matches!(r, Row::Cand(_))).count()
        };
        let counts = view_counts(&set, Window::Two, &none);
        assert_eq!(counts.all, 3);
        assert_eq!(counts.planned + counts.rejected, counts.all);
        assert_eq!(cands(&set, ListView::All, &none), counts.all);
        assert_eq!(cands(&set, ListView::Planned, &none), counts.planned);
        assert_eq!(cands(&set, ListView::Rejected, &none), counts.rejected);
        // 何も無いマスターでは、作者は未確定（要確認）。
        assert_eq!(counts.needs_review, 3);
        assert_eq!(cands(&set, ListView::NeedsReview, &none), 3);
        // 確認済みにした分は、要確認の件数と表示から外れる。
        let mut confirmed = HashSet::new();
        let first = set.indices_in_window(Window::Two)[0];
        confirmed.insert((set.candidates[first].role, set.candidates[first].key.clone()));
        assert_eq!(view_counts(&set, Window::Two, &confirmed).needs_review, 2);
        assert_eq!(cands(&set, ListView::NeedsReview, &confirmed), 2);
        // 採否を変えると、採用予定・不採用の件数が変わる。
        set.candidates[first].unadopt();
        assert!(view_counts(&set, Window::Two, &none).rejected >= 1);
        assert!(cands(&set, ListView::Rejected, &none) >= 1);
    }

    #[test]
    fn query_filters_by_name_alias_and_form_with_normalization() {
        let inputs = [input("[Aaa (Kouji)] t.zip", None), input("[Aaa (Zzz)] u.zip", None)];
        let cats: Vec<TagManagerCategoryUi> = Vec::new();
        let mut set = build_candidates(&inputs, &cats, &MatchOptions::default(), Lang::English);
        let opts = MatchOptions::default();
        let names = |set: &CandidateSet, q: &str| -> Vec<String> {
            rows_for(set, Window::Two, ListView::All, &HashSet::new(), q, &opts)
                .into_iter()
                .filter_map(|r| if let Row::Cand(i) = r { Some(set.candidates[i].name.clone()) } else { None })
                .collect()
        };
        assert_eq!(names(&set, "kou"), ["Kouji"], "部分一致・大文字小文字を区別しない");
        assert_eq!(names(&set, "ＫＯＵ"), ["Kouji"], "全角半角を吸収");
        assert_eq!(names(&set, "  ").len(), 3, "空白だけは絞り込まない");
        // まとめた候補は、別名のどれでも見つかる。
        let (a, b) = (
            set.candidates.iter().position(|c| c.name == "Kouji").unwrap(),
            set.candidates.iter().position(|c| c.name == "Zzz").unwrap(),
        );
        set.merge_candidates(&[a, b], None, 1, &cats).unwrap();
        assert_eq!(names(&set, "zz"), ["Kouji|Zzz"]);
        assert_eq!(names(&set, "kou"), ["Kouji|Zzz"]);
        assert!(names(&set, "nomatch").is_empty());
    }

    #[test]
    fn confirm_starts_at_the_first_window_that_has_candidates() {
        let only_authors = build_candidates(&[crate::tag_candidates::input_from_info_text("[A (B)] t.zip", None, None)], &[], &MatchOptions::default(), Lang::English);
        let c = TagConfirm::new(only_authors, vec![], vec![], vec![], None, Arc::new(Vec::new()), 5000, Arc::new(Vec::new()), Arc::new(Vec::new())).unwrap();
        assert_eq!(c.stage, Window::Two, "窓1が空なら2枚目から");
        let empty = build_candidates(&[], &[], &MatchOptions::default(), Lang::English);
        assert!(TagConfirm::new(empty, vec![], vec![], vec![], None, Arc::new(Vec::new()), 5000, Arc::new(Vec::new()), Arc::new(Vec::new())).is_none());
    }

    #[test]
    fn adopted_count_counts_per_window() {
        let inputs = [input("[Aaa (Bbb)] t.zip", None)];
        let mut set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::English);
        assert_eq!(adopted_count(&set, Window::Two), 2);
        for c in set.candidates.iter_mut().filter(|c| c.role.window() == Window::Two) {
            c.unadopt();
        }
        assert_eq!(adopted_count(&set, Window::Two), 0);
        assert_eq!(adopted_count(&set, Window::One), 1, "形式 zip");
    }
}
