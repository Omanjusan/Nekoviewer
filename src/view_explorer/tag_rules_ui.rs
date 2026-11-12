//! タグ自動取得の大ルール（`tag_rules`）の窓「タグ抽出ルール定義」。
//!
//! 一覧（カードで有効/無効・編集・削除、コピー、初期ルールの足し直し、再読込）と、ステップ式の編集画面
//! （①名前 → ②条件 → ③結果 → ④確認＋ためし判定）。編集は下書きに対して行い、④の「保存して一覧へ」で保存する。
//! 条件の「対象」は選ばせない（文字列・形の条件は、ファイル名・見出し行・Category・題名を自動ですべて見る）。

use crate::i18n;
use crate::tag_rules::{CondTarget, Condition, NameShape, RuleInput, RuleRole, TagRule, TextMatch};
use crate::tag_similarity::MatchOptions;

use super::NekoviewApp;

/// ルール一覧に出す、種類の表示名。
fn role_label(role: RuleRole) -> &'static str {
    let t = i18n::t();
    match role {
        RuleRole::Format => t.tag_confirm_role_format(),
        RuleRole::Main => t.tag_confirm_role_main(),
        RuleRole::Language => t.tag_confirm_role_language(),
        RuleRole::Event => t.tag_confirm_role_event(),
        RuleRole::Author => t.tag_confirm_role_author(),
        RuleRole::Group => t.tag_confirm_role_group(),
        RuleRole::Performer => t.tag_confirm_role_performer(),
        RuleRole::Character => t.tag_confirm_role_character(),
        RuleRole::Parody => t.tag_confirm_role_parody(),
        RuleRole::Attribute => t.tag_confirm_role_attribute(),
        RuleRole::YearMonth => t.tag_confirm_role_year_month(),
    }
}

/// ルール1件の概要（「条件 N件（いずれか） / 無効: … / 有効: …」）。
fn summary(rule: &TagRule) -> String {
    let join = |set: &std::collections::BTreeSet<RuleRole>| set.iter().map(|r| role_label(*r)).collect::<Vec<_>>().join("・");
    i18n::t().tag_rules_summary(rule.conditions.len(), rule.match_all, &join(&rule.effect.disable), &join(&rule.effect.enable))
}

/// 条件の対象のすべて（新しく作る条件は、これを見る）。
const ALL_TARGETS: [CondTarget; 4] = [CondTarget::FileName, CondTarget::InfoTitle, CondTarget::InfoCategory, CondTarget::Title];

fn is_all_targets(targets: &[CondTarget]) -> bool {
    ALL_TARGETS.iter().all(|t| targets.contains(t))
}

fn target_label(t: CondTarget) -> &'static str {
    let l = i18n::t();
    match t {
        CondTarget::FileName => l.tag_rules_target_file_name(),
        CondTarget::InfoTitle => l.tag_rules_target_info_title(),
        CondTarget::InfoCategory => l.tag_rules_target_info_category(),
        CondTarget::Title => l.tag_rules_target_title(),
    }
}

/// ステップの数（①名前 ②条件 ③結果 ④確認）。
const STEPS: usize = 4;
/// 編集できるステップの数。
const READY_STEPS: usize = 4;

/// 編集を離れるときの行き先（未保存なら、確認を出してから）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leave {
    ToList,
    CloseWindow,
}

/// 1件のルールの編集（下書き）。「保存して一覧へ」まで、ルール一式には入らない。
pub(super) struct RuleEditor {
    draft: TagRule,
    /// 新しく作るルール（保存で、一覧の末尾に足す）。
    is_new: bool,
    step: usize,
    dirty: bool,
    /// 未保存のまま離れようとして、確認を出している。
    leave_ask: Option<Leave>,
    /// ためし判定の入力（④）。保存しない。
    trial_file: String,
    trial_titles: String,
    trial_category: String,
}

impl RuleEditor {
    fn edit(rule: TagRule) -> Self {
        Self { draft: rule, is_new: false, step: 0, dirty: false, leave_ask: None, trial_file: String::new(), trial_titles: String::new(), trial_category: String::new() }
    }

    /// 空のルール（無効・条件1つ（空）・効果なし）。
    fn blank(name: String) -> Self {
        let draft = TagRule {
            id: 0,
            name,
            enabled: false,
            conditions: vec![Condition::Text { targets: ALL_TARGETS.to_vec(), how: TextMatch::Contains, text: String::new() }],
            match_all: false,
            effect: Default::default(),
        };
        Self { draft, is_new: true, step: 0, dirty: false, leave_ask: None, trial_file: String::new(), trial_titles: String::new(), trial_category: String::new() }
    }

