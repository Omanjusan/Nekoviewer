//! タグ自動取得の学習結果（`tag_learning` / `tag_model`）の保持と、タグパネルの状態表示。
//!
//! - 学習データは起動時に設定フォルダから読み、モデルを作る（学習データが空ならモデルは無し）
//! - 「学習結果を使う」が OFF のとき、または学習データが無いときは、候補生成にモデルを渡さない（現行ルールのみ）
//! - タグ学習窓（件数の選択・学習を始める・目安の一致率）と、ラベル窓（ファイル名の括弧ごとの種別を確認して学習する）

use std::sync::Arc;



use std::collections::HashSet;
use std::path::PathBuf;

use crate::i18n;
use crate::tag_corpus::Label;
use crate::tag_features::{tokenize, Token};
use crate::tag_learning::{entry_from_labels, labels_from_entry, select_samples, suggest_labels, LearnedSet};
use crate::tag_model::Model;
use crate::tag_similarity::MatchOptions;

use super::NekoviewApp;

/// 設定フォルダの学習データを読む。無い・壊れているときは空。
pub(super) fn load_learned(root: &std::path::Path) -> LearnedSet {
    LearnedSet::load(root).unwrap_or_default()
}

/// 学習データからモデルを作る（空なら None）。
pub(super) fn train_model(set: &LearnedSet, opts: &MatchOptions) -> Option<Arc<Model>> {
    Model::train(&set.training_data(opts)).map(Arc::new)
}

impl NekoviewApp {
    /// 候補生成に渡すモデル。学習結果を使わない設定のとき、学習データが無いときは None（現行ルールのみ）。
    pub(super) fn active_tag_model(&self) -> Option<Arc<Model>> {
        if self.config.tag_learning_use {
            self.tag_model.clone()
        } else {
            None
        }
    }

    /// 学習データが変わった（追加・削除）あとに、モデルを作り直す。
    pub(super) fn rebuild_tag_model(&mut self) {
        self.tag_model = train_model(&self.tag_learned, &self.config.tag_match);
    }

    /// 「タグ管理」メニューの「現フォルダに対するタグ抽出用学習…」から呼ぶ。学習窓が開いていなければ開く。
    pub(super) fn open_tag_learn_window(&mut self) {
        if self.tag_learn.is_none() {
            self.tag_learn = Some(TagLearn::open(self));
        }
    }

