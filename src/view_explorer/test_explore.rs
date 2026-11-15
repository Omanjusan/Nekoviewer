//! 自動テスト用（Cargo feature `test-control`）: エクスプローラー側の操作と、検索まわりの状態ダンプ。
//! 制御口の仕様は `test_control.rs`、E2E は `tests/search_e2e.rs`。通常ビルドには入らない。

use redb::ReadableDatabase;

use crate::test_control::{ExploreCmd, TabArg};

use super::{DirectoryNavigationSource, FolderPaneTab, NekoviewApp, SearchMode};

impl NekoviewApp {
    /// 制御口のコマンドを、画面の操作と同じ関数を通して実行する。
    pub fn test_explore_cmd(&mut self, cmd: ExploreCmd) {
        match cmd {
            ExploreCmd::Open(path) => self.navigate_to(path, DirectoryNavigationSource::Tree),
            ExploreCmd::Tab(tab) => {
                let tab = match tab {
                    TabArg::Real => FolderPaneTab::RealTree,
                    TabArg::Favorites => FolderPaneTab::Favorites,
                    TabArg::Search => FolderPaneTab::Search,
                    TabArg::Virtual => FolderPaneTab::VirtualFolders,
                    TabArg::Tag => FolderPaneTab::TagSearch,
                };
                self.switch_folder_tab(tab);
            }
            ExploreCmd::SearchMode(new) => {
                self.set_search_mode(if new { SearchMode::New } else { SearchMode::Legacy });
            }
            ExploreCmd::SearchText(text) => self.search_form.new_text = text,
            ExploreCmd::SearchSize { min, max } => {
                self.search_form.new_size_min_mb = min;
                self.search_form.new_size_max_mb = max;
            }
            ExploreCmd::SearchStart => match self.search_form.mode {
                SearchMode::New => self.start_new_search(None),
                SearchMode::Legacy => {
                    let ctx = self.egui_ctx.clone();
                    self.start_search(&ctx, None);
                }
            },
            ExploreCmd::SearchCancel => self.cancel_new_search(),
            ExploreCmd::SearchNotice => self.search_notice_open = true,
            ExploreCmd::GotoReal(idx) => {
                if let Some(file) = self.archives.get(idx).cloned() {
                    self.open_file_location_in_folders_tab(file);
                }
            }
            ExploreCmd::VirtualAdd(path) => {
                if let Some(db) = self.spread_db.clone() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    // 同じ親の下に同じ実フォルダは登録できない。すでに登録があれば、その子として足す
                    // （同じ実フォルダを登録した仮想ノードが複数ある状態を作るため）。
                    let existing = crate::virtual_folders::list_nodes(&db);
                    let parent = existing.iter().find(|n| n.real_path == path).map_or(crate::virtual_folders::ROOT_ID, |n| n.id);
                    let _ = crate::virtual_folders::add_node(&db, parent, &path, &name);
                    self.virtual_lookup_loaded = false;
                }
            }
            ExploreCmd::GotoVirtual { idx, pick } => {
                if let Some(file) = self.archives.get(idx).cloned() {
                    self.start_goto_virtual(file, egui::pos2(100.0, 100.0));
                    if let Some(n) = pick {
                        let chosen = self.goto_virtual_menu.as_ref().and_then(|m| m.entries.get(n).map(|e| (e.0, m.file.clone())));
                        if let Some((id, file)) = chosen {
                            self.open_file_location_in_virtual_tab(id, file);
                        }
                    }
                }
            }
            ExploreCmd::ListMode(list) => {
                self.view_mode.mode = if list { super::view_mode::ViewMode::List } else { super::view_mode::ViewMode::Card };
            }
            ExploreCmd::SearchSelect(idx) => self.select_search_history(idx),
        }
    }

    /// 検索タブ・履歴・台帳の状態（`dump.json` の `search`）。
    pub fn test_search_json(&self) -> serde_json::Value {
        let tab = match self.folder_pane_tab {
            FolderPaneTab::RealTree => "real",
            FolderPaneTab::Favorites => "favorites",
            FolderPaneTab::Search => "search",
            FolderPaneTab::VirtualFolders => "virtual",
            FolderPaneTab::TagSearch => "tag",
        };
        let history: Vec<serde_json::Value> = self
            .search_history
            .iter()
            .map(|e| {
                serde_json::json!({
                    "label": e.label,
                    "hits": e.hits.len(),
                    "running": e.running,
                    "restored": e.restored,
                    "saved_at": e.saved_at,
                    "cancelled": e.cancelled,
                    "unconfirmed": e.unconfirmed,
                    "mode": if e.form.mode == SearchMode::New { "new" } else { "legacy" },
                    "progress": e.progress.map(|(n, t)| serde_json::json!([n, t])),
                })
            })
            .collect();
        let ledger_total = self.spread_db.as_ref().and_then(|db| {
            let guard = db.lock().ok()?;
            let tx = guard.begin_read().ok()?;
            Some(crate::file_identity::record_count_tx(&tx))
        });
        serde_json::json!({
            "tab": tab,
            "mode": if self.search_form.mode == SearchMode::New { "new" } else { "legacy" },
            "scan_state": match &self.scan_state {
                super::ScanState::Idle => "idle",
                super::ScanState::Loading { .. } => "loading",
                super::ScanState::Done => "done",
            },
            "loading_shown": self.scan_loading_visible(),
            "form_text": self.search_form.new_text,
            // 現在のフォルダ表示の重複マーカーの件数（遡及検出が済んだかの目安）
            "dir_markers": self.dir_markers.len(),
            // 検索結果（新モード）で、重複マーカーのアイコンが出るファイル: {ファイル名: アイコン名}
            "cross_markers": self
                .archives
                .iter()
                .filter_map(|p| {
                    let icon = self.cross_marker_icon(p)?;
                    Some((p.display().to_string(), format!("{icon:?}")))
                })
                .collect::<std::collections::BTreeMap<_, _>>(),
            "virtual_node": self.viewing_virtual_node,
            "goto_menu": self.goto_virtual_menu.as_ref().map(|m| m.len()),
            "virtual_ids": self
                .spread_db
                .as_ref()
                .map(|db| crate::virtual_folders::list_nodes(db).iter().map(|n| n.id).collect::<Vec<_>>())
                .unwrap_or_default(),
            "history_visible": self.visible_search_history(),
            "current_dir": self.current_dir.display().to_string(),
            // カーソル（選択枠）が乗っているファイル名。フォルダの行に乗っていれば null。
            "cursor_file": match &self.grid_cursor {
                Some(super::GridEntry::Archive(i)) => self
                    .archives
                    .get(*i)
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            },
            "job_running": self.search_job.is_some(),
            "notice_open": self.search_notice_open,
            "any_running": self.any_search_running(),
            "viewing": self.viewing_search,
            "selected": self.search_selected,
            "archives": self.archives.len(),
            "history": history,
            "ledger_total": ledger_total,
        })
    }
}
