use std::path::PathBuf;
use std::sync::mpsc;

use crate::types::ExplorerSortKey;
use crate::neko_dir;
use crate::fs::dir;
use super::*;

impl NekoviewApp {
    /// 指定ディレクトリへ遷移する（ツリーパネル・サムネグリッドの↑/フォルダクリック共通処理）。
    /// お気に入りタブ表示中ならそれを解除し、現在地・監視先を更新してスキャンを開始する。
    pub(super) fn navigate_to(&mut self, path: PathBuf) {
        self.viewing_favorites = None;
        self.current_dir = path.clone();
        self.viewing_dir = Some(path);
        // サマリーはスキャン完了時（poll_scan）にスキャン結果から起動する
        self.cd_summary = None;
        self.cd_summary_rx = None;
        self.start_scan();
        self.persist_state();
    }

    /// 指定ドライブへ切り替える（ドライブ一覧のクリック・キーボードEnter共通処理）。
    /// ツリーのルート自体をそのドライブへ差し替え、展開状態をリセットする。
    pub(super) fn navigate_to_drive(&mut self, path: PathBuf) {
        self.current_dir = path.clone();
        self.start_scan();
        self.tree_root = path.clone();
        self.tree_expanded.clear();
        self.tree_children.clear();
        self.tree_cursor = None;
        self.viewing_dir = None;
        self.cd_summary = None;
        self.cd_summary_rx = None;
        self.tree_scan_pending = Some(TreeScanPending {
            path: path.clone(),
            rx: dir::spawn_scan_subdirs(path, {
                let c = self.egui_ctx.clone();
                move || c.request_repaint()
            }),
        });
        self.persist_state();
    }

    /// バックグラウンドスキャンを起動する（UIをブロックしない）
    pub(super) fn start_scan(&mut self) {
        let rx = dir::spawn_scan(self.current_dir.clone(), {
            let c = self.egui_ctx.clone();
            move || c.request_repaint()
        });
        self.scan_state = ScanState::Loading {
            dir: self.current_dir.clone(),
            rx,
            started_at: std::time::Instant::now(),
        };
        self.subdirs.clear();
        self.archives.clear();
        self.filtered_indices.clear();
        self.raw_image_files.clear();
        self.invalid_archives.clear();
        // thumb_failed はセッション内で保持する（再入場のたびの無駄な再試行を避ける）。
        // ネットワーク失敗分はマウント回復検知（poll_mount_checks）で解禁される。
        // リンク切れ表示中のマウント配下へ入る場合は到達可否を再確認する（回復検知の入口）
        if let Some(root) = self.network_unreachable_mounts.iter()
            .find(|r| self.current_dir.starts_with(r))
            .cloned()
        {
            self.spawn_mount_check_if_needed(root);
        }
        // DBは既存の場合のみ開く。新規作成は対象ファイルの存在が確定してから
        // （poll_scan）行い、通過しただけのフォルダに空DBを作らない。
        self.cache_neko_dir = neko_dir::neko_dir_for(&self.current_dir, &self.config);
        self.cache_db = self.cache_neko_dir.as_deref().and_then(neko_dir::open_cache_db_if_exists);
        self.thumbnails.clear();
        self.thumb_pending.clear();
        self.pending_loads.lock().unwrap().clear();
        self.selected_archive_index = None;
        self.multi_selected.clear();
        self.select_anchor = None;
        self.explorer_scroll_offset = 0.0;
    }

    /// フレームごとにスキャン結果をポーリングして反映する
    pub(super) fn poll_scan(&mut self) {
        let result = match self.scan_state {
            ScanState::Loading { ref dir, ref rx, .. } => {
                // 移動先が変わっていたら古い結果を捨てる
                if *dir != self.current_dir {
                    self.scan_state = ScanState::Idle;
                    return;
                }
                rx.try_recv().ok()
            }
            _ => return,
        };

        if let Some((subdirs, archives, raw_images)) = result {
            // 対象ファイルが存在するフォルダに限りDBを新規作成する
            if self.cache_db.is_none() && !(archives.is_empty() && raw_images.is_empty()) {
                self.cache_db = self.cache_neko_dir.as_deref().and_then(neko_dir::open_cache_db);
            }
            self.subdirs = subdirs;
            self.archives = archives.into_iter()
                .filter(|p| {
                    let filename = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    self.cache_db.as_ref()
                        .map_or(true, |db| !neko_dir::is_invalid_and_current(db, filename, p))
                })
                .collect();
            for img in raw_images {
                self.raw_image_files.insert(img.clone());
                self.archives.push(img);
            }
            if let Some(db) = self.spread_db.clone() {
                let filenames: Vec<String> = self.archives.iter()
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
                    .collect();
                crate::spread_state::gc_dir(&db, &self.current_dir, &filenames);
                self.spread_states = crate::spread_state::list_dir_entries(&db, &self.current_dir)
                    .into_iter()
                    .map(|(name, mode, offset)| (name, (mode, offset)))
                    .collect();
                crate::favorites::gc_dir(&db, &self.current_dir, &filenames);
                self.favorite_states = crate::favorites::list_dir_favorites(&db, &self.current_dir)
                    .into_iter()
                    .collect();
            } else {
                self.spread_states.clear();
                self.favorite_states.clear();
            }
            self.scan_state = ScanState::Done;
            self.sort_archives();
            // グリッドの統一カーソルを新しいディレクトリの先頭（↑があればそれ）へ即座に
            // 合わせる。矢印キーを押すまで何もカーソルが出ない空白期間を作らないため。
            let entries = self.grid_entries();
            if let Some(first) = entries.first() {
                self.set_grid_cursor(first.clone());
            } else {
                self.grid_cursor = None;
                self.selected_archive_index = None;
                self.selected_archive_meta = None;
            }
            self.multi_selected.clear();
            self.select_anchor = None;
            // サマリーはスキャン済みリストを使い回して起動する（ネットワークの再列挙を避ける）
            if self.viewing_dir.as_ref() == Some(&self.current_dir) {
                self.cd_summary_rx = Some(spawn_summary_worker(
                    self.current_dir.clone(),
                    self.archive_filenames(),
                    self.cache_db.clone(),
                    self.egui_ctx.clone(),
                ));
            }
        }
    }

