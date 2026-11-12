//! タグ候補の確認ウィンドウ（2枚）。走査・候補生成が終わったあとに開く。
//!
//! - 1枚目: 作者・グループ名を除く項目（形式・メイン・言語・イベント）。「次へ」で2枚目へ
//! - 2枚目: 作者・グループ名。「戻る」「適用」「キャンセル」。キャンセルは全部を捨てる（ディスクには何も書いていない）
//! - 上部で、照合の切替項目（正規化5項目と類似のしきい値）を切り替えられる。切り替えると別スレッドで候補を作り直し、
//!   ユーザーが変えた採否は、同じ候補に引き継ぐ
//! - 行は、採否のチェック・名前・対象の件数・「未確定」の印と、類似提案があれば選択欄（既存の候補・新規作成・付けない）
//! - 類似品の束は見出しの行を出し、まとめて採用・不採用にできる
//!
//! 「適用」は、DBのバックアップ → マスターへの新規カテゴリ・要素の追加 → 各ファイルへのタグ追加、を別スレッドで行う
//! （`tag_apply`）。既存のタグは消さない。バックアップに失敗したら、何も変更せずに中止する。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crate::i18n::{self, Lang};
use crate::tag_candidates::{
    build_candidates_full, over_limit, Bundle, Candidate, CandidateInput, CandidateSet, CatRef, Choice, MergeError, Role, Window,
    LIMIT_EXPAND_STEP,
};
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

/// 絞り込み欄の文字列に合う候補か（名前・別名・見つかった表記のどれかに、正規化した部分一致。空なら全部）。
fn matches_query(c: &Candidate, query: &str, opts: &MatchOptions) -> bool {
    let q = keys(query, opts).exact;
    if q.is_empty() {
        return true;
    }
    let hit = |s: &str| keys(s, opts).exact.contains(&q);
    split_aliases(&c.name).into_iter().any(hit) || c.forms.iter().any(|f| hit(f))
}

