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
        Some(Self { set, inputs: Arc::new(inputs), paths, master: Arc::new(master), model, rules, stage, move_rules, separators, sep_input: String::new(), sep_error: None, view: ListView::All, settings_open: false, rules_dialog_open: false, rebuild: None, threshold_draft: None, merge_dialog: None, bulk_move_dialog: None, filter: String::new(), next_bundle_id: 1, rename_review: None, limit, limit_keep: false, truncated: Vec::new(), reattach: false, compact: false, confirmed: HashSet::new(), resolve_dialog: None, why_dialog: None })
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
        self.resolve_dialog = Some(ResolveDialog { role: c.role, key: c.key.clone(), sel: c.choice.clone(), move_cat, remember: c.category_rule, parent_mode: c.parent_mode });
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
        self.bulk_move_dialog = Some(BulkMoveDialog { role: c.role, selected, filter: String::new(), target: None, remember: false });
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
    ctext::band_title(role_label(role), role == Role::Main)
}

fn role_label(role: Role) -> &'static str {
    let t = i18n::t();
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
        let mut cancel = false;
        let mut apply_clicked = false;
        let mut row_action: Option<RowAction> = None;
        let mut opts = self.config.tag_match;
        let before = opts;
        let mut default_review = self.config.tag_confirm_default_review;

        let center = self.explorer_center(ctx);
        egui::Window::new(match stage {
            Window::One => t.tag_confirm_title_one(),
            Window::Two => t.tag_confirm_title_two(),
        })
        .id(egui::Id::new("tag_confirm_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([1000.0, 600.0])
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(center)
        .show(ctx, |ui| {
            // 見出し帯: 表示の絞り込み（チップ）・要確認を既定にするチェック・名前の絞り込み・表示密度・照合の設定・一括操作。
            let counts = view_counts(&st.set, stage, &st.confirmed);
            let window_idx: Vec<usize> = st.set.indices_in_window(stage);
            ui.horizontal_wrapped(|ui| {
                for (v, label, n) in [
                    (ListView::All, ctext::VIEW_ALL, counts.all),
                    (ListView::NeedsReview, ctext::VIEW_NEEDS_REVIEW, counts.needs_review),
                    (ListView::Planned, ctext::ADOPT_LABEL, counts.planned),
                    (ListView::Rejected, ctext::REJECT_LABEL, counts.rejected),
                ] {
                    if ui.selectable_label(st.view == v, format!("{label} {n}")).clicked() {
                        st.view = v;
                    }
                }
                ui.checkbox(&mut default_review, ctext::DEFAULT_REVIEW_LABEL).on_hover_text(ctext::DEFAULT_REVIEW_HINT);
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(t.tag_confirm_filter_label());
                ui.add(egui::TextEdit::singleline(&mut st.filter).desired_width(180.0).hint_text(t.tag_confirm_filter_hint()));
                if !st.filter.is_empty() && ui.small_button("✕").clicked() {
                    st.filter.clear();
                }
                ui.separator();
                let density = if st.compact { ctext::DENSITY_COMPACT } else { ctext::DENSITY_STANDARD };
                if ui.small_button(density).on_hover_text(ctext::DENSITY_HINT).clicked() {
                    st.compact = !st.compact;
                }
                if ui.small_button(ctext::SETTINGS_BUTTON).on_hover_text(ctext::SETTINGS_HINT).clicked() {
                    st.settings_open = true;
                }
                ui.menu_button(ctext::BULK_LABEL, |ui| {
                    if ui.button(ctext::BULK_ADOPT_ALL).clicked() {
                        for &i in &window_idx {
                            st.set.candidates[i].adopt();
                            confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                        }
                        ui.close();
                    }
                    if ui.button(ctext::BULK_REJECT_ALL).clicked() {
                        for &i in &window_idx {
                            st.set.candidates[i].unadopt();
                            confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                        }
                        ui.close();
                    }
                    if ui.button(ctext::BULK_REJECT_UNCONFIRMED).clicked() {
                        for &i in &window_idx {
                            if st.set.candidates[i].unconfirmed {
                                st.set.candidates[i].unadopt();
                                confirm_if_flagged(&mut st.confirmed, &st.set.candidates[i]);
                            }
                        }
                        ui.close();
                    }
                });
                if st.rebuild.is_some() {
                    ui.add(egui::Spinner::new());
                }
            });
            ui.separator();

            // 一覧（表）。列見出しは固定で、行は固定の高さで仮想スクロールする。
            let rows = rows_for(&st.set, stage, st.view, &st.confirmed, &st.filter, &opts);
            let compact = st.compact;
            let row_h = table_row_height(ui, compact);
            let footer_h = 72.0;
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
                                    if ui.small_button(ctext::CLUSTER_ADOPT_LABEL).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.adopt();
                                        }
                                    }
                                    if ui.small_button(ctext::CLUSTER_REJECT_LABEL).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.unadopt();
                                        }
                                    }
                                    if ui.small_button(ctext::MERGE_LABEL).clicked() {
                                        row_action = Some(RowAction::MergeCluster(id));
                                    }
                                }),
                                Row::Cand(i) => draw_candidate_row(ui, &mut st.set, i, &st.master, &mut row_action, &mut st.confirmed, t, first + k, compact),
                            }
                        }
                        if rows.is_empty() {
                            ui.weak(if st.view == ListView::NeedsReview && st.filter.is_empty() { ctext::NEEDS_REVIEW_EMPTY } else { t.tag_confirm_empty() });
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
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_confirm_truncated(&rows.join(" / ")));
                }
            });

            // 操作ボタン。適用は、最後の窓（作者・グループの窓。無ければ1枚目）で押せる。
            ui.separator();
            let any_adopted = st.set.candidates.iter().any(|c| c.is_adopted());
            // 上限を超えたまま（拡張も維持も選んでいない）の間は適用できない。
            let blocked = !st.limit_keep && !over_limit(&st.set.candidates, st.limit).is_empty();
            let apply_enabled = any_adopted && !blocked && self.spread_db.is_some() && self.tag_apply_job.is_none();
            let apply_hint = if blocked {
                t.tag_confirm_limit_pending().to_string()
            } else if !any_adopted {
                t.tag_confirm_apply_none().to_string()
            } else {
                t.tag_confirm_apply_pending().to_string()
            };
            ui.horizontal(|ui| {
                let has_two = st.set.candidates.iter().any(|c| c.role.window() == Window::Two);
                // 適用のできる窓では、前回の自動付与分を外して付け直すかを選べる（既定はOFF）。
                if stage == Window::Two || !has_two {
                    ui.checkbox(&mut st.reattach, t.tag_confirm_reattach()).on_hover_text(t.tag_confirm_reattach_hover());
                }
                if stage == Window::Two {
                    let back_enabled = st.set.candidates.iter().any(|c| c.role.window() == Window::One);
                    if ui.add_enabled(back_enabled, egui::Button::new(t.tag_confirm_back())).clicked() {
                        next_stage = Some(Window::One);
                    }
                } else if ui.add_enabled(has_two, egui::Button::new(t.tag_confirm_next())).clicked() {
                    next_stage = Some(Window::Two);
                }
                if stage == Window::Two || !has_two {
                    let btn = ui.add_enabled(apply_enabled, egui::Button::new(t.tag_confirm_apply()));
                    let btn = if apply_enabled { btn } else { btn.on_disabled_hover_text(apply_hint) };
                    if btn.clicked() {
                        apply_clicked = true;
                    }
                }
                if ui.button(t.archive_open_cancel()).clicked() {
                    cancel = true;
                }
            });
        });

        // 「要確認を既定にする」チェックの変更を保存する。
        if default_review != self.config.tag_confirm_default_review {
            self.config.tag_confirm_default_review = default_review;
            self.persist_state();
        }
        // 照合の設定（別ポップアップ）。切り替えたら、下の `opts != before` で候補を作り直す。
        if open && !cancel && st.settings_open {
            let mut close = false;
            let mut delete_rule: Option<usize> = None;
            let mut sep_action: Option<SepAction> = None;
            let modal = self.explorer_modal(ctx, "tag_match_settings").show(ctx, |ui| {
                ui.set_width(520.0);
                ui.heading(ctext::SETTINGS_TITLE);
                ui.weak(ctext::SETTINGS_NOTE);
                ui.add_space(6.0);
                ui.checkbox(&mut opts.width, t.tag_confirm_opt_width());
                ui.checkbox(&mut opts.space, t.tag_confirm_opt_space());
                ui.checkbox(&mut opts.hyphen_underscore, t.tag_confirm_opt_hyphen());
                ui.checkbox(&mut opts.symbols, t.tag_confirm_opt_symbols());
                ui.checkbox(&mut opts.romaji, t.tag_confirm_opt_romaji());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(t.tag_confirm_threshold());
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
                ui.weak(ctext::THRESHOLD_NOTE);
                // 保存したカテゴリ振替ルールの一覧と削除。
                ui.add_space(8.0);
                ui.separator();
                ui.strong(ctext::RULES_SECTION_TITLE);
                ui.weak(ctext::RULES_SECTION_NOTE);
                if self.candidate_rules.is_read_only() {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), ctext::RULES_READ_ONLY);
                }
                if self.candidate_rules.moves.is_empty() {
                    ui.weak(ctext::RULES_EMPTY);
                }
                egui::ScrollArea::vertical().id_salt("tag_move_rules").max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, r) in self.candidate_rules.moves.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(ctext::rule_line(rule_role_name(r.role), &r.name, &r.category));
                            if ui.small_button(ctext::RULES_DELETE_LABEL).clicked() {
                                delete_rule = Some(i);
                            }
                        });
                    }
                });
                // 上位名の区切り記号。
                ui.add_space(8.0);
                ui.separator();
                ui.strong(ctext::SEPARATORS_TITLE);
                ui.weak(ctext::SEPARATORS_NOTE);
                let seps = self.candidate_rules.effective_separators();
                if seps.is_empty() {
                    ui.weak(ctext::SEPARATORS_EMPTY);
                }
                ui.horizontal_wrapped(|ui| {
                    for (i, sep) in seps.iter().enumerate() {
                        ui.label(egui::RichText::new(sep).strong());
                        if ui.small_button("×").on_hover_text(ctext::RULES_DELETE_LABEL).clicked() {
                            sep_action = Some(SepAction::Remove(i));
                        }
                        ui.add_space(6.0);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut st.sep_input).desired_width(80.0).hint_text(ctext::SEPARATORS_ADD_HINT));
                    if ui.button(ctext::SEPARATORS_ADD_LABEL).clicked() {
                        sep_action = Some(SepAction::Add);
                    }
                    if !self.candidate_rules.separators_are_default() && ui.button(ctext::SEPARATORS_RESET_LABEL).clicked() {
                        sep_action = Some(SepAction::Reset);
                    }
                });
                if let Some(err) = &st.sep_error {
                    ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
                }
                ui.add_space(8.0);
                if ui.button(ctext::CLOSE_LABEL).clicked() {
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
        if apply_clicked {
            // 既存要素の名前が書き換わるなら、先に一覧を見せて確認する。
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
        } else if open && !cancel {
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
                    ui.heading(t.tag_rename_review_title());
                    ui.label(t.tag_rename_review_intro(review.renames.len()));
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (old, new) in &review.renames {
                            ui.label(format!("{old}  →  {new}"));
                        }
                    });
                    if review.conflicts > 0 {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_rename_review_conflicts(review.conflicts));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(t.tag_rename_review_apply()).clicked() {
                            proceed = true;
                        }
                        if ui.button(t.tag_confirm_back()).clicked() {
                            back = true;
                        }
                    });
                });
                if back || modal.should_close() {
                    st.rename_review = None;
                }
            }
            self.draw_tag_merge_dialog(ctx, &mut st);
            self.draw_tag_bulk_move_dialog(ctx, &mut st);
            self.draw_tag_resolve_dialog(ctx, &mut st);
            self.draw_tag_why_dialog(ctx, &mut st);
            self.draw_tag_rule_hits_dialog(ctx, &mut st);
            if proceed {
                self.start_tag_apply(&st);
            } else {
                self.tag_confirm = Some(st);
            }
        }
    }
}