    /// 入力の誤り（（ステップの番号, 理由））。他のルールの名前と重なる名前は、誤り。
    fn errors(&self, other_names: &[String]) -> Vec<(usize, &'static str)> {
        let t = i18n::t();
        let mut out = Vec::new();
        let name = self.draft.name.trim();
        if name.is_empty() {
            out.push((0, t.tag_rules_err_name_empty()));
        } else if other_names.iter().any(|n| n.trim() == name) {
            out.push((0, t.tag_rules_err_name_dup()));
        }
        if self.draft.conditions.is_empty() {
            out.push((1, t.tag_rules_err_no_condition()));
        }
        if self.draft.conditions.iter().any(|c| matches!(c, Condition::Text { text, .. } if text.trim().is_empty())) {
            out.push((1, t.tag_rules_err_text_empty()));
        }
        // 有効なルールは、効果を1つ以上持つ（効果が空でいいのは、無効のルールだけ）。
        if self.draft.enabled && self.draft.effect.disable.is_empty() && self.draft.effect.enable.is_empty() {
            out.push((2, t.tag_rules_err_effect_empty()));
        }
        out
    }
}

/// 編集画面の描画の結果（描画中は状態を変えられないので、あとで行う）。
#[derive(Default)]
struct EditorOut {
    /// 一覧へ戻る（未保存なら確認を出す）。
    back_to_list: bool,
    /// 保存して一覧へ。
    save: bool,
    /// 確認で「破棄する」を選んだ。
    discard: bool,
}

fn how_label(h: TextMatch) -> &'static str {
    let t = i18n::t();
    match h {
        TextMatch::Contains => t.tag_rules_how_contains(),
        TextMatch::Prefix => t.tag_rules_how_prefix(),
        TextMatch::Exact => t.tag_rules_how_exact(),
    }
}

fn shape_label(s: NameShape) -> &'static str {
    match s {
        NameShape::Dash2 => "X - Y",
        NameShape::Dash2Paren => "X - Y (Z)",
        NameShape::Dash3 => "X - Y - Z",
    }
}

/// 種類の効果の状態（0=そのまま, 1=無効, 2=有効）。
fn effect_state(rule: &TagRule, role: RuleRole) -> u8 {
    if rule.effect.disable.contains(&role) {
        1
    } else if rule.effect.enable.contains(&role) {
        2
    } else {
        0
    }
}

/// 種類の効果を変える。無効と有効の両方にはならない。「そのまま」は、無効だけを外す
/// （効かない「有効」の指定が JSON にあっても、そのまま保持する）。
fn set_effect_state(rule: &mut TagRule, role: RuleRole, state: u8) {
    match state {
        1 => {
            rule.effect.enable.remove(&role);
            rule.effect.disable.insert(role);
        }
        2 => {
            rule.effect.disable.remove(&role);
            rule.effect.enable.insert(role);
        }
        _ => {
            rule.effect.disable.remove(&role);
            if role.is_opt_in() {
                rule.effect.enable.remove(&role);
            }
        }
    }
}

/// 条件の1行の説明（ためし判定で、当たった条件を示す）。
fn describe_condition(c: &Condition) -> String {
    let t = i18n::t();
    match c {
        Condition::Text { text, how, .. } => t.tag_rules_describe_text(text, how_label(*how)),
        Condition::Shape { shape, .. } => t.tag_rules_describe_shape(shape_label(*shape)),
    }
}

/// ためし判定の入力から、評価の入力を作る。ファイル名が空なら None。
fn trial_input(ed: &RuleEditor) -> Option<RuleInput> {
    let file = ed.trial_file.trim();
    if file.is_empty() {
        return None;
    }
    let titles = ed.trial_titles.trim();
    let category = ed.trial_category.trim();
    let info = if titles.is_empty() && category.is_empty() {
        None
    } else {
        let mut text = titles.to_string();
        if !category.is_empty() {
            text.push_str(&format!("\n\nCategory: {category}"));
        }
        Some(crate::tag_source::parse_info_txt(&text))
    };
    Some(RuleInput::new(file, info.as_ref()))
}