    /// 学習の元にするファイル名（現在のフォルダの対象）。お気に入り・検索・類似・タグ検索の表示中は空。
    fn tag_learn_source_names(&self) -> Vec<String> {
        if self.viewing_favorites.is_some()
            || self.viewing_similar.is_some()
            || self.viewing_search.is_some()
            || self.folder_pane_tab == super::FolderPaneTab::TagSearch
        {
            return Vec::new();
        }
        self.tag_scan_targets().iter().filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string)).collect()
    }

    /// 一括取得ボタンから呼ぶ。学習データが無く、案内を止めていないときは、先に学習の案内を出す。
    pub(super) fn request_tag_scan(&mut self) {
        if self.tag_learned.is_empty() && !self.config.tag_learn_intro_hidden && self.tag_learn.is_none() {
            let mut st = TagLearn::open(self);
            st.stage = LearnStage::Intro;
            self.tag_learn = Some(st);
            return;
        }
        self.start_tag_scan();
    }

    /// ファイルの右クリックメニュー: 「タグ学習に追加…」。このファイル1件のラベル窓を開く。
    pub(super) fn draw_tag_learn_menu_item(&mut self, ui: &mut egui::Ui, hover: &std::path::Path) {
        let t = i18n::t();
        let Some(name) = hover.file_name().and_then(|n| n.to_str()).map(str::to_string) else { return };
        let learnable = !tokenize(&name).is_empty();
        let resp = ui.add_enabled(learnable, egui::Button::new(t.tag_learn_add_menu()));
        let resp = if learnable { resp } else { resp.on_disabled_hover_text(t.tag_learn_add_menu_disabled()) };
        if resp.clicked() {
            if let Some(st) = TagLearn::single(self, &name) {
                self.tag_learn = Some(st);
            }
            ui.close();
        }
    }

    /// タグ学習窓（案内・ハブ・ラベル窓・学習データ閲覧窓）。
    pub(super) fn draw_tag_learn(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.tag_learn.take() else { return };
        let t = i18n::t();
        // フォルダが変わったら、元のファイル名を取り直す（ハブのときだけ）。
        if st.dir != self.current_dir && matches!(st.stage, LearnStage::Hub | LearnStage::Intro) {
            st.refresh(self);
        }
        // 目安の一致率（交差検証）。学習データが変わったときだけ計算する。
        let learned_len = self.tag_learned.len();
        if matches!(st.stage, LearnStage::Hub) && st.estimate.map(|e| e.0) != Some(learned_len) {
            let percent = if learned_len >= 10 {
                crate::tag_eval::k_fold(&self.tag_learned.entries, 5, &self.config.tag_match, i18n::t())
                    .and_then(|h| h.model.exact_rate())
                    .map(|r| (r * 100.0).round() as u32)
            } else {
                None
            };
            st.estimate = Some((learned_len, percent));
        }

        let mut open = true;
        let (mut start, mut commit, mut cancel_labeling, mut skip_scan, mut to_viewer) = (false, false, false, false, false);
        let mut close_clicked = false;
        let mut clear_all = false;
        let mut remove_entry: Option<String> = None;
        let mut remove_label: Option<(String, usize)> = None;
        let mut use_learning = self.config.tag_learning_use;
        let mut hide_intro = self.config.tag_learn_intro_hidden;
        let title = match st.stage {
            LearnStage::Intro => t.tag_learn_intro_title(),
            LearnStage::Hub => t.tag_learn_window_title(),
            LearnStage::Labeling(_) => t.tag_learn_label_window_title(),
            LearnStage::Viewer => t.tag_learn_viewer_title(),
        };
        let resizable = matches!(st.stage, LearnStage::Labeling(_) | LearnStage::Viewer);
        let center = self.explorer_center(ctx);
        egui::Window::new(title)
            .id(egui::Id::new("tag_learn_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(resizable)
            .default_size([640.0, 520.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(center)
            .show(ctx, |ui| match &mut st.stage {
                LearnStage::Intro => {
                    ui.label(t.tag_learn_intro_body());
                    ui.add_space(4.0);
                    ui.label(t.tag_learn_count_heading());
                    for n in [10usize, 20, 30] {
                        ui.radio_value(&mut st.sample_count, n, t.tag_learn_count_label(n));
                    }
                    ui.weak(t.tag_learn_folder_info(st.available, st.sample_count.min(st.available)));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let resp = ui.add_enabled(st.available > 0, egui::Button::new(t.tag_learn_intro_start()));
                        let resp = if st.available > 0 { resp } else { resp.on_disabled_hover_text(t.tag_learn_start_disabled_hover()) };
                        start = resp.clicked();
                        skip_scan = ui.button(t.tag_learn_intro_skip()).clicked();
                    });
                    ui.checkbox(&mut hide_intro, t.tag_learn_intro_hide());
                }
                LearnStage::Hub => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(t.tag_learn_status(learned_len));
                        to_viewer = ui.add_enabled(learned_len > 0, egui::Button::new(t.tag_learn_view_button())).clicked();
                        if st.confirm_clear {
                            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_learn_clear_confirm());
                            if ui.button(t.tag_learn_clear_do()).clicked() {
                                clear_all = true;
                                st.confirm_clear = false;
                            }
                            if ui.button(t.tag_learn_clear_cancel()).clicked() {
                                st.confirm_clear = false;
                            }
                        } else if ui.add_enabled(learned_len > 0, egui::Button::new(t.tag_learn_clear_button())).clicked() {
                            st.confirm_clear = true;
                        }
                    });
                    match st.estimate.and_then(|e| e.1) {
                        Some(p) => ui.weak(t.tag_learn_estimate(p)),
                        None => ui.weak(t.tag_learn_estimate_none()),
                    };
                    ui.checkbox(&mut use_learning, t.tag_learn_use_checkbox());
                    ui.separator();
                    ui.label(t.tag_learn_count_heading());
                    for n in [10usize, 20, 30] {
                        ui.radio_value(&mut st.sample_count, n, t.tag_learn_count_label(n));
                    }
                    ui.weak(t.tag_learn_folder_info(st.available, st.sample_count.min(st.available)));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let resp = ui.add_enabled(st.available > 0, egui::Button::new(t.tag_learn_start_button()));
                        let resp = if st.available > 0 { resp } else { resp.on_disabled_hover_text(t.tag_learn_start_disabled_hover()) };
                        start = resp.clicked();
                        close_clicked = ui.button(t.tag_learn_close_button()).clicked();
                    });
                }
                LearnStage::Viewer => {
                    let entries = &self.tag_learned.entries;
                    if entries.is_empty() {
                        ui.label(t.tag_learn_viewer_empty());
                    }
                    let row_h = ui.text_style_height(&egui::TextStyle::Body) * 2.0 + ui.spacing().item_spacing.y * 3.0 + 6.0;
                    egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show_rows(ui, row_h, entries.len(), |ui, range| {
                        for i in range {
                            // 新しく足したものを上に出す。
                            let e = &entries[entries.len() - 1 - i];
                            ui.horizontal(|ui| {
                                ui.label(&e.file_name);
                                if ui.small_button(t.tag_learn_viewer_delete()).clicked() {
                                    remove_entry = Some(e.file_name.clone());
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                if e.labels.is_empty() {
                                    ui.weak(t.tag_learn_viewer_no_labels());
                                }
                                for (li, (label, text)) in e.labels.iter().enumerate() {
                                    let resp = ui.small_button(format!("{text} = {} ×", t.tag_learn_label_name(*label)));
                                    if resp.on_hover_text(t.tag_learn_viewer_remove_label_hover()).clicked() {
                                        remove_label = Some((e.file_name.clone(), li));
                                    }
                                }
                            });
                            ui.separator();
                        }
                    });
                }
                LearnStage::Labeling(rows) => {
                    ui.label(t.tag_learn_label_help());
                    let confirmed = rows.iter().filter(|r| r.confirmed).count();
                    ui.horizontal(|ui| {
                        ui.strong(t.tag_learn_confirmed_count(confirmed, rows.len()));
                        if ui.button(t.tag_learn_confirm_all()).clicked() {
                            rows.iter_mut().for_each(|r| r.confirmed = true);
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (ri, row) in rows.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut row.confirmed, "");
                                ui.label(&row.file_name);
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(26.0);
                                for (ti, (token, label)) in row.tokens.iter_mut().enumerate() {
                                    let before = *label;
                                    ui.strong(&token.text);
                                    egui::ComboBox::from_id_salt(("tag_learn_label", ri, ti))
                                        .selected_text(t.tag_learn_label_name(*label))
                                        .width(90.0)
                                        .show_ui(ui, |ui| {
                                            for l in [Label::Author, Label::Group, Label::Event, Label::Language, Label::Ignore] {
                                                ui.selectable_value(label, l, t.tag_learn_label_name(l));
                                            }
                                        });
                                    if *label != before {
                                        row.confirmed = true;
                                    }
                                    ui.add_space(10.0);
                                }
                            });
                            ui.separator();
                        }
                    });
                    ui.horizontal(|ui| {
                        let resp = ui.add_enabled(confirmed > 0, egui::Button::new(t.tag_learn_commit_button()));
                        let resp = if confirmed > 0 {
                            resp.on_hover_text(t.tag_learn_commit_hover(confirmed, rows.len()))
                        } else {
                            resp.on_disabled_hover_text(t.tag_learn_commit_disabled_hover())
                        };
                        commit = resp.clicked();
                        cancel_labeling = ui.button(t.tag_learn_cancel_button()).clicked();
                    });
                }
            });

        if use_learning != self.config.tag_learning_use || hide_intro != self.config.tag_learn_intro_hidden {
            self.config.tag_learning_use = use_learning;
            self.config.tag_learn_intro_hidden = hide_intro;
            self.persist_state();
        }
        // 学習データの編集（閲覧窓・全消去）。変わったら、保存してモデルを作り直す。
        let mut edited = false;
        if let Some((name, idx)) = remove_label {
            edited |= self.tag_learned.remove_label(&name, idx);
        }
        if let Some(name) = remove_entry {
            edited |= self.tag_learned.remove(&name);
        }
        if clear_all {
            self.tag_learned.clear();
            edited = true;
        }
        if edited {
            if !self.tag_learned.save(&self.config.config_root) {
                self.set_toast(t.tag_learn_save_failed_toast());
            }
            self.rebuild_tag_model();
            st.refresh(self);
        }
        if to_viewer {
            st.stage = LearnStage::Viewer;
        }
        if start {
            let rows = st.pick_rows(self);
            if rows.is_empty() {
                // 学習できるファイルが無い。案内から来たなら、そのまま取得へ。
                if matches!(st.stage, LearnStage::Intro) {
                    skip_scan = true;
                }
            } else {
                st.then_scan = matches!(st.stage, LearnStage::Intro);
                st.stage = LearnStage::Labeling(rows);
            }
        }
        if commit {
            if let LearnStage::Labeling(rows) = &st.stage {
                let entries: Vec<_> = rows.iter().filter(|r| r.confirmed).map(|r| entry_from_labels(&r.file_name, &r.tokens)).collect();
                let n = entries.len();
                for e in entries {
                    self.tag_learned.add(e);
                }
                let saved = self.tag_learned.save(&self.config.config_root);
                self.rebuild_tag_model();
                if saved {
                    self.set_toast(t.tag_learn_done_toast(n));
                } else {
                    self.set_toast(t.tag_learn_save_failed_toast());
                }
            }
            // 1件追加（右クリック）と、案内からの学習は、終わったら窓を閉じる。案内からのときは、続けて取得する。
            if st.single || st.then_scan {
                open = false;
                if st.then_scan {
                    self.start_tag_scan();
                }
            } else {
                st.stage = LearnStage::Hub;
                st.refresh(self);
            }
        }
        if cancel_labeling {
            if st.single || st.then_scan {
                open = false;
            } else {
                st.stage = LearnStage::Hub;
            }
        }
        if skip_scan {
            open = false;
            self.start_tag_scan();
        }
        if open && !close_clicked {
            self.tag_learn = Some(st);
        }
    }
}