    /// 現在の archives（生画像含む）のファイル名一覧。サマリー計算用。
    pub(super) fn archive_filenames(&self) -> Vec<String> {
        self.archives.iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect()
    }

    /// フレームごとにツリー展開スキャン結果をポーリングして反映する
    pub(super) fn poll_tree_scan(&mut self) {
        let result = if let Some(ref pending) = self.tree_scan_pending {
            pending.rx.try_recv().ok().map(|subdirs| (pending.path.clone(), subdirs))
        } else {
            return;
        };

        if let Some((path, subdirs)) = result {
            self.tree_children.insert(path.clone(), subdirs);
            // ルートの場合は展開済みにする
            if path == self.tree_root {
                self.tree_expanded.insert(path);
            }
            self.tree_scan_pending = None;
        }
    }

    pub(super) fn sort_archives(&mut self) {
        let ascending = self.sort_ascending;
        // お気に入り一覧表示中は favorite_states が実ディレクトリ用の古いデータのままで
        // 信頼できないため、スティッキー判定は通常のディレクトリ表示中のみ行う。
        let sticky_favorites = self.viewing_favorites.is_none();
        let is_fav = |p: &PathBuf| -> bool {
            sticky_favorites
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| self.favorite_states.contains_key(name))
        };
        match self.sort_key {
            ExplorerSortKey::Name => {
                self.archives.sort_by(|a, b| {
                    let fav_cmp = is_fav(b).cmp(&is_fav(a));
                    if fav_cmp != std::cmp::Ordering::Equal { return fav_cmp; }
                    let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let cmp = na.cmp(nb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            ExplorerSortKey::Date => {
                self.archives.sort_by(|a, b| {
                    let fav_cmp = is_fav(b).cmp(&is_fav(a));
                    if fav_cmp != std::cmp::Ordering::Equal { return fav_cmp; }
                    let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
                    let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
                    let cmp = ta.cmp(&tb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            ExplorerSortKey::Size => {
                self.archives.sort_by(|a, b| {
                    let fav_cmp = is_fav(b).cmp(&is_fav(a));
                    if fav_cmp != std::cmp::Ordering::Equal { return fav_cmp; }
                    let sa = std::fs::metadata(a).map(|m| m.len()).unwrap_or(0);
                    let sb = std::fs::metadata(b).map(|m| m.len()).unwrap_or(0);
                    let cmp = sa.cmp(&sb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
        }
        self.recompute_filter();
    }

    /// フィルタ文字列・ON/OFF・archives の並び替えのいずれかが変わった時に呼び、
    /// 表示・選択・キー操作の対象となる `filtered_indices` を作り直す。
    pub(super) fn recompute_filter(&mut self) {
        if self.filter_enabled && !self.filter_text.trim().is_empty() {
            let text = self.filter_text.trim();
            // *, ?, [...] / [!...] が含まれる場合のみ glob パターンとして扱う。
            // 含まれない場合や glob として不正な場合は従来通りの部分一致にフォールバックする。
            let pattern = if text.contains(['*', '?', '[']) {
                glob::Pattern::new(text).ok()
            } else {
                None
            };
            let match_opts = glob::MatchOptions {
                case_sensitive: false,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            };
            let needle = text.to_lowercase();
            self.filtered_indices = self.archives.iter().enumerate()
                .filter(|(_, p)| {
                    let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return false };
                    match &pattern {
                        Some(pat) => pat.matches_with(name, match_opts),
                        None => name.to_lowercase().contains(&needle),
                    }
                })
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_indices = (0..self.archives.len()).collect();
        }

        // 選択中の項目がフィルタで除外されたら先頭に付け直す
        if let Some(idx) = self.selected_archive_index {
            if !self.filtered_indices.contains(&idx) {
                self.selected_archive_index = self.filtered_indices.first().copied();
            }
        }
        // 複数選択もフィルタで隠れた分は外す（表示外の項目を選択集合に残さない）
        let filtered_set: std::collections::HashSet<usize> = self.filtered_indices.iter().copied().collect();
        self.multi_selected.retain(|idx| filtered_set.contains(idx));
    }
}

/// cd_summary の計算をバックグラウンドスレッドで行い、受信チャンネルを返す。
/// ディレクトリの再列挙はせず、スキャン済みのファイル名一覧を受け取って
/// ローカルDBのカウントだけを行う（ネットワークI/Oなし）。
pub(super) fn spawn_summary_worker(
    path: PathBuf,
    filenames: Vec<String>,
    db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
    ctx: egui::Context,
) -> mpsc::Receiver<(PathBuf, usize, usize)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let total = filenames.len();
        let saved = db.map(|db| neko_dir::count_cached_thumbs(&db, &filenames)).unwrap_or(0);
        let _ = tx.send((path, saved, total));
        // ROOT を起こして poll_workers に結果を回収させる
        ctx.request_repaint();
    });
    rx
}