/// ③結果: 種類ごとに、当てはまったときの効果を選ぶ。
fn draw_effects_step(ui: &mut egui::Ui, ed: &mut RuleEditor) {
    let t = i18n::t();
    ui.strong(t.tag_rules_effect_heading());
    ui.weak(t.tag_rules_effect_hint());
    let groups: [(&str, Vec<RuleRole>, bool); 2] = [
        (t.tag_rules_effect_group_existing(), RuleRole::EXISTING.iter().chain(RuleRole::FROM_AUTHORITY.iter()).copied().collect(), false),
        (t.tag_rules_effect_group_new(), RuleRole::OPT_IN.to_vec(), true),
    ];
    for (caption, roles, opt_in) in groups {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(caption).weak().strong());
        for role in roles {
            ui.horizontal(|ui| {
                ui.label(role_label(role));
                let mut state = effect_state(&ed.draft, role);
                let before = state;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 右から並べる（見た目は「そのまま｜有効｜無効」の順に、左から）。
                    if opt_in {
                        ui.selectable_value(&mut state, 1, t.tag_rules_eff_disable());
                        ui.selectable_value(&mut state, 2, t.tag_rules_eff_enable());
                    } else {
                        ui.selectable_value(&mut state, 1, t.tag_rules_eff_disable());
                    }
                    ui.selectable_value(&mut state, 0, t.tag_rules_eff_none());
                });
                // 「有効」を持つ種類（演者など）以外では、2択のまま。JSON由来の効かない「有効」は、触れずに保持する。
                if state != before {
                    set_effect_state(&mut ed.draft, role, state);
                    ed.dirty = true;
                }
            });
        }
        if opt_in {
            ui.weak(t.tag_rules_effect_new_hint());
        }
    }
}

/// ④確認: ルールのまとめと、ためし判定。
fn draw_review_step(ui: &mut egui::Ui, ed: &mut RuleEditor, opts: &MatchOptions) {
    let t = i18n::t();
    ui.strong(t.tag_rules_review_heading());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let name = ed.draft.name.trim();
        ui.label(egui::RichText::new(if name.is_empty() { "…" } else { name }).strong());
        ui.weak(summary(&ed.draft));
    });
    ui.add_space(4.0);
    ui.strong(t.tag_rules_trial_heading());
    ui.weak(t.tag_rules_trial_hint());
    ui.add(egui::TextEdit::singleline(&mut ed.trial_file).hint_text(t.tag_rules_trial_file_hint()).desired_width(f32::INFINITY));
    ui.add(egui::TextEdit::multiline(&mut ed.trial_titles).hint_text(t.tag_rules_trial_titles_hint()).desired_rows(2).desired_width(f32::INFINITY));
    ui.add(egui::TextEdit::singleline(&mut ed.trial_category).hint_text(t.tag_rules_trial_category_hint()).desired_width(f32::INFINITY));
    let Some(input) = trial_input(ed) else {
        ui.weak(t.tag_rules_trial_need_file());
        return;
    };
    // 無効のルールでも、条件に当てはまるかは試せる（効果の表示は、有効にしたと仮定して出す）。
    let mut rule = ed.draft.clone();
    rule.enabled = true;
    let hit = rule.applies_to(&input, opts);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (text, color) = if hit {
            (t.tag_rules_trial_hit(), egui::Color32::from_rgb(70, 170, 100))
        } else {
            (t.tag_rules_trial_miss(), ui.visuals().weak_text_color())
        };
        ui.label(egui::RichText::new(text).strong().color(color));
        for c in &rule.conditions {
            let ok = c.matches(&input, opts);
            ui.label(format!("{} {}", if ok { "✔" } else { "✖" }, describe_condition(c)));
        }
        if hit {
            let join = |set: &std::collections::BTreeSet<RuleRole>| set.iter().map(|r| role_label(*r)).collect::<Vec<_>>().join("・");
            if rule.effect.disable.is_empty() && rule.effect.enable.is_empty() {
                ui.weak(t.tag_rules_trial_no_effect());
            }
            if !rule.effect.disable.is_empty() {
                ui.label(t.tag_rules_trial_effect_disable(&join(&rule.effect.disable)));
            }
            if !rule.effect.enable.is_empty() {
                ui.label(t.tag_rules_trial_effect_enable(&join(&rule.effect.enable)));
            }
        }
        if !ed.draft.enabled {
            ui.weak(t.tag_rules_trial_disabled_note());
        }
    });
}