/// タグ学習窓の状態。
pub(super) struct TagLearn {
    /// 1回に学習する件数（10 / 20 / 30）。
    sample_count: usize,
    stage: LearnStage,
    /// 目安の一致率（計算した時点の学習件数, 一致率の％）。学習データが変わったら取り直す。
    estimate: Option<(usize, Option<u32>)>,
    /// 元にしたフォルダと、そのファイル名（対象の全部）。
    dir: PathBuf,
    names: Vec<String>,
    /// 括弧があって未学習のファイルの数。
    available: usize,
    /// 右クリックからの1件追加（終わったら窓を閉じる）。
    single: bool,
    /// 案内からの学習（学習を確定したら、続けて一括取得を始める）。
    then_scan: bool,
    /// 全消去の確認中。
    confirm_clear: bool,
}

enum LearnStage {
    /// 学習データが無いまま一括取得を押したときの案内。
    Intro,
    Hub,
    Labeling(Vec<LabelRow>),
    /// 学習データの閲覧（ラベル単位・ファイル単位の削除）。
    Viewer,
}

struct LabelRow {
    file_name: String,
    tokens: Vec<(Token, Label)>,
    confirmed: bool,
}

impl TagLearn {
    fn open(app: &NekoviewApp) -> Self {
        let mut st = Self {
            sample_count: 20,
            stage: LearnStage::Hub,
            estimate: None,
            dir: PathBuf::new(),
            names: Vec::new(),
            available: 0,
            single: false,
            then_scan: false,
            confirm_clear: false,
        };
        st.refresh(app);
        st
    }

