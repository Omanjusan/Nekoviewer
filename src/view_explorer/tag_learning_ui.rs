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
use crate::tag_learning::{entry_from_labels, select_samples, suggest_labels, LearnedSet};
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

    /// タグパネルの学習行: 「タグ学習…」ボタン、学習済みの件数、学習結果を使う/使わないの切替。
    pub(super) fn draw_tag_learning_status(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        let n = self.tag_learned.len();
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                if ui.small_button(t.tag_learn_open_button()).clicked() && self.tag_learn.is_none() {
                    self.tag_learn = Some(TagLearn::open(self));
                }
                ui.weak(t.tag_learn_status(n)).on_hover_text(t.tag_learn_status_hover());
                // 使う/使わない。学習データが無いときは切り替えても結果が変わらないので、押せなくする。
                let on = self.config.tag_learning_use;
                let resp = ui.add_enabled(n > 0, egui::Button::selectable(on, t.tag_learn_use_toggle(on)).small());
                let resp = if n > 0 { resp.on_hover_text(t.tag_learn_use_hover()) } else { resp.on_disabled_hover_text(t.tag_learn_use_disabled_hover()) };
                if resp.clicked() {
                    self.config.tag_learning_use = !on;
                    self.persist_state();
                }
            });
        });
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

    /// タグ学習窓とラベル窓。
    pub(super) fn draw_tag_learn(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.tag_learn.take() else { return };
        let t = i18n::t();
        // フォルダが変わったら、元のファイル名を取り直す（ラベル付けの途中は触らない）。
        if st.dir != self.current_dir && matches!(st.stage, LearnStage::Hub) {
            st.refresh(self);
        }
        // 目安の一致率（交差検証）。学習データが変わったときだけ計算する。
        let learned_len = self.tag_learned.len();
        if st.estimate.map(|e| e.0) != Some(learned_len) {
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
        let (mut start, mut commit, mut cancel_labeling) = (false, false, false);
        let mut use_learning = self.config.tag_learning_use;
        let title = match st.stage {
            LearnStage::Hub => t.tag_learn_window_title(),
            LearnStage::Labeling(_) => t.tag_learn_label_window_title(),
        };
        let mut close_clicked = false;
        egui::Window::new(title)
            .id(egui::Id::new("tag_learn_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(matches!(st.stage, LearnStage::Labeling(_)))
            .default_size([640.0, 520.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match &mut st.stage {
                LearnStage::Hub => {
                    ui.label(t.tag_learn_status(learned_len));
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
                    let will = st.sample_count.min(st.available);
                    ui.weak(t.tag_learn_folder_info(st.available, will));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let resp = ui.add_enabled(st.available > 0, egui::Button::new(t.tag_learn_start_button()));
                        let resp = if st.available > 0 { resp } else { resp.on_disabled_hover_text(t.tag_learn_start_disabled_hover()) };
                        start = resp.clicked();
                        close_clicked = ui.button(t.tag_learn_close_button()).clicked();
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

        if use_learning != self.config.tag_learning_use {
            self.config.tag_learning_use = use_learning;
            self.persist_state();
        }
        if start {
            let exclude: HashSet<String> = self.tag_learned.entries.iter().map(|e| e.file_name.clone()).collect();
            let picked = select_samples(&st.names, &exclude, st.sample_count);
            let rows: Vec<LabelRow> = picked
                .into_iter()
                .map(|name| {
                    let tokens = suggest_labels(&name, self.tag_model.as_deref(), &self.tag_manager_categories, &self.config.tag_match, i18n::t());
                    LabelRow { file_name: name, tokens, confirmed: false }
                })
                .filter(|r| !r.tokens.is_empty())
                .collect();
            if !rows.is_empty() {
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
            st.stage = LearnStage::Hub;
            st.refresh(self);
        }
        if cancel_labeling {
            st.stage = LearnStage::Hub;
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
}

enum LearnStage {
    Hub,
    Labeling(Vec<LabelRow>),
}

struct LabelRow {
    file_name: String,
    tokens: Vec<(Token, Label)>,
    confirmed: bool,
}

impl TagLearn {
    fn open(app: &NekoviewApp) -> Self {
        let mut st = Self { sample_count: 20, stage: LearnStage::Hub, estimate: None, dir: PathBuf::new(), names: Vec::new(), available: 0 };
        st.refresh(app);
        st
    }

    /// 現在のフォルダのファイル名を取り直し、学習できる数を数える。
    fn refresh(&mut self, app: &NekoviewApp) {
        self.dir = app.current_dir.clone();
        self.names = app.tag_learn_source_names();
        let learned: HashSet<&str> = app.tag_learned.entries.iter().map(|e| e.file_name.as_str()).collect();
        self.available = self.names.iter().filter(|n| !learned.contains(n.as_str()) && !tokenize(n).is_empty()).count();
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