/// 編集画面（ステップ式）を描く。
fn draw_rule_editor(ui: &mut egui::Ui, ed: &mut RuleEditor, errors: &[(usize, &'static str)], opts: &MatchOptions) -> EditorOut {
    let t = i18n::t();
    let red = egui::Color32::from_rgb(230, 90, 80);
    let mut out = EditorOut::default();

    // 未保存のまま離れようとしたときの確認。
    if ed.leave_ask.is_some() {
        egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.0, red)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(red, t.tag_rules_leave_ask());
            ui.horizontal(|ui| {
                if ui.button(egui::RichText::new(t.tag_rules_leave_discard()).color(red)).clicked() {
                    out.discard = true;
                }
                if ui.button(t.tag_rules_leave_stay()).clicked() {
                    ed.leave_ask = None;
                }
            });
        });
    }

    // ステップ表示。誤りのあるステップには、赤い印を付ける。③④は次の単位で作る。
    ui.horizontal(|ui| {
        for i in 0..STEPS {
            let bad = errors.iter().any(|(s, _)| *s == i);
            let label = format!("{}{}", if bad { "❗ " } else { "" }, t.tag_rules_step_name(i));
            let text = if bad { egui::RichText::new(label).color(red) } else { egui::RichText::new(label) };
            if i < READY_STEPS {
                if ui.selectable_label(ed.step == i, text).clicked() {
                    ed.step = i;
                }
            } else {
                ui.add_enabled(false, egui::Button::new(text)).on_disabled_hover_text(t.tag_rules_step_pending_hover());
            }
        }
    });
    ui.separator();

    let step_errors: Vec<&'static str> = errors.iter().filter(|(s, _)| *s == ed.step).map(|(_, e)| *e).collect();
    egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
        if ed.step == 0 {
            ui.strong(t.tag_rules_name_heading());
            ui.weak(t.tag_rules_name_hint());
            if ui.add(egui::TextEdit::singleline(&mut ed.draft.name).desired_width(f32::INFINITY)).changed() {
                ed.dirty = true;
            }
        } else if ed.step == 1 {
            ui.strong(t.tag_rules_cond_heading());
            ui.weak(t.tag_rules_cond_hint());
            let mut remove: Option<usize> = None;
            for (i, c) in ed.draft.conditions.iter_mut().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        match c {
                            Condition::Text { text, how, .. } => {
                                if ui.add(egui::TextEdit::singleline(text).hint_text(t.tag_rules_text_hint()).desired_width(220.0)).changed() {
                                    ed.dirty = true;
                                }
                                egui::ComboBox::from_id_salt(("tag_rule_how", i)).selected_text(how_label(*how)).show_ui(ui, |ui| {
                                    for h in [TextMatch::Contains, TextMatch::Prefix, TextMatch::Exact] {
                                        if ui.selectable_value(how, h, how_label(h)).changed() {
                                            ed.dirty = true;
                                        }
                                    }
                                });
                            }
                            Condition::Shape { shape, .. } => {
                                ui.label(t.tag_rules_shape_prefix());
                                egui::ComboBox::from_id_salt(("tag_rule_shape", i)).selected_text(shape_label(*shape)).show_ui(ui, |ui| {
                                    for sh in [NameShape::Dash2, NameShape::Dash2Paren, NameShape::Dash3] {
                                        if ui.selectable_value(shape, sh, shape_label(sh)).changed() {
                                            ed.dirty = true;
                                        }
                                    }
                                });
                                ui.label(t.tag_rules_shape_suffix());
                            }
                        }
                        if ui.button(t.tag_rules_delete_button()).clicked() {
                            remove = Some(i);
                        }
                    });
                    // 対象を限定した条件（JSONで作ったもの）: 表示だけ。限定の変更はできず、解除だけできる。
                    let targets = match c {
                        Condition::Text { targets, .. } | Condition::Shape { targets, .. } => targets,
                    };
                    if !is_all_targets(targets) {
                        ui.horizontal(|ui| {
                            let names: Vec<&str> = targets.iter().map(|x| target_label(*x)).collect();
                            ui.weak(t.tag_rules_limited(&names.join("・")));
                            if ui.small_button(t.tag_rules_unlimit()).clicked() {
                                *targets = ALL_TARGETS.to_vec();
                                ed.dirty = true;
                            }
                        });
                    }
                });
            }
            if let Some(i) = remove {
                ed.draft.conditions.remove(i);
                ed.dirty = true;
            }
            ui.horizontal(|ui| {
                if ui.button(t.tag_rules_add_text()).clicked() {
                    ed.draft.conditions.push(Condition::Text { targets: ALL_TARGETS.to_vec(), how: TextMatch::Contains, text: String::new() });
                    ed.dirty = true;
                }
                if ui.button(t.tag_rules_add_shape()).clicked() {
                    ed.draft.conditions.push(Condition::Shape { targets: ALL_TARGETS.to_vec(), shape: NameShape::Dash2Paren });
                    ed.dirty = true;
                }
            });
            if ed.draft.conditions.len() > 1 {
                ui.horizontal(|ui| {
                    ui.weak(t.tag_rules_mode_label());
                    if ui.radio_value(&mut ed.draft.match_all, false, t.tag_rules_mode_any()).changed() {
                        ed.dirty = true;
                    }
                    if ui.radio_value(&mut ed.draft.match_all, true, t.tag_rules_mode_all()).changed() {
                        ed.dirty = true;
                    }
                });
            }
        } else if ed.step == 2 {
            draw_effects_step(ui, ed);
        } else {
            draw_review_step(ui, ed, opts);
        }
        for e in &step_errors {
            ui.colored_label(red, *e);
        }
    });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button(t.tag_rules_back_to_list()).clicked() {
            out.back_to_list = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 最後のステップ（④確認）で保存する。
            if ed.step + 1 >= READY_STEPS {
                let ok = errors.is_empty();
                let btn = ui.add_enabled(ok, egui::Button::new(t.tag_rules_save_and_back()));
                let btn = if ok { btn } else { btn.on_disabled_hover_text(t.tag_rules_save_blocked_hover()) };
                if btn.clicked() {
                    out.save = true;
                }
            } else if ui.button(t.tag_rules_next()).clicked() {
                ed.step += 1;
            }
            if ui.add_enabled(ed.step > 0, egui::Button::new(t.tag_rules_prev())).clicked() {
                ed.step -= 1;
            }
        });
    });
    out
}