    /// 1件だけのラベル窓。学習済みなら、保存済みのラベルを初期値にする。括弧が無いファイル名は None。
    fn single(app: &NekoviewApp, file_name: &str) -> Option<Self> {
        let tokens = match app.tag_learned.entries.iter().find(|e| e.file_name == file_name) {
            Some(e) => labels_from_entry(e, &app.config.tag_match),
            None => suggest_labels(file_name, app.tag_model.as_deref(), &app.tag_manager_categories, &app.config.tag_match, i18n::t()),
        };
        if tokens.is_empty() {
            return None;
        }
        let mut st = Self::open(app);
        st.single = true;
        st.stage = LearnStage::Labeling(vec![LabelRow { file_name: file_name.to_string(), tokens, confirmed: false }]);
        Some(st)
    }

    /// 現在のフォルダのファイル名を取り直し、学習できる数を数える。
    fn refresh(&mut self, app: &NekoviewApp) {
        self.dir = app.current_dir.clone();
        self.names = app.tag_learn_source_names();
        let learned: HashSet<&str> = app.tag_learned.entries.iter().map(|e| e.file_name.as_str()).collect();
        self.available = self.names.iter().filter(|n| !learned.contains(n.as_str()) && !tokenize(n).is_empty()).count();
    }

    /// 学習する件数ぶんのファイルを選び、ラベル付けの行にする（初期値はモデルまたはルールの予測）。
    fn pick_rows(&self, app: &NekoviewApp) -> Vec<LabelRow> {
        let exclude: HashSet<String> = app.tag_learned.entries.iter().map(|e| e.file_name.clone()).collect();
        select_samples(&self.names, &exclude, self.sample_count)
            .into_iter()
            .map(|name| {
                let tokens = suggest_labels(&name, app.tag_model.as_deref(), &app.tag_manager_categories, &app.config.tag_match, i18n::t());
                LabelRow { file_name: name, tokens, confirmed: false }
            })
            .filter(|r| !r.tokens.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_corpus::{CorpusEntry, Label};

    fn set_of(names: &[(&str, &[(Label, &str)])]) -> LearnedSet {
        LearnedSet {
            version: 1,
            entries: names
                .iter()
                .map(|(n, l)| CorpusEntry { file_name: n.to_string(), labels: l.iter().map(|(a, b)| (*a, b.to_string())).collect(), hard: false })
                .collect(),
        }
    }

    #[test]
    fn empty_learned_set_gives_no_model() {
        assert!(train_model(&LearnedSet::default(), &MatchOptions::default()).is_none());
    }

    #[test]
    fn learned_set_gives_a_model() {
        let s = set_of(&[("[A (B)] T.zip", &[(Label::Group, "A"), (Label::Author, "B")])]);
        assert!(train_model(&s, &MatchOptions::default()).is_some());
    }

    #[test]
    fn missing_or_broken_file_loads_as_empty() {
        let d = std::env::temp_dir().join(format!("nekoviewer_learn_ui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(load_learned(&d).is_empty());
        std::fs::write(d.join("nekoviewer_tag_learning.json"), "{broken").unwrap();
        assert!(load_learned(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
