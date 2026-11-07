//! タグ候補の確認ウィンドウ（2枚）。走査・候補生成が終わったあとに開く。
//!
//! - 1枚目: 作者・グループ名を除く項目（形式・メイン・言語・イベント）。「次へ」で2枚目へ
//! - 2枚目: 作者・グループ名。「戻る」「適用」「キャンセル」。キャンセルは全部を捨てる（ディスクには何も書いていない）
//! - 上部で、照合の切替項目（正規化5項目と類似のしきい値）を切り替えられる。切り替えると別スレッドで候補を作り直し、
//!   ユーザーが変えた採否は、同じ候補に引き継ぐ
//! - 行は、採否のチェック・名前・対象の件数・「未確定」の印と、類似提案があれば選択欄（既存の候補・新規作成・付けない）
//! - 類似品の束は見出しの行を出し、まとめて採用・不採用にできる
//!
//! 「適用」は、この単位では押せない（次の単位で、バックアップ・マスターへの追加・タグ付けを実装する）。

use std::collections::HashMap;
use std::sync::{mpsc, Arc};

use crate::i18n::{self, Lang};
use crate::tag_candidates::{
    build_candidates, CandidateInput, CandidateSet, CatRef, Choice, Role, Window, ERROR_NEW_ELEMENTS, WARN_NEW_ELEMENTS,
};
use crate::tag_similarity::MatchOptions;

use super::{NekoviewApp, TagManagerCategoryUi};

/// 一覧の1行（種類の見出し・類似品の見出し・候補）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Section(Role),
    Cluster { id: usize, size: usize },
    Cand(usize),
}

/// 窓に出す行を作る。`only_unconfirmed` なら未確定の候補だけ。
pub(super) fn rows_for(set: &CandidateSet, window: Window, only_unconfirmed: bool) -> Vec<Row> {
    let idx: Vec<usize> = set
        .indices_in_window(window)
        .into_iter()
        .filter(|&i| !only_unconfirmed || set.candidates[i].unconfirmed)
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
    master: Arc<Vec<TagManagerCategoryUi>>,
    stage: Window,
    only_unconfirmed: bool,
    rebuild: Option<mpsc::Receiver<CandidateSet>>,
    /// しきい値スライダーをドラッグしている間の仮の値（離したときに確定して作り直す）。
    threshold_draft: Option<u8>,
}

impl TagConfirm {
    pub(super) fn new(
        set: CandidateSet,
        inputs: Vec<CandidateInput>,
        master: Vec<TagManagerCategoryUi>,
    ) -> Option<Self> {
        let has = |w: Window| set.candidates.iter().any(|c| c.role.window() == w);
        let stage = if has(Window::One) {
            Window::One
        } else if has(Window::Two) {
            Window::Two
        } else {
            return None;
        };
        Some(Self { set, inputs: Arc::new(inputs), master: Arc::new(master), stage, only_unconfirmed: false, rebuild: None, threshold_draft: None })
    }

    /// 切替項目が変わったので、別スレッドで候補を作り直す。進行中の作り直しは捨てる。
    fn start_rebuild(&mut self, opts: MatchOptions, lang: Lang) {
        let (tx, rx) = mpsc::channel();
        let (inputs, master) = (Arc::clone(&self.inputs), Arc::clone(&self.master));
        std::thread::spawn(move || {
            let _ = tx.send(build_candidates(&inputs, &master, &opts, lang));
        });
        self.rebuild = Some(rx);
    }