/// 一覧の操作（描画中は状態を変えられないので、あとでまとめて行う）。
enum RuleAction {
    /// 有効/無効を切り替える。
    Toggle(u64),
    /// 削除の確認を出す／やめる。
    AskDelete(Option<u64>),
    /// 削除する。
    Delete(u64),
    /// コピー元のルールをコピーして、新しいルールを足し、編集画面を開く。
    Copy,
    /// 編集画面を開く。
    Edit(u64),
    /// 空のルールを作る編集画面を開く。
    New,
    /// 無い初期ルールを、無効で足す。
    RestoreInitial,
    /// 設定フォルダのルールファイルを読み直す。
    Reload,
}

/// コピーの名前: 「元の名前 のコピー」。同じ名前があれば「 のコピー 2」「 のコピー 3」…にする。
fn unique_copy_name(set: &crate::tag_rules::RuleSet, base: &str, suffix: &str) -> String {
    let first = format!("{base}{suffix}");
    if !set.rules.iter().any(|r| r.name == first) {
        return first;
    }
    (2..).map(|n| format!("{first} {n}")).find(|name| !set.rules.iter().any(|r| &r.name == name)).unwrap_or(first)
}

impl NekoviewApp {
    /// ルール一式を差し替えて保存する。保存できなかったら、いまのルールのまま残して、トーストで知らせる。成否を返す。
    fn commit_tag_rules(&mut self, new: crate::tag_rules::RuleSet) -> bool {
        if new.save(&self.config.config_root) {
            self.tag_rules = new;
            true
        } else {
            self.set_toast(i18n::t().tag_rules_save_failed());
            false
        }
    }

    /// 下書きを保存する（新規は末尾に足し、既存は置き換える）。保存できたら true。
    fn save_rule_draft(&mut self, ed: &RuleEditor) -> bool {
        let mut new = self.tag_rules.clone();
        let mut rule = ed.draft.clone();
        rule.name = rule.name.trim().to_string();
        let ok = if ed.is_new { new.add(rule).is_ok() } else { new.update(rule).is_ok() };
        if !ok {
            self.set_toast(i18n::t().tag_rules_save_failed());
            return false;
        }
        self.commit_tag_rules(new)
    }