impl NekoviewApp {
    /// 上限超過のダイアログ。「拡張する」は上限を増やす（まだ超えていれば次のフレームでまた出る）。
    /// 「上限を維持する」は、該当ファイル数の多い要素から上限まで残して、あとを切り捨てる。
    fn draw_tag_limit_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm, over: &[(Role, usize)]) {
        let t = i18n::t();
        let rows: Vec<String> = over.iter().map(|&(r, n)| t.tag_limit_row(role_label(r), n, n - st.limit)).collect();
        let db_size = std::fs::metadata(crate::dev_db_backup::db_path(&self.config.config_root))
            .map(|m| super::db_backup_ui::format_size(m.len()))
            .unwrap_or_else(|_| "-".to_owned());
        let (mut expand, mut keep) = (false, false);
        self.explorer_modal(ctx, "tag_limit_dialog").show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(t.tag_limit_title());
            ui.add_space(4.0);
            ui.label(t.tag_limit_body(st.limit, &rows.join("\n"), &db_size, LIMIT_EXPAND_STEP));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t.tag_limit_expand(LIMIT_EXPAND_STEP)).clicked() {
                    expand = true;
                }
                if ui.button(t.tag_limit_keep()).clicked() {
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
            ui.heading(ctext::MERGE_TITLE);
            ui.add_space(4.0);
            ui.label(ctext::merge_intro(&md.name));
            ui.add_space(6.0);
            ui.weak(i18n::t().tag_confirm_filter_label());
            ui.add(egui::TextEdit::singleline(&mut md.filter).desired_width(240.0).hint_text(t.tag_confirm_filter_hint()));
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
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), "まとめる前に知っておくこと");
            for line in ctext::MERGE_CONSEQUENCES {
                ui.label(format!("・{line}"));
            }
            if let Some(msg) = &md.msg {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(220, 60, 60), msg);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t.archive_open_cancel()).clicked() {
                    cancel = true;
                }
                if ui.add_enabled(chosen.len() >= 2, egui::Button::new(ctext::MERGE_OK_LABEL)).clicked() {
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
        let t = i18n::t();
        let opts = self.config.tag_match;
        let role = bd.role;
        let (mut ok, mut cancel) = (false, false);
        // 移し先にできるカテゴリ（同じ種類は、抽出元のカテゴリも同じ）。
        let targets: Vec<(usize, String)> = st.set.candidates.iter().find(|c| c.role == role).map(|c| ctext::move_targets(c, &st.master)).unwrap_or_default();
        let modal = self.explorer_modal(ctx, "tag_bulk_move_dialog").show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(ctext::BULK_MOVE_TITLE);
            ui.add_space(4.0);
            ui.label(ctext::bulk_move_intro(role.new_category_name(Lang::Japanese)));
            ui.add_space(6.0);
            ui.weak(t.tag_confirm_filter_label());
            ui.add(egui::TextEdit::singleline(&mut bd.filter).desired_width(240.0).hint_text(t.tag_confirm_filter_hint()));
            let idx: Vec<usize> = (0..st.set.candidates.len())
                .filter(|&i| {
                    let c = &st.set.candidates[i];
                    c.role == role && (bd.selected.contains(&c.key) || matches_query(c, &bd.filter, &opts))
                })
                .collect();
            // 表示中の候補を、まとめて選ぶ・外す。
            ui.horizontal(|ui| {
                if ui.small_button("表示中をすべて選ぶ").clicked() {
                    for &i in &idx {
                        bd.selected.insert(st.set.candidates[i].key.clone());
                    }
                }
                if ui.small_button("すべて外す").clicked() {
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
            if targets.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(200, 140, 0), ctext::MOVE_NO_TARGET);
            } else {
                ui.horizontal(|ui| {
                    ui.label(ctext::MOVE_PICK_LABEL);
                    let current = bd.target.and_then(|i| targets.iter().find(|t| t.0 == i)).map_or_else(|| ctext::MOVE_PICK_PLACEHOLDER.to_string(), |t| t.1.clone());
                    egui::ComboBox::from_id_salt("tag_bulk_move_cat").selected_text(current).show_ui(ui, |ui| {
                        for (i, name) in &targets {
                            if ui.selectable_label(bd.target == Some(*i), name).clicked() {
                                bd.target = Some(*i);
                            }
                        }
                    });
                });
                ui.checkbox(&mut bd.remember, ctext::REMEMBER_LABEL).on_hover_text(ctext::REMEMBER_HINT);
            }
            let chosen: Vec<&Candidate> = st.set.candidates.iter().filter(|c| c.role == role && bd.selected.contains(&c.key)).collect();
            let files: HashSet<usize> = chosen.iter().flat_map(|c| c.files.iter().copied()).collect();
            let cat_name = bd.target.and_then(|i| targets.iter().find(|t| t.0 == i)).map(|t| t.1.as_str());
            ui.add_space(4.0);
            ui.label(ctext::bulk_move_preview(chosen.len(), files.len(), cat_name));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t.archive_open_cancel()).clicked() {
                    cancel = true;
                }
                if ui.add_enabled(!chosen.is_empty() && bd.target.is_some(), egui::Button::new(ctext::BULK_MOVE_OK_LABEL)).clicked() {
                    ok = true;
                }
            });
        });
        if cancel || modal.should_close() {
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
        let t = i18n::t();
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
            ui.weak(ctext::RESOLVE_SUBTITLE);
            if let Some(n) = note {
                ui.add_space(4.0);
                ui.colored_label(amber, n);
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("tag_resolve_options").max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                for o in &options {
                    let selected = rd.move_cat.is_none() && rd.sel == o.choice;
                    let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                    let framed = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.radio(selected, "").clicked() {
                                rd.sel = o.choice.clone();
                                rd.move_cat = None;
                            }
                            ui.label(egui::RichText::new(&o.title).strong());
                        });
                        ui.label(egui::RichText::new(&o.desc).weak());
                    });
                    if framed.response.interact(egui::Sense::click()).clicked() {
                        rd.sel = o.choice.clone();
                        rd.move_cat = None;
                    }
                }
                // 登録先のカテゴリを選び直す（抽出された種類が実際と違うとき）。
                if movable {
                    let selected = rd.move_cat.is_some();
                    let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.4) } else { egui::Color32::TRANSPARENT };
                    let framed = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!targets.is_empty(), egui::RadioButton::new(selected, "")).clicked() {
                                rd.move_cat = rd.move_cat.or(targets.first().map(|t| t.0));
                                rd.sel = Choice::New;
                            }
                            ui.label(egui::RichText::new(ctext::MOVE_TITLE).strong());
                        });
                        ui.label(egui::RichText::new(&move_desc).weak());
                        if targets.is_empty() {
                            ui.colored_label(amber, ctext::MOVE_NO_TARGET);
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(ctext::MOVE_PICK_LABEL);
                                let current = rd
                                    .move_cat
                                    .and_then(|i| targets.iter().find(|t| t.0 == i))
                                    .map_or_else(|| ctext::MOVE_PICK_PLACEHOLDER.to_string(), |t| t.1.clone());
                                egui::ComboBox::from_id_salt("tag_resolve_move_cat").selected_text(current).show_ui(ui, |ui| {
                                    for (i, name) in &targets {
                                        if ui.selectable_label(rd.move_cat == Some(*i), name).clicked() {
                                            rd.move_cat = Some(*i);
                                            rd.sel = Choice::New;
                                        }
                                    }
                                });
                            });
                            if let Some(preview) = rd.move_cat.and_then(|i| previews.iter().find(|p| p.0 == i)) {
                                ui.label(&preview.1);
                            }
                            // 次回以降も同じ扱いにする（ルールとして保存）。まとめた候補には出さない。
                            if can_remember && rd.move_cat.is_some() {
                                ui.checkbox(&mut rd.remember, ctext::REMEMBER_LABEL).on_hover_text(ctext::REMEMBER_HINT);
                            }
                            if rule_exists {
                                ui.weak(ctext::RULE_EXISTS_NOTE);
                            }
                        }
                    });
                    if !targets.is_empty() && framed.response.interact(egui::Sense::click()).clicked() {
                        rd.move_cat = rd.move_cat.or(targets.first().map(|t| t.0));
                        rd.sel = Choice::New;
                    }
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
                if ui.button(t.archive_open_cancel()).clicked() {
                    cancel = true;
                }
                if ui.button(ctext::RESOLVE_OK_LABEL).clicked() {
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
                match rd.move_cat {
                    Some(cat) => {
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
                    None => {
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
                if ui.button(ctext::CLOSE_LABEL).clicked() {
                    close = true;
                }
                if ui.button(ctext::RESOLVE_BUTTON_LABEL).clicked() {
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
    /// 「ルールが働いた候補」ポップアップ。成立したルールの名前とファイル数を並べる。
    fn draw_tag_rule_hits_dialog(&mut self, ctx: &egui::Context, st: &mut TagConfirm) {
        if !st.rules_dialog_open {
            return;
        }
        let hits: Vec<String> = st.set.rule_hits.iter().map(|(name, n)| ctext::rule_hit_line(name, *n)).collect();
        let mut close = false;
        let modal = self.explorer_modal(ctx, "tag_rule_hits_dialog").show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(ctext::RULE_HITS_TITLE);
            ui.add_space(4.0);
            for line in &hits {
                ui.label(format!("・{line}"));
            }
            ui.add_space(6.0);
            ui.weak(ctext::RULE_HITS_NOTE);
            ui.add_space(8.0);
            if ui.button(ctext::CLOSE_LABEL).clicked() {
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

fn merge_error_text(e: &MergeError, t: Lang) -> String {
    match e {
        MergeError::TooFew => t.tag_confirm_merge_err_too_few().to_string(),
        MergeError::MixedRoles => t.tag_confirm_merge_err_mixed().to_string(),
        MergeError::MultipleExisting { first, second } => t.tag_confirm_merge_err_multiple(first, second),
        MergeError::AliasOverlap { alias, element } => t.tag_confirm_merge_err_overlap(alias, element),
    }
}

// ── 一覧（表） ──────────────────────────────────────────────────────

/// 採否トグルの列の幅。
const TOGGLE_COL_W: f32 = 124.0;
/// ファイル数の列の幅。
const COUNT_COL_W: f32 = 84.0;
/// 「⋯」の列の幅。
const OPS_COL_W: f32 = 150.0;
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
        let rest = (total - TOGGLE_COL_W - COUNT_COL_W - OPS_COL_W - 120.0).max(100.0);
        Self { total, name: (total * 0.24).clamp(130.0, 280.0).min(rest) }
    }
    fn x_name(self) -> f32 {
        TOGGLE_COL_W
    }
    fn x_count(self) -> f32 {
        TOGGLE_COL_W + self.name
    }
    fn x_state(self) -> f32 {
        self.x_count() + COUNT_COL_W
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
fn in_cell(ui: &mut egui::Ui, r: egui::Rect, layout: egui::Layout, add: impl FnOnce(&mut egui::Ui)) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(layout), |ui| {
        ui.set_clip_rect(r.intersect(ui.clip_rect()));
        add(ui);
    });
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
    put(CELL_PAD, egui::Align2::LEFT_CENTER, ctext::COL_TOGGLE);
    put(cols.x_name() + CELL_PAD, egui::Align2::LEFT_CENTER, ctext::COL_NAME);
    put(cols.x_state() - CELL_PAD, egui::Align2::RIGHT_CENTER, ctext::COL_COUNT);
    put(cols.x_state() + CELL_PAD, egui::Align2::LEFT_CENTER, ctext::COL_STATE);
    for x in [cols.x_name(), cols.x_count(), cols.x_state(), cols.x_ops()] {
        painter.vline(rect.left() + x, rect.y_range(), stroke);
    }
    painter.hline(rect.x_range(), rect.top(), stroke);
    painter.hline(rect.x_range(), rect.bottom(), stroke);
}

/// 候補1行。列は、採否・名前・ファイル数・要確認（選択欄）・「⋯」の固定位置。
/// 標準は2段で、「内容・要確認」の列の1段目に要確認の印・選択欄、2段目に何をどこにどうするかの1文を出す。コンパクトは1段で、その列に両方を並べる。
/// 長い文字は折り返さず、切り詰めてホバーで全文を出す。
fn draw_candidate_row(
    ui: &mut egui::Ui,
    set: &mut CandidateSet,
    i: usize,
    master: &[TagManagerCategoryUi],
    action: &mut Option<RowAction>,
    confirmed: &mut HashSet<(Role, String)>,
    t: Lang,
    row_no: usize,
    compact: bool,
) {
    let reason = ctext::review_kind(&set.candidates[i]);
    let id = (set.candidates[i].role, set.candidates[i].key.clone());
    // 要確認のうち、ユーザーが確認済みのもの。
    let is_confirmed = reason.is_some() && confirmed.contains(&id);
    let conclusion = ctext::conclusion(&set.candidates[i], master);
    let parent_hint = ctext::parent_hint(&set.candidates[i]);
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
    let right = egui::Layout::right_to_left(egui::Align::Center);

    // 採否は、この1つのトグルだけ（「まとめる」は「⋯」から）。
    in_cell(ui, at(CELL_PAD, TOGGLE_COL_W - CELL_PAD, 0.0, row_h), left, |ui| {
        let adopted = c.is_adopted();
        let label = egui::RichText::new(ctext::toggle_label(adopted));
        let toggle = if adopted {
            egui::Button::new(label.strong().color(ui.visuals().selection.stroke.color)).fill(ui.visuals().selection.bg_fill)
        } else {
            egui::Button::new(label.weak())
        };
        if ui.add(toggle.min_size(egui::vec2(TOGGLE_COL_W - CELL_PAD * 2.0, 0.0))).on_hover_text(ctext::TOGGLE_HINT).clicked() {
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
        name_full.push_str(&format!("（{}: {}）", ctext::MERGED_LABEL, c.aliases.join(" ＝ ")));
    }
    if !c.alt_forms.is_empty() {
        name_full.push_str(&format!("（{}）", c.alt_forms.join(" / ")));
    }
    in_cell(ui, at(cols.x_name() + CELL_PAD, cols.x_count() - CELL_PAD, 0.0, line1_h), left, |ui| {
        let label = egui::RichText::new(&name_full);
        let label = if matches!(c.choice, Choice::New) { label.strong() } else { label };
        ui.add(egui::Label::new(label).truncate()).on_hover_text(&name_full);
    });

    // ファイル数（右寄せ）。
    in_cell(ui, at(cols.x_count(), cols.x_state() - CELL_PAD, 0.0, line1_h), right, |ui| {
        ui.weak(ctext::file_count(c.files.len()));
    });

    // 要確認の印と理由ボタン（・コンパクトでは結論文）。
    in_cell(ui, at(cols.x_state() + CELL_PAD, cols.x_ops() - CELL_PAD, 0.0, line1_h), left, |ui| {
        if let Some(kind) = &reason {
            if is_confirmed {
                ui.label(egui::RichText::new(ctext::CONFIRMED_LABEL).color(ui.visuals().selection.stroke.color));
            } else {
                let badge = ctext::review_badge(kind);
                // 理由ボタンを先に確保して、印の文字は残りの幅で切り詰める。
                let why_w = 72.0;
                let badge_w = (ui.available_width() - why_w).max(40.0);
                // 高さは文字に任せ、幅だけを制限する（高さ0の枠に置くと、文字が上下に切れる）。
                ui.scope(|ui| {
                    ui.set_max_width(badge_w);
                    ui.add(egui::Label::new(egui::RichText::new(&badge).color(amber)).truncate()).on_hover_text(&badge);
                });
                if ui.small_button(ctext::WHY_BUTTON_LABEL).clicked() {
                    *action = Some(RowAction::OpenWhy(i));
                }
            }
        }
        if let Some(hint) = &parent_hint {
            ui.weak(hint).on_hover_text("「変更…」で、上位名も登録するかを選べます");
        }
        if compact {
            ui.add(egui::Label::new(conclusion.as_str()).truncate()).on_hover_text(&conclusion);
        }
    });

    // 「変更…／解決する…」と「⋯」。
    in_cell(ui, at(cols.x_ops() + CELL_PAD, total - CELL_PAD, 0.0, line1_h), left, |ui| {
        let unresolved = reason.is_some() && !is_confirmed;
        let label = if unresolved { ctext::RESOLVE_BUTTON_LABEL } else { ctext::CHANGE_BUTTON_LABEL };
        let button = if unresolved { egui::Button::new(label).small().fill(ui.visuals().selection.bg_fill) } else { egui::Button::new(label).small() };
        if ui.add(button).clicked() {
            *action = Some(RowAction::OpenResolve(i));
        }
        ui.menu_button("⋯", |ui| {
            if ui.button(ctext::MERGE_LABEL).clicked() {
                *action = Some(RowAction::OpenMerge(i));
                ui.close();
            }
            if ctext::can_move_category(c) && ui.button(ctext::BULK_MOVE_LABEL).clicked() {
                *action = Some(RowAction::OpenBulkMove(i));
                ui.close();
            }
            if !c.aliases.is_empty() {
                ui.separator();
                ui.weak(t.tag_confirm_primary_alias());
                for (pos, a) in c.aliases.iter().enumerate() {
                    if ui.radio(pos == 0, a).clicked() {
                        *action = Some(RowAction::SetPrimary(i, pos));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(ctext::SPLIT_LABEL).clicked() {
                    *action = Some(RowAction::Split(i));
                    ui.close();
                }
            }
        });
    });

    // 2段目（標準のみ）: 何をどこにどうするかの1文を、「内容・要確認」の列の中に出す（列をまたがない）。
    if !compact {
        in_cell(ui, at(cols.x_state() + CELL_PAD, cols.x_ops() - CELL_PAD, line1_h, row_h), left, |ui| {
            ui.add(egui::Label::new(conclusion.as_str()).truncate()).on_hover_text(&conclusion);
        });
    }

    // 罫線: 列の境目（行の全高）と、行の下の全幅の区切り。
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    for x in [cols.x_name(), cols.x_count(), cols.x_state(), cols.x_ops()] {
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
        let t = i18n::t();
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
                self.set_toast(t.tag_apply_done(&report));
            }
            Err(ApplyError::BackupFailed) => self.set_toast(t.tag_apply_backup_failed()),
            Err(ApplyError::RecordFailed) => self.set_toast(t.tag_apply_record_failed()),
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