    /// 作り直しの完了を取り込む。ユーザーが変えた採否は引き継ぐ。
    fn poll_rebuild(&mut self) -> bool {
        let Some(rx) = &self.rebuild else { return false };
        match rx.try_recv() {
            Ok(mut fresh) => {
                self.set.carry_over_choices(&mut fresh);
                self.set = fresh;
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
    }
}

impl NekoviewApp {
    /// 走査と候補生成の完了後に、確認ウィンドウを開く（候補が1つも無ければ開かない）。
    pub(super) fn open_tag_confirm(&mut self, set: CandidateSet, inputs: Vec<CandidateInput>, master: Vec<TagManagerCategoryUi>) {
        match TagConfirm::new(set, inputs, master) {
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
        let mut opts = self.config.tag_match;
        let before = opts;

        egui::Window::new(match stage {
            Window::One => t.tag_confirm_title_one(),
            Window::Two => t.tag_confirm_title_two(),
        })
        .id(egui::Id::new("tag_confirm_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([780.0, 560.0])
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

            // 警告（新規要素数）。
            // 採否を変えると新規要素数も変わるので、毎フレーム数え直す（候補の数だけの軽い集計）。
            let limits = crate::tag_candidates::recount_limits(&st.set.candidates);
            let (new_count, warn) = match stage {
                Window::One => (limits.window1_new, limits.window1_warn),
                Window::Two => (limits.window2_new, limits.window2_warn),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(t.tag_confirm_summary(adopted_count(&st.set, stage), new_count));
                if warn {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_confirm_warn_many(WARN_NEW_ELEMENTS));
                }
                if new_count > ERROR_NEW_ELEMENTS {
                    ui.colored_label(egui::Color32::from_rgb(220, 60, 60), t.tag_confirm_error_many(ERROR_NEW_ELEMENTS));
                }
            });

            // 一括操作。
            let window_idx: Vec<usize> = st.set.indices_in_window(stage);
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
            ui.separator();

            // 一覧。
            let rows = rows_for(&st.set, stage, st.only_unconfirmed);
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
                                });
                            }
                            Row::Cand(i) => draw_candidate_row(ui, &mut st.set, i, &st.master, t),
                        }
                    }
                    if rows.is_empty() {
                        ui.weak(t.tag_confirm_empty());
                    }
                },
            );

            // 操作ボタン。
            ui.separator();
            ui.horizontal(|ui| {
                if stage == Window::Two {
                    let back_enabled = st.set.candidates.iter().any(|c| c.role.window() == Window::One);
                    if ui.add_enabled(back_enabled, egui::Button::new(t.tag_confirm_back())).clicked() {
                        next_stage = Some(Window::One);
                    }
                    // 適用は次の単位で実装する。
                    ui.add_enabled(false, egui::Button::new(t.tag_confirm_apply())).on_disabled_hover_text(t.tag_confirm_apply_pending());
                } else {
                    let has_two = st.set.candidates.iter().any(|c| c.role.window() == Window::Two);
                    if ui.add_enabled(has_two, egui::Button::new(t.tag_confirm_next())).clicked() {
                        next_stage = Some(Window::Two);
                    }
                    if !has_two {
                        ui.add_enabled(false, egui::Button::new(t.tag_confirm_apply())).on_disabled_hover_text(t.tag_confirm_apply_pending());
                    }
                }
                if ui.button(t.archive_open_cancel()).clicked() {
                    cancel = true;
                }
            });
        });

        if opts != before {
            let clamped = opts.clamped();
            self.config.tag_match = clamped;
            self.persist_state();
            st.start_rebuild(clamped, i18n::t());
        }
        if let Some(s) = next_stage {
            st.stage = s;
        }
        if open && !cancel {
            self.tag_confirm = Some(st);
        }
    }
}

/// 候補1行。
fn draw_candidate_row(ui: &mut egui::Ui, set: &mut CandidateSet, i: usize, master: &[TagManagerCategoryUi], t: Lang) {
    let c = &mut set.candidates[i];
    ui.horizontal(|ui| {
        let mut adopted = c.is_adopted();
        if ui.checkbox(&mut adopted, "").changed() {
            if adopted {
                c.adopt();
            } else {
                c.unadopt();
            }
        }
        let label = egui::RichText::new(&c.name);
        ui.label(if matches!(c.choice, Choice::New) { label.strong() } else { label });
        ui.weak(t.tag_confirm_file_count(c.files.len()));
        if c.unconfirmed {
            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_confirm_unconfirmed());
        }
        match &c.choice {
            Choice::Existing { tier_id, .. } if c.suggestions.is_empty() => {
                ui.weak(t.tag_confirm_existing(&element_name(master, *tier_id)));
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
                    if ui.selectable_label(selected, format!("{} ({}%)", s.name, s.score)).clicked() {
                        c.choice = Choice::Existing { cat_idx: s.cat_idx, tier_id: s.tier_id };
                    }
                }
                if ui.selectable_label(matches!(c.choice, Choice::Skip), t.tag_confirm_choice_skip()).clicked() {
                    c.choice = Choice::Skip;
                }
            });
        }
    });
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
        let w1 = rows_for(&set, Window::One, false);
        assert_eq!(w1.first(), Some(&Row::Section(Role::Format)));
        assert!(w1.contains(&Row::Section(Role::Main)));
        assert!(!w1.iter().any(|r| matches!(r, Row::Section(Role::Author))));
        let w2 = rows_for(&set, Window::Two, false);
        assert_eq!(w2.first(), Some(&Row::Section(Role::Author)), "種類順は作者→グループ");
        // Bbbbbbb と Bbbbbbc は類似で、同じ束の見出しが付く。
        assert!(w2.iter().any(|r| matches!(r, Row::Cluster { size: 2, .. })), "{w2:?}");
        let cand_rows = w2.iter().filter(|r| matches!(r, Row::Cand(_))).count();
        assert_eq!(cand_rows, 3, "Aaa / Bbbbbbb / Bbbbbbc");
        // 未確定だけ: すべて未確定（既存タグが無い）。何も無いマスターでは変わらない。
        assert_eq!(rows_for(&set, Window::Two, true).len(), w2.len());
        // 窓1の候補は未確定ではないので、フィルタで消える。
        assert!(rows_for(&set, Window::One, true).is_empty());
    }

    #[test]
    fn confirm_starts_at_the_first_window_that_has_candidates() {
        let only_authors = build_candidates(&[crate::tag_candidates::input_from_info_text("[A (B)] t.zip", None, None)], &[], &MatchOptions::default(), Lang::English);
        let c = TagConfirm::new(only_authors, vec![], vec![]).unwrap();
        assert_eq!(c.stage, Window::Two, "窓1が空なら2枚目から");
        let empty = build_candidates(&[], &[], &MatchOptions::default(), Lang::English);
        assert!(TagConfirm::new(empty, vec![], vec![]).is_none());
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