    /// 「タグ抽出ルール定義」窓。一覧と、編集画面（ステップ式）を、同じ窓の中で切り替える。
    pub(super) fn draw_tag_rules_window(&mut self, ctx: &egui::Context) {
        if !self.tag_rules_open {
            return;
        }
        let t = i18n::t();
        let mut open = true;
        let mut action: Option<RuleAction> = None;
        let mut editor = self.tag_rules_editor.take();
        let mut editor_out = EditorOut::default();
        let opts = self.config.tag_match;
        let center = self.explorer_center(ctx);
        let warn = egui::Color32::from_rgb(230, 90, 80);
        // 編集中のルール以外の名前（同名の誤りの判定に使う）。
        let other_names: Vec<String> = match &editor {
            Some(ed) => self.tag_rules.rules.iter().filter(|r| ed.is_new || r.id != ed.draft.id).map(|r| r.name.clone()).collect(),
            None => Vec::new(),
        };
        // コピー元は、選んでいなければ先頭。消えていたら先頭に戻す。
        let copy_src = self.tag_rules_copy_src.filter(|id| self.tag_rules.rules.iter().any(|r| r.id == *id)).or_else(|| self.tag_rules.rules.first().map(|r| r.id));
        let mut copy_pick = copy_src;
        let title = match &editor {
            Some(ed) => format!("{} — {}", t.tag_rules_window_title(), if ed.draft.name.trim().is_empty() { "…" } else { ed.draft.name.trim() }),
            None => t.tag_rules_window_title().to_string(),
        };
        egui::Window::new(title)
            .id(egui::Id::new("tag_rules_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0, 460.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .show(ctx, |ui| {
                if let Some(ed) = editor.as_mut() {
                    let errors = ed.errors(&other_names);
                    editor_out = draw_rule_editor(ui, ed, &errors, &opts);
                    return;
                }
                ui.weak(t.tag_rules_list_hint());
                ui.add_space(4.0);
                if self.tag_rules.rules.is_empty() {
                    ui.label(t.tag_rules_empty());
                }
                egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
                    for rule in &self.tag_rules.rules {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let name = egui::RichText::new(&rule.name);
                                ui.label(if rule.enabled { name.strong() } else { name.weak() });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if self.tag_rules_del_ask == Some(rule.id) {
                                        if ui.button(t.tag_rules_delete_cancel()).clicked() {
                                            action = Some(RuleAction::AskDelete(None));
                                        }
                                        if ui.button(egui::RichText::new(t.tag_rules_delete_do()).color(warn)).clicked() {
                                            action = Some(RuleAction::Delete(rule.id));
                                        }
                                        ui.colored_label(warn, t.tag_rules_delete_ask());
                                    } else {
                                        if ui.button(egui::RichText::new(t.tag_rules_delete_button()).color(warn)).clicked() {
                                            action = Some(RuleAction::AskDelete(Some(rule.id)));
                                        }
                                        if ui.button(t.tag_rules_edit_button()).clicked() {
                                            action = Some(RuleAction::Edit(rule.id));
                                        }
                                        let on = rule.enabled;
                                        if ui.add(egui::Button::selectable(on, t.tag_rules_toggle(on))).clicked() {
                                            action = Some(RuleAction::Toggle(rule.id));
                                        }
                                    }
                                });
                            });
                            ui.weak(summary(rule));
                        });
                    }
                });
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong(t.tag_rules_copy_heading());
                    ui.horizontal(|ui| {
                        let selected = self.tag_rules.rules.iter().find(|r| Some(r.id) == copy_pick).map(|r| r.name.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("tag_rules_copy_src").selected_text(selected).width(280.0).show_ui(ui, |ui| {
                            for r in &self.tag_rules.rules {
                                ui.selectable_value(&mut copy_pick, Some(r.id), &r.name);
                            }
                        });
                        let can_copy = copy_pick.is_some();
                        if ui.add_enabled(can_copy, egui::Button::new(t.tag_rules_copy_button())).clicked() {
                            action = Some(RuleAction::Copy);
                        }
                    });
                    ui.weak(t.tag_rules_copy_hint());
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(t.tag_rules_new_button()).clicked() {
                        action = Some(RuleAction::New);
                    }
                    if ui.button(t.tag_rules_restore_button()).on_hover_text(t.tag_rules_restore_hover()).clicked() {
                        action = Some(RuleAction::RestoreInitial);
                    }
                    if ui.button(t.tag_rules_reload_button()).on_hover_text(t.tag_rules_reload_hover()).clicked() {
                        action = Some(RuleAction::Reload);
                    }
                });
            });
        self.tag_rules_copy_src = copy_pick;

        // 編集画面の結果。
        if let Some(ed) = editor.as_mut() {
            if editor_out.back_to_list {
                if ed.dirty {
                    ed.leave_ask = Some(Leave::ToList);
                } else {
                    editor = None;
                }
            } else if editor_out.save {
                let ed_ref = editor.as_ref().expect("editor");
                if self.save_rule_draft(ed_ref) {
                    self.set_toast(t.tag_rules_saved());
                    editor = None;
                }
            } else if editor_out.discard {
                if ed.leave_ask == Some(Leave::CloseWindow) {
                    open = false;
                }
                editor = None;
            }
        }
        // 窓の × は、編集中に未保存の変更があれば、確認を出して開いたままにする。
        if !open {
            if let Some(ed) = editor.as_mut() {
                if ed.dirty && ed.leave_ask.is_none() {
                    ed.leave_ask = Some(Leave::CloseWindow);
                    open = true;
                }
            }
        }
        self.tag_rules_editor = editor;
        if let Some(a) = action {
            self.apply_rule_action(a);
        }
        if !open {
            self.tag_rules_open = false;
            self.tag_rules_del_ask = None;
            self.tag_rules_editor = None;
        }
    }

    fn apply_rule_action(&mut self, action: RuleAction) {
        let t = i18n::t();
        match action {
            RuleAction::AskDelete(id) => self.tag_rules_del_ask = id,
            RuleAction::Toggle(id) => {
                let mut new = self.tag_rules.clone();
                let Some(rule) = new.rules.iter().find(|r| r.id == id).cloned() else { return };
                let mut rule = rule;
                rule.enabled = !rule.enabled;
                // 有効にするには効果が要る（効果が空のひな形は、無効のまま）。
                if new.update(rule).is_err() {
                    self.set_toast(t.tag_rules_enable_needs_effect());
                    return;
                }
                self.commit_tag_rules(new);
            }
            RuleAction::Delete(id) => {
                let mut new = self.tag_rules.clone();
                new.remove(id);
                if self.commit_tag_rules(new) {
                    self.tag_rules_del_ask = None;
                }
            }
            RuleAction::Edit(id) => {
                if let Some(rule) = self.tag_rules.rules.iter().find(|r| r.id == id).cloned() {
                    self.tag_rules_del_ask = None;
                    self.tag_rules_editor = Some(RuleEditor::edit(rule));
                }
            }
            RuleAction::New => {
                let base = t.tag_rules_new_name();
                let mut name = base.to_string();
                let mut n = 2;
                while self.tag_rules.rules.iter().any(|r| r.name == name) {
                    name = format!("{base} {n}");
                    n += 1;
                }
                self.tag_rules_del_ask = None;
                self.tag_rules_editor = Some(RuleEditor::blank(name));
            }
            RuleAction::Copy => {
                let Some(src) = self.tag_rules_copy_src.and_then(|id| self.tag_rules.rules.iter().find(|r| r.id == id)).cloned() else { return };
                let mut new = self.tag_rules.clone();
                let mut copy = src.clone();
                copy.name = unique_copy_name(&new, &src.name, t.tag_rules_copy_suffix());
                let Ok(id) = new.add(copy) else { return };
                if self.commit_tag_rules(new) {
                    self.set_toast(t.tag_rules_copied(&src.name));
                    // 「コピーして編集」: コピーを、そのまま編集画面で開く。
                    if let Some(rule) = self.tag_rules.rules.iter().find(|r| r.id == id).cloned() {
                        self.tag_rules_del_ask = None;
                        self.tag_rules_editor = Some(RuleEditor::edit(rule));
                    }
                }
            }
            RuleAction::RestoreInitial => {
                let mut new = self.tag_rules.clone();
                let n = new.add_missing_initial();
                if n == 0 {
                    self.set_toast(t.tag_rules_restore_none());
                } else if self.commit_tag_rules(new) {
                    self.set_toast(t.tag_rules_restored(n));
                }
            }
            RuleAction::Reload => match crate::tag_rules::RuleSet::load(&self.config.config_root) {
                Some(set) => {
                    let n = set.rules.len();
                    self.tag_rules = set;
                    self.tag_rules_del_ask = None;
                    self.set_toast(t.tag_rules_reloaded(n));
                }
                None if crate::tag_rules::RuleSet::file_exists(&self.config.config_root) => self.set_toast(t.tag_rules_reload_failed()),
                None => {
                    self.tag_rules = crate::tag_rules::RuleSet::default();
                    self.set_toast(t.tag_rules_reloaded(0));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_rules::Preset;

    #[test]
    fn summary_lists_conditions_and_effects() {
        let s = summary(&Preset::Cosplay.to_rule(1));
        assert!(s.contains("無効") && s.contains("有効"), "{s}");
        assert!(s.contains("作者") && s.contains("演者"), "{s}");
    }

    #[test]
    fn editor_reports_errors_per_step() {
        let mut ed = RuleEditor::blank("新しいルール".into());
        // 空のルール: 条件の文字列が空（②）。
        assert_eq!(ed.errors(&[]).iter().map(|e| e.0).collect::<Vec<_>>(), vec![1]);
        if let Condition::Text { text, .. } = &mut ed.draft.conditions[0] {
            *text = "AI".into();
        }
        assert!(ed.errors(&[]).is_empty());
        // 同名のルールがある（①）。前後の空白は無視する。
        assert_eq!(ed.errors(&[" 新しいルール ".to_string()]).iter().map(|e| e.0).collect::<Vec<_>>(), vec![0]);
        ed.draft.name = "  ".into();
        assert_eq!(ed.errors(&[]).iter().map(|e| e.0).collect::<Vec<_>>(), vec![0]);
        // 条件が1つも無い（②）。
        ed.draft.name = "x".into();
        ed.draft.conditions.clear();
        assert_eq!(ed.errors(&[]).iter().map(|e| e.0).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn blank_rule_is_saved_disabled_and_limited_targets_can_be_released() {
        let mut set = crate::tag_rules::RuleSet::default();
        let mut ed = RuleEditor::blank("新しいルール".into());
        if let Condition::Text { text, .. } = &mut ed.draft.conditions[0] {
            *text = "cosplay".into();
        }
        assert!(ed.errors(&[]).is_empty() && ed.is_new && !ed.draft.enabled);
        set.add(ed.draft.clone()).expect("効果が空でも、無効なら保存できる");
        // 対象を限定した条件（初期ルールの doujinshi）は、限定の判定ができ、解除すると全対象になる。
        let mut c = Preset::Doujinshi.to_rule(1).conditions.remove(0);
        let Condition::Text { targets, .. } = &mut c else { panic!("文字列の条件") };
        assert!(!is_all_targets(targets));
        *targets = ALL_TARGETS.to_vec();
        assert!(is_all_targets(targets));
        // AI generated は4つの対象すべてを見る（限定なし）。
        let ai = Preset::AiGenerated.to_rule(1);
        assert!(matches!(&ai.conditions[0], Condition::Text { targets, .. } if is_all_targets(targets)));
    }

    #[test]
    fn effect_choices_keep_the_two_sets_exclusive_and_preserve_inert_enables() {
        let mut r = Preset::Cosplay.to_rule(1);
        // 演者: 有効 → 無効 → そのまま。無効と有効の両方にはならない。
        assert_eq!(effect_state(&r, RuleRole::Performer), 2);
        set_effect_state(&mut r, RuleRole::Performer, 1);
        assert!(r.effect.disable.contains(&RuleRole::Performer) && !r.effect.enable.contains(&RuleRole::Performer));
        set_effect_state(&mut r, RuleRole::Performer, 0);
        assert_eq!(effect_state(&r, RuleRole::Performer), 0);
        // 作者: 無効 → そのまま。
        assert_eq!(effect_state(&r, RuleRole::Author), 1);
        set_effect_state(&mut r, RuleRole::Author, 0);
        assert_eq!(effect_state(&r, RuleRole::Author), 0);
        // 従来の種類に JSON で入っていた「有効」は、「そのまま」にしても消えない（効かないが、触らない）。
        r.effect.enable.insert(RuleRole::Group);
        set_effect_state(&mut r, RuleRole::Group, 0);
        assert!(r.effect.enable.contains(&RuleRole::Group));
        // 無効にすると、有効の側からは外れる（両方にはならない）。
        set_effect_state(&mut r, RuleRole::Group, 1);
        assert!(!r.effect.enable.contains(&RuleRole::Group) && r.effect.disable.intersection(&r.effect.enable).next().is_none());
    }

    #[test]
    fn enabled_rule_without_effect_is_an_error_at_step_three() {
        let mut manga = Preset::Manga.to_rule(1);
        manga.enabled = false;
        let mut ed = RuleEditor::edit(manga);
        assert!(ed.errors(&[]).is_empty(), "無効のルールは、効果が空でもよい");
        ed.draft.enabled = true;
        assert_eq!(ed.errors(&[]).iter().map(|e| e.0).collect::<Vec<_>>(), vec![2]);
        set_effect_state(&mut ed.draft, RuleRole::Author, 1);
        assert!(ed.errors(&[]).is_empty());
    }

    #[test]
    fn trial_judgement_uses_file_name_info_titles_and_category() {
        let opts = MatchOptions::default();
        let mut ed = RuleEditor::edit(Preset::AiGenerated.to_rule(1));
        assert!(trial_input(&ed).is_none(), "ファイル名が空なら、判定しない");
        ed.trial_file = "[Circle (Author)] Book [AI Generated].zip".into();
        let input = trial_input(&ed).unwrap();
        assert!(ed.draft.applies_to(&input, &opts));
        assert!(describe_condition(&ed.draft.conditions[0]).contains("AI generated"));
        // 当てはまらないファイル名。
        ed.trial_file = "[Circle (Author)] Book.zip".into();
        assert!(!ed.draft.applies_to(&trial_input(&ed).unwrap(), &opts));
        // info.txt の見出し行で当てはまる。
        ed.trial_titles = "Some Book [AI Generated]".into();
        assert!(ed.draft.applies_to(&trial_input(&ed).unwrap(), &opts));
        // Category だけを見る初期ルール（doujinshi）は、Category を入れたときだけ当てはまる。
        let mut dj = RuleEditor::edit(Preset::Doujinshi.to_rule(2));
        dj.trial_file = "x.zip".into();
        assert!(!dj.draft.applies_to(&trial_input(&dj).unwrap(), &opts));
        dj.trial_category = "Doujinshi".into();
        assert!(dj.draft.applies_to(&trial_input(&dj).unwrap(), &opts));
    }

    #[test]
    fn copy_names_are_numbered_when_taken() {
        let mut set = crate::tag_rules::RuleSet::default();
        set.add(Preset::Cosplay.to_rule(0)).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー");
        let mut c1 = Preset::Cosplay.to_rule(0);
        c1.name = "cosplay のコピー".into();
        set.add(c1).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー 2");
        let mut c2 = Preset::Cosplay.to_rule(0);
        c2.name = "cosplay のコピー 2".into();
        set.add(c2).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー 3");
    }

    #[test]
    fn every_role_has_a_label() {
        for r in RuleRole::ALL {
            assert!(!role_label(r).is_empty(), "{r:?}");
        }
    }
}