/// 窓に出す行を作る。`only_unconfirmed` なら未確定の候補だけ。`query` は名前の絞り込み（別名を含む部分一致）。
pub(super) fn rows_for(set: &CandidateSet, window: Window, only_unconfirmed: bool, query: &str, opts: &MatchOptions) -> Vec<Row> {
    let idx: Vec<usize> = set
        .indices_in_window(window)
        .into_iter()
        .filter(|&i| !only_unconfirmed || set.candidates[i].unconfirmed || set.candidates[i].review)
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

/// 候補を「付ける」件数（採用済み）。
pub(super) fn adopted_count(set: &CandidateSet, window: Window) -> usize {
    set.candidates.iter().filter(|c| c.role.window() == window && c.is_adopted()).count()
}

/// tier_id の要素名（表示用）。
fn element_name(master: &[TagManagerCategoryUi], tier_id: u64) -> String {
    master
        .iter()
        .flat_map(|c| c.tiers.iter())
        .find(|t| t.id == tier_id)
        .and_then(|t| t.element.clone())
        .unwrap_or_else(|| format!("#{tier_id}"))
}

pub(super) struct TagConfirm {
    set: CandidateSet,
    inputs: Arc<Vec<CandidateInput>>,
    /// 入力と同じ順のファイルのパス（適用でタグを書く先）。
    paths: Vec<PathBuf>,
    master: Arc<Vec<TagManagerCategoryUi>>,
    /// 候補生成に使った学習モデル（切替項目を変えた作り直しでも同じモデルを使う）。
    model: Option<Arc<Model>>,
    /// 候補生成に使った大ルール（作り直しでも同じルールを使う）。
    rules: Arc<Vec<TagRule>>,
    stage: Window,
    only_unconfirmed: bool,
    rebuild: Option<mpsc::Receiver<CandidateSet>>,
    /// しきい値スライダーをドラッグしている間の仮の値（離したときに確定して作り直す）。
    threshold_draft: Option<u8>,
    /// 「まとめる」にチェックした候補（種類, 候補のキー）。
    checked: HashSet<(Role, String)>,
    /// 名前の絞り込み欄。
    filter: String,
    next_bundle_id: u64,
    /// まとめを拒否したときの理由（窓に出す）。
    merge_msg: Option<String>,
    /// 適用前の確認（既存要素の名前が書き換わるとき）。Some の間は確認の窓を出す。
    rename_review: Option<RenameReview>,
    /// 1カテゴリあたりの新規要素数の上限（設定の既定値から始まり、「拡張する」で増える）。
    limit: usize,
    /// 「上限を維持する」を選んだ。作り直したあとも、上限を超えた分を切り捨てる。
    limit_keep: bool,
    /// 切り捨てて候補から除いた数（種類ごと）。窓に注意書きを出す。
    truncated: Vec<(Role, usize)>,
}

/// 適用前に見せる、書き換わる既存要素の一覧。
struct RenameReview {
    /// （書き換え前, 書き換え後）。
    renames: Vec<(String, String)>,
    /// 別名が重なって書き換えない候補の数。
    conflicts: usize,
}

/// 一覧の行から出す操作（描画中は候補の並びを変えられないので、描画後にまとめて行う）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    /// 束（類似品の同じ番号）をまとめる。
    MergeCluster(usize),
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
    ) -> Option<Self> {
        let has = |w: Window| set.candidates.iter().any(|c| c.role.window() == w);
        let stage = if has(Window::One) {
            Window::One
        } else if has(Window::Two) {
            Window::Two
        } else {
            return None;
        };
        Some(Self { set, inputs: Arc::new(inputs), paths, master: Arc::new(master), model, rules, stage, only_unconfirmed: false, rebuild: None, threshold_draft: None, checked: HashSet::new(), filter: String::new(), next_bundle_id: 1, merge_msg: None, rename_review: None, limit, limit_keep: false, truncated: Vec::new() })
    }

    /// 切替項目が変わったので、別スレッドで候補を作り直す。進行中の作り直しは捨てる。
    fn start_rebuild(&mut self, opts: MatchOptions, lang: Lang, name_lang: Lang) {
        let (tx, rx) = mpsc::channel();
        let (inputs, master, model, rules) = (Arc::clone(&self.inputs), Arc::clone(&self.master), self.model.clone(), Arc::clone(&self.rules));
        // 手動でまとめた候補は、作り直したあとも同じまとめを復元する。
        let bundles: Vec<Bundle> = self.set.snapshot_bundles();
        std::thread::spawn(move || {
            let mut fresh = build_candidates_full(&inputs, &master, &opts, lang, name_lang, model.as_deref(), &rules);
            fresh.apply_bundles(&bundles, &master);
            let _ = tx.send(fresh);
        });
        self.rebuild = Some(rx);
        self.checked.clear();
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
        match TagConfirm::new(
            set,
            inputs,
            paths,
            master,
            self.active_tag_model(),
            Arc::new(self.tag_rules.rules.clone()),
            self.config.tag_new_element_limit as usize,
        ) {
            Some(c) => self.tag_confirm = Some(c),
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
        let mut confirm_merge = false;
        let mut row_action: Option<RowAction> = None;
        let mut opts = self.config.tag_match;
        let before = opts;

        let center = self.explorer_center(ctx);
        egui::Window::new(match stage {
            Window::One => t.tag_confirm_title_one(),
            Window::Two => t.tag_confirm_title_two(),
        })
        .id(egui::Id::new("tag_confirm_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([780.0, 560.0])
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(center)
        .show(ctx, |ui| {
            // 照合の切替項目。
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut opts.width, t.tag_confirm_opt_width());
                ui.checkbox(&mut opts.space, t.tag_confirm_opt_space());
                ui.checkbox(&mut opts.hyphen_underscore, t.tag_confirm_opt_hyphen());
                ui.checkbox(&mut opts.symbols, t.tag_confirm_opt_symbols());
                ui.checkbox(&mut opts.romaji, t.tag_confirm_opt_romaji());
            });
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
            ui.separator();

            // 新規要素数。採否を変えると変わるので、毎フレーム数え直す（候補の数だけの軽い集計）。
            let limits = crate::tag_candidates::recount_limits(&st.set.candidates);
            let new_count = match stage {
                Window::One => limits.window1_new,
                Window::Two => limits.window2_new,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(t.tag_confirm_summary(adopted_count(&st.set, stage), new_count));
            });
            // 上限のため切り捨てた分があれば知らせる。
            if !st.truncated.is_empty() {
                let rows: Vec<String> = st.truncated.iter().map(|&(r, n)| format!("{} {n}", role_label(r))).collect();
                ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_confirm_truncated(&rows.join(" / ")));
            }

            // 一括操作。
            let window_idx: Vec<usize> = st.set.indices_in_window(stage);
            // 大ルールが成立したファイルがあれば、ルール名と件数を出す。
            if !st.set.rule_hits.is_empty() {
                let hits: Vec<String> = st.set.rule_hits.iter().map(|(name, n)| format!("{name} {n}件")).collect();
                ui.weak(t.tag_confirm_rule_hits(&hits.join(" / ")));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button(t.tag_confirm_adopt_all()).clicked() {
                    for &i in &window_idx {
                        st.set.candidates[i].adopt();
                    }
                }
                if ui.button(t.tag_confirm_unadopt_all()).clicked() {
                    for &i in &window_idx {
                        st.set.candidates[i].unadopt();
                    }
                }
                if ui.button(t.tag_confirm_unadopt_unconfirmed()).clicked() {
                    for &i in &window_idx {
                        if st.set.candidates[i].unconfirmed {
                            st.set.candidates[i].unadopt();
                        }
                    }
                }
                ui.checkbox(&mut st.only_unconfirmed, t.tag_confirm_only_unconfirmed());
            });
            // 名前の絞り込みと、チェックした行をまとめる操作。
            let checked_here = window_idx
                .iter()
                .filter(|&&i| st.checked.contains(&(st.set.candidates[i].role, st.set.candidates[i].key.clone())))
                .count();
            ui.horizontal_wrapped(|ui| {
                ui.label(t.tag_confirm_filter_label());
                ui.add(egui::TextEdit::singleline(&mut st.filter).desired_width(160.0).hint_text(t.tag_confirm_filter_hint()));
                if !st.filter.is_empty() && ui.small_button("✕").clicked() {
                    st.filter.clear();
                }
                ui.separator();
                let btn = ui.add_enabled(checked_here >= 2, egui::Button::new(t.tag_confirm_merge_button(checked_here)));
                if btn.on_hover_text(t.tag_confirm_merge_hover()).on_disabled_hover_text(t.tag_confirm_merge_hover()).clicked() {
                    confirm_merge = true;
                }
                if checked_here > 0 && ui.small_button(t.tag_confirm_merge_clear()).clicked() {
                    st.checked.clear();
                }
            });
            if let Some(msg) = &st.merge_msg {
                ui.colored_label(egui::Color32::from_rgb(220, 60, 60), msg);
            }
            ui.separator();

            // 一覧。
            let rows = rows_for(&st.set, stage, st.only_unconfirmed, &st.filter, &opts);
            let row_h = ui.spacing().interact_size.y + 6.0;
            let footer_h = 40.0;
            let list_h = (ui.available_height() - footer_h).max(80.0);
            egui::ScrollArea::vertical().id_salt(("tag_confirm_list", stage as u8)).max_height(list_h).auto_shrink([false, false]).show_rows(
                ui,
                row_h,
                rows.len(),
                |ui, range| {
                    for row in &rows[range] {
                        match *row {
                            Row::Section(role) => {
                                ui.strong(role_label(role));
                            }
                            Row::Cluster { id, size } => {
                                ui.horizontal(|ui| {
                                    ui.weak(t.tag_confirm_cluster_header(size));
                                    if ui.small_button(t.tag_confirm_cluster_adopt()).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.adopt();
                                        }
                                    }
                                    if ui.small_button(t.tag_confirm_cluster_unadopt()).clicked() {
                                        for c in st.set.candidates.iter_mut().filter(|c| c.cluster == id && c.role.window() == stage) {
                                            c.unadopt();
                                        }
                                    }
                                    if ui.small_button(t.tag_confirm_cluster_merge()).on_hover_text(t.tag_confirm_merge_hover()).clicked() {
                                        row_action = Some(RowAction::MergeCluster(id));
                                    }
                                });
                            }
                            Row::Cand(i) => {
                                draw_candidate_row(ui, &mut st.set, i, &st.master, &mut st.checked, &mut row_action, t)
                            }
                        }
                    }
                    if rows.is_empty() {
                        ui.weak(t.tag_confirm_empty());
                    }
                },
            );

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

        // まとめる・分解・先頭の別名の変更（描画中は並びを変えられないので、ここで行う）。
        let merge_idxs: Option<Vec<usize>> = match row_action {
            Some(RowAction::MergeCluster(id)) => Some(
                (0..st.set.candidates.len())
                    .filter(|&i| st.set.candidates[i].cluster == id && st.set.candidates[i].role.window() == stage)
                    .collect(),
            ),
            _ if confirm_merge => Some(
                (0..st.set.candidates.len())
                    .filter(|&i| {
                        let c = &st.set.candidates[i];
                        c.role.window() == stage && st.checked.contains(&(c.role, c.key.clone()))
                    })
                    .collect(),
            ),
            _ => None,
        };
        if let Some(idxs) = merge_idxs {
            let id = st.next_bundle_id;
            match st.set.merge_candidates(&idxs, None, id, &st.master) {
                Ok(_) => {
                    st.next_bundle_id += 1;
                    st.checked.clear();
                    st.merge_msg = None;
                }
                Err(e) => st.merge_msg = Some(merge_error_text(&e, t)),
            }
        }
        match row_action {
            Some(RowAction::Split(i)) => {
                st.set.split_candidate(i);
                st.merge_msg = None;
            }
            Some(RowAction::SetPrimary(i, pos)) => st.set.set_primary_alias(i, pos),
            _ => {}
        }
        if opts != before {
            let clamped = opts.clamped();
            self.config.tag_match = clamped;
            self.persist_state();
            st.start_rebuild(clamped, i18n::t(), self.config.tag_register_name.name_lang(i18n::t()));
        }
        if let Some(s) = next_stage {
            st.stage = s;
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

fn merge_error_text(e: &MergeError, t: Lang) -> String {
    match e {
        MergeError::TooFew => t.tag_confirm_merge_err_too_few().to_string(),
        MergeError::MixedRoles => t.tag_confirm_merge_err_mixed().to_string(),
        MergeError::MultipleExisting { first, second } => t.tag_confirm_merge_err_multiple(first, second),
        MergeError::AliasOverlap { alias, element } => t.tag_confirm_merge_err_overlap(alias, element),
    }
}

/// 候補1行。
fn draw_candidate_row(
    ui: &mut egui::Ui,
    set: &mut CandidateSet,
    i: usize,
    master: &[TagManagerCategoryUi],
    checked: &mut HashSet<(Role, String)>,
    action: &mut Option<RowAction>,
    t: Lang,
) {
    let c = &mut set.candidates[i];
    ui.horizontal(|ui| {
        // 「まとめる」の対象にするチェック（採否のチェックとは別）。
        let id = (c.role, c.key.clone());
        let mut merge_on = checked.contains(&id);
        if ui.checkbox(&mut merge_on, "").on_hover_text(t.tag_confirm_merge_check_hover()).changed() {
            if merge_on {
                checked.insert(id);
            } else {
                checked.remove(&id);
            }
        }
        let mut adopted = c.is_adopted();
        if ui.checkbox(&mut adopted, "").changed() {
            if adopted {
                c.adopt();
            } else {
                c.unadopt();
            }
        }
        let label = egui::RichText::new(&c.name);
        let name_resp = ui.label(if matches!(c.choice, Choice::New) { label.strong() } else { label });
        if !c.aliases.is_empty() {
            name_resp.on_hover_text(t.tag_confirm_merged_hover(c.aliases.len()));
            ui.menu_button(t.tag_confirm_merged_menu(), |ui| {
                ui.weak(t.tag_confirm_primary_alias());
                for (pos, a) in c.aliases.iter().enumerate() {
                    if ui.radio(pos == 0, a).clicked() {
                        *action = Some(RowAction::SetPrimary(i, pos));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(t.tag_confirm_split()).clicked() {
                    *action = Some(RowAction::Split(i));
                    ui.close();
                }
            });
        }
        // 対にした別の見出し行の表記（英語⇄日本語）。登録するときは、登録名の別名として足される。
        if !c.alt_forms.is_empty() {
            ui.weak(format!("（{}）", c.alt_forms.join(" / ")));
        }
        ui.weak(t.tag_confirm_file_count(c.files.len()));
        if c.unconfirmed {
            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_confirm_unconfirmed());
        }
        // 無効と判定されて、未採用で残している候補（ルール・学習のどちらによるかを添える）。
        if c.review {
            let amber = egui::Color32::from_rgb(230, 160, 40);
            match &c.rule_reason {
                Some(rule) => ui.colored_label(amber, t.tag_confirm_review_rule(rule)),
                None => ui.colored_label(amber, t.tag_confirm_review_learned()),
            };
        }
        match &c.choice {
            Choice::Existing { tier_id, .. } if c.suggestions.is_empty() => {
                ui.weak(t.tag_confirm_existing(&element_name(master, *tier_id)));
            }
            Choice::Alias { tier_id, .. } if c.suggestions.is_empty() => {
                ui.weak(t.tag_confirm_choice_alias(&element_name(master, *tier_id), &c.name));
            }
            _ if c.category == CatRef::New && matches!(c.choice, Choice::New) => {
                ui.weak(t.tag_confirm_new_category(c.role.new_category_name(t)));
            }
            _ => {}
        }
        if !c.suggestions.is_empty() {
            let current = match &c.choice {
                Choice::Existing { tier_id, .. } => {
                    let score = c.suggestions.iter().find(|s| s.tier_id == *tier_id).map_or(100, |s| s.score);
                    format!("{} ({score}%)", element_name(master, *tier_id))
                }
                Choice::Alias { tier_id, .. } => t.tag_confirm_choice_alias(&element_name(master, *tier_id), &c.name),
                Choice::New => t.tag_confirm_choice_new(&c.name),
                Choice::Skip => t.tag_confirm_choice_skip().to_string(),
            };
            egui::ComboBox::from_id_salt(("tag_confirm_choice", i)).selected_text(current).width(220.0).show_ui(ui, |ui| {
                if ui.selectable_label(matches!(c.choice, Choice::New), t.tag_confirm_choice_new(&c.name)).clicked() {
                    c.choice = Choice::New;
                }
                let suggestions = c.suggestions.clone();
                for s in suggestions {
                    let selected = matches!(c.choice, Choice::Existing { tier_id, .. } if tier_id == s.tier_id);
                    let shown = if s.alias != s.name && s.name.contains('|') {
                        format!("{} ({}%) ≈ {}", s.name, s.score, s.alias)
                    } else {
                        format!("{} ({}%)", s.name, s.score)
                    };
                    if ui.selectable_label(selected, shown).clicked() {
                        c.choice = Choice::Existing { cat_idx: s.cat_idx, tier_id: s.tier_id };
                    }
                    let alias_selected = matches!(c.choice, Choice::Alias { tier_id, .. } if tier_id == s.tier_id);
                    if ui.selectable_label(alias_selected, t.tag_confirm_choice_alias(&s.name, &c.name)).clicked() {
                        c.choice = Choice::Alias { cat_idx: s.cat_idx, tier_id: s.tier_id };
                    }
                }
                if ui.selectable_label(matches!(c.choice, Choice::Skip), t.tag_confirm_choice_skip()).clicked() {
                    c.choice = Choice::Skip;
                }
            });
        }
    });
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
            let out = run_apply(&db, &paths, &plan, &worker_master, &backup, &save_master, &save_record);
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
        let w1 = rows_for(&set, Window::One, false, "", &MatchOptions::default());
        assert_eq!(w1.first(), Some(&Row::Section(Role::Format)));
        assert!(w1.contains(&Row::Section(Role::Main)));
        assert!(!w1.iter().any(|r| matches!(r, Row::Section(Role::Author))));
        let w2 = rows_for(&set, Window::Two, false, "", &MatchOptions::default());
        assert_eq!(w2.first(), Some(&Row::Section(Role::Author)), "種類順は作者→グループ");
        // Bbbbbbb と Bbbbbbc は類似で、同じ束の見出しが付く。
        assert!(w2.iter().any(|r| matches!(r, Row::Cluster { size: 2, .. })), "{w2:?}");
        let cand_rows = w2.iter().filter(|r| matches!(r, Row::Cand(_))).count();
        assert_eq!(cand_rows, 3, "Aaa / Bbbbbbb / Bbbbbbc");
        // 未確定だけ: すべて未確定（既存タグが無い）。何も無いマスターでは変わらない。
        assert_eq!(rows_for(&set, Window::Two, true, "", &MatchOptions::default()).len(), w2.len());
        // 窓1の候補は未確定ではないので、フィルタで消える。
        assert!(rows_for(&set, Window::One, true, "", &MatchOptions::default()).is_empty());
    }

    #[test]
    fn query_filters_by_name_alias_and_form_with_normalization() {
        let inputs = [input("[Aaa (Kouji)] t.zip", None), input("[Aaa (Zzz)] u.zip", None)];
        let cats: Vec<TagManagerCategoryUi> = Vec::new();
        let mut set = build_candidates(&inputs, &cats, &MatchOptions::default(), Lang::English);
        let opts = MatchOptions::default();
        let names = |set: &CandidateSet, q: &str| -> Vec<String> {
            rows_for(set, Window::Two, false, q, &opts)
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
        let c = TagConfirm::new(only_authors, vec![], vec![], vec![], None, Arc::new(Vec::new()), 5000).unwrap();
        assert_eq!(c.stage, Window::Two, "窓1が空なら2枚目から");
        let empty = build_candidates(&[], &[], &MatchOptions::default(), Lang::English);
        assert!(TagConfirm::new(empty, vec![], vec![], vec![], None, Arc::new(Vec::new()), 5000).is_none());
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
