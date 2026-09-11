use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::atomic::Ordering;

use crate::types::ExplorerSortKey;
use crate::neko_dir;
use crate::fs::dir;
use crate::fs::mount::{list_gvfs_smb_mounts, list_local_drives};
use super::*;

fn thumbnail_local_limit(
    max_configured: usize,
    cores: usize,
    since_input: std::time::Duration,
    since_folder_open: std::time::Duration,
) -> usize {
    let max_local = max_configured.min(cores).max(1);
    let input_cap = if since_input < std::time::Duration::from_secs(1) { max_local.min(2) } else { max_local };
    // フォルダを開いた直後は表示中デコード（可視セルの直接request）とバックグラウンド
    // 先読みが競合しやすいので、開いてからしばらくは先読み側の並列度を絞る。
    let warmup_cap = if since_folder_open < std::time::Duration::from_millis(300) {
        1
    } else if since_folder_open < std::time::Duration::from_millis(900) {
        max_local.min(2)
    } else {
        max_local
    };
    input_cap.min(warmup_cap)
}

fn take_allowed_thumbnail(
    queue: &mut std::collections::VecDeque<PathBuf>,
    queued: &HashSet<PathBuf>,
    missing: &HashSet<PathBuf>,
    only_missing: bool,
    allow_local: bool,
    allow_network: bool,
) -> Option<PathBuf> {
    let pos = queue.iter().position(|path| {
        if !queued.contains(path) { return false; }
        if only_missing && !missing.contains(path) { return false; }
        if crate::fs::dir::is_gvfs_path(path) { allow_network } else { allow_local }
    })?;
    queue.remove(pos)
}

impl NekoviewApp {
    /// 指定ディレクトリへ遷移する。
    /// お気に入りタブ表示中ならそれを解除し、現在地・監視先を更新してスキャンを開始する。
    pub(super) fn navigate_to(&mut self, path: PathBuf, source: DirectoryNavigationSource) {
        self.viewing_favorites = None;
        self.current_dir = path.clone();
        self.viewing_dir = Some(path.clone());
        // サマリーはスキャン完了時（poll_scan）にスキャン結果から起動する
        self.cd_summary = None;
        self.cd_summary_rx = None;
        self.start_scan();
        match source {
            DirectoryNavigationSource::Tree => {
                // ツリーで選べるノードは既に可視なので、追従・アラインさせない。
                // 直前の別操作から残った要求も、後のフレームで発火しないよう破棄する。
                self.tree_autofocus = None;
                self.tree_autofocus_pending = None;
                self.tree_autofocus_scroll_pending = false;
            }
            DirectoryNavigationSource::ItemPane | DirectoryNavigationSource::System => {
                self.start_tree_autofocus(path);
            }
        }
        self.persist_state();
    }

    /// ディレクトリツリー側を現在地まで自動展開させる。root(tree_root) から target までの
    /// path component 列を計算し、1階層ずつ逐次展開する TreeAutoFocus 状態をセットする。
    /// target が tree_root 配下でない場合（別ドライブ切替直後の競合等）は何もしない。
    pub(super) fn start_tree_autofocus(&mut self, target: PathBuf) {
        self.tree_autofocus_pending = None;
        let Some(remaining) = tree_autofocus_components(&self.tree_root, &target) else {
            // target が tree_root 配下でない（別ドライブ切替直後の競合等）→ 何もしない
            self.tree_autofocus = None;
            return;
        };
        if remaining.is_empty() {
            // target 自体が tree_root（ルート直下を見ている）
            self.tree_cursor = Some(target);
            self.tree_autofocus = None;
            self.tree_autofocus_scroll_pending = true;
            return;
        }
        self.tree_autofocus = Some(TreeAutoFocus {
            target,
            remaining,
            current: self.tree_root.clone(),
        });
    }

    /// フレームごとにツリー自動追従を1階層分だけ進める。
    /// 兄弟ディレクトリの中身には踏み込まず、常に一本道の経路だけを辿る。
    pub(super) fn poll_tree_autofocus(&mut self) {
        // 自動追従専用レーンのロード結果を受信する
        if let Some(pending) = &self.tree_autofocus_pending {
            match pending.rx.try_recv() {
                Ok(subdirs) => {
                    self.tree_children.insert(pending.path.clone(), subdirs);
                    self.tree_autofocus_pending = None;
                }
                Err(_) => return, // まだロード中
            }
        }

        // tree_children に既にキャッシュ済みの階層は非同期を挟まず同一フレーム内で
        // 連鎖処理する（イベント駆動の repaint に頼らず一気に進める。ロードが要る
        // 階層に当たった時だけ抜けて次フレームへ持ち越す）。
        loop {
            let Some(af) = self.tree_autofocus.as_ref() else { return };

            let Some(component) = af.remaining.front().cloned() else {
                // 全階層展開完了 → カーソルを合わせて終了
                let target = af.target.clone();
                self.tree_cursor = Some(target);
                self.tree_autofocus = None;
                self.tree_autofocus_scroll_pending = true;
                return;
            };

            let current = af.current.clone();
            let found = match self.tree_children.get(&current) {
                Some(children) => {
                    // direct child directory の中から component 名と一致するものだけを探す
                    // （兄弟ディレクトリの内部へは踏み込まない）
                    children.iter().find(|c| c.file_name() == Some(component.as_os_str())).cloned()
                }
                None => {
                    // 未ロードならこの階層だけロードして次フレームへ持ち越す
                    self.tree_autofocus_pending = Some(TreeScanPending {
                        path: current.clone(),
                        rx: dir::spawn_scan_subdirs(current, {
                            let c = self.egui_ctx.clone();
                            move || c.request_repaint()
                        }),
                    });
                    return;
                }
            };

            match found {
                Some(child) => {
                    self.tree_expanded.insert(current);
                    let af = self.tree_autofocus.as_mut().expect("checked above");
                    af.remaining.pop_front();
                    af.current = child;
                }
                None => {
                    // 対象パスがツリー上に存在しない（隠しディレクトリ等）→ ここまでで打ち切り
                    self.tree_autofocus = None;
                    return;
                }
            }
        }
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
        self.tree_autofocus = None;
        self.tree_autofocus_pending = None;
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

    /// リロードボタンから呼ばれる。ドライブ一覧・現在CD位置・ツリーを再スキャンする。
    pub(super) fn reload_current(&mut self) {
        // お気に入り一覧表示中は実ディレクトリの概念が無く、start_scan()を呼ぶと
        // enter_favorite_view が差し替えた self.archives を実フォルダの中身で
        // 上書きしてしまう（exit_favorite_view相当が意図せず起きる）ため何もしない。
        if self.viewing_favorites.is_some() {
            return;
        }

        // ドライブ一覧の再取得（同期・軽量なローカル列挙のみ、ネットワークI/Oは行わない）。
        // GVFS切断（電源off等）は gvfsd がマウントエントリを即座に消さないため、
        // readdir だけでは検知できない。到達可否はバックグラウンドで別途確認し、
        // 不通と判明した時点で network_unreachable_mounts に記録される。
        // 既に不通判定済みのマウントは、復活が確認できるまで一覧に出さない
        // （出してしまうと次のリロードごとに表示→非表示を繰り返すため）。
        //
        // mount_check_pending が空でない（＝バックグラウンドの到達可否チェックが
        // 進行中）間は list_gvfs_smb_mounts() を呼ばない。進行中チェックの read_dir と
        // 同時にトップレベル /run/user/uid/gvfs を readdir すると gvfsd 内部で
        // ロック競合し、メインスレッドまでブロックされることがあるため。
        // その間は直前に取得済みの gvfs_mount_entries をそのまま使い回す。
        let mut drives = list_local_drives();
        if self.mount_check_pending.is_empty() {
            let gvfs_mounts = list_gvfs_smb_mounts();
            for mount in &gvfs_mounts {
                self.spawn_mount_check_if_needed(mount.path.clone());
            }
            self.gvfs_mount_entries = gvfs_mounts;
        }
        drives.extend(
            self.gvfs_mount_entries
                .iter()
                .filter(|m| !self.network_unreachable_mounts.contains(&m.path))
                .cloned(),
        );
        self.drives = drives;
        let home = self.drives.first().map(|d| d.path.clone());

        // ツリールート自体が消えたマウント配下だった場合、安全にホームドライブへ退避する。
        // ネットワークマウント配下は同期 exists() を使わず、バックグラウンドで確認済みの
        // 到達可否（network_unreachable_mounts）を参照する
        // （不通の GVFS マウント配下で exists() を呼ぶと CIFS タイムアウトまで
        // メインスレッドがブロックされ、リロード操作でUIごと固まってしまうため）。
        if !self.path_reachable(&self.tree_root.clone()) {
            if let Some(home) = home {
                self.navigate_to_drive(home);
            }
            return;
        }

        // ツリールートは無事だが、CD位置だけが消えたマウント配下だった場合はホームへ移動する。
        if let Some(viewing) = self.viewing_dir.clone() {
            if !self.path_reachable(&viewing) {
                if let Some(home) = home {
                    self.navigate_to(home, DirectoryNavigationSource::System);
                }
                return;
            }
        }

        self.start_scan();

        // ツリー: ルート + 展開済み全ノードをスレッド1本でまとめて再取得する。
        // 個別ノードの遅延展開（tree_scan_pending）が進行中でも衝突はしない
        // （どちらが後から書き込んでも tree_children の内容は同じソースから来るため実害なし）。
        let mut targets: Vec<PathBuf> = vec![self.tree_root.clone()];
        targets.extend(self.tree_expanded.iter().cloned());
        self.tree_reload_pending = Some(TreeReloadPending {
            rx: dir::spawn_scan_subdirs_many(targets, {
                let c = self.egui_ctx.clone();
                move || c.request_repaint()
            }),
        });
    }

    /// path の到達可否を判定する。ネットワークマウント配下は同期I/Oを行わず、
    /// バックグラウンドで確認済みの network_unreachable_mounts を参照する
    /// （未確認の場合は楽観的に到達可能とみなす）。それ以外は通常の exists()。
    pub(super) fn path_reachable(&self, path: &std::path::Path) -> bool {
        match self.network_mount_root_cached(path) {
            Some(root) => !self.network_unreachable_mounts.contains(&root),
            None => path.exists(),
        }
    }

    /// バックグラウンドスキャンを起動する（UIをブロックしない）
    pub(super) fn start_scan(&mut self) {
        self.thumb_session.fetch_add(1, Ordering::AcqRel);
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
        // PWD再入場は明示的な再試行契機なので、同一滞在中の失敗抑制を解除する。
        self.thumb_failed.clear();
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
        self.cache_db = self.cache_neko_dir.as_deref()
            .and_then(|p| neko_dir::open_cache_db_if_exists(p, &self.current_dir));
        self.refresh_thumbnail_generation_state();
        self.thumbnails.clear();
        self.thumb_display_requested.clear();
        self.thumb_pending.clear();
        self.thumb_queue.clear();
        self.thumb_priority_queue.clear();
        self.thumb_queued.clear();
        self.thumb_missing_queued.clear();
        self.thumb_priority_queued.clear();
        self.pending_loads.lock().unwrap().clear();
        self.selected_archive_index = None;
        self.multi_selected.clear();
        self.select_anchor = None;
        self.explorer_scroll_offset = 0.0;
    }

    /// フレームごとにスキャン結果をポーリングして反映する
    pub(super) fn poll_scan(&mut self) {
        // お気に入り/検索結果の横断表示中はarchives/subdirsを差し替えているため、
        // 入室時に開始していた実ディレクトリの背後スキャン結果が遅れて届くと
        // 無条件の上書きで表示が壊れる（検索完了直後にサブフォルダが復活する等）。
        // 表示を抜けるとき（exit_favorite_view/exit_search_view、switch_folder_tab経由で
        // 必ず呼ばれる）に改めてstart_scan()されるため、ここでは単に無視すればよい。
        // folder_pane_tab とOptionフラグの二重チェック（タブ状態を唯一の一次判定にしつつ、
        // フラグの取りこぼしがあっても安全側に倒す）。
        if self.folder_pane_tab != FolderPaneTab::RealTree
            || self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
        {
            return;
        }
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
            let existing_filenames: Vec<String> = archives.iter().chain(raw_images.iter())
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
                .collect();
            // 対象ファイルが存在するフォルダに限りDBを新規作成する
            if self.cache_db.is_none() && !(archives.is_empty() && raw_images.is_empty()) {
                self.cache_db = self.cache_neko_dir.as_deref()
                    .and_then(|p| neko_dir::open_cache_db(p, &self.current_dir));
            }
            self.refresh_thumbnail_generation_state();
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
            // archives の顔ぶれが変わったので、カード情報帯のメタデータキャッシュを捨てる
            // （消失・更新・別フォルダ移動の反映）。可視カードぶんは描画時に再充填される。
            self.archive_meta_cache.clear();
            if let Some(db) = self.spread_db.clone() {
                let filenames: Vec<String> = self.archives.iter()
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
                    .collect();
                crate::spread_state::gc_dir(&db, &self.current_dir, &filenames);
                self.spread_states = crate::spread_state::list_dir_entries(&db, &self.current_dir)
                    .into_iter()
                    .map(|(name, mode, offset)| (name, (mode, offset)))
                    .collect();
                crate::spread_state::gc_archive_sorts(&db, &self.current_dir, &filenames);
                self.archive_sort_states = crate::spread_state::list_dir_archive_sorts(&db, &self.current_dir)
                    .into_iter()
                    .map(|(name, key, ascending)| (name, (key, ascending)))
                    .collect();
                crate::favorites::gc_dir(&db, &self.current_dir, &filenames);
                self.favorite_states = crate::favorites::list_dir_favorites(&db, &self.current_dir)
                    .into_iter()
                    .collect();
            } else {
                self.spread_states.clear();
                self.archive_sort_states.clear();
                self.favorite_states.clear();
            }
            self.saved_archive_settings = self.spread_db.as_ref()
                .map(|db| crate::spread_state::saved_settings_for_paths(db, &self.archives))
                .unwrap_or_default();
            if let Some(db) = &self.cache_db {
                let _ = neko_dir::sync_thumbnail_records(
                    db,
                    &self.archive_filenames(),
                    &existing_filenames,
                );
            }
            self.rebuild_thumbnail_queue();
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
                    self.config.thumb_size,
                    self.config.thumb_filter.thumbnail_cache_id(),
                    HashSet::new(),
                    self.egui_ctx.clone(),
                ));
            }
            // CLIでファイル指定起動された場合の自動オープン。初回スキャン結果でのみ試行し、
            // 成否に関わらず一度きりで消費する（以降このディレクトリへ戻っても再発火しない）。
            if let Some(target) = self.pending_open_target.take() {
                self.try_open_pending_target(target);
            }
        }
    }

    /// 起動時オープン対象を、ダブルクリックで開くのと同じ手順で開く。
    /// 対象がスキャン結果に見当たらない・破損等で開けない場合は何もしない
    /// （＝現在表示中の親DIRのままに留める＝要件の「開けなければDIRに留める」を満たす）。
    fn try_open_pending_target(&mut self, target: PathBuf) {
        let Some(real_idx) = self.archives.iter().position(|p| p == &target) else { return; };
        self.selected_archive_index = Some(real_idx);
        self.selected_archive_meta = None;
        self.grid_cursor = Some(GridEntry::Archive(real_idx));

        if self.raw_image_files.contains(&target) {
            if self.network_gate(&target) {
                self.open_viewer(ViewerState::new_raw(target.clone(), self.viewer_slots, self.config.default_slot));
            }
            return;
        }
        if self.invalid_archives.contains(&target) { return; }
        if !self.network_gate(&target) { return; }
        if !self.check_memory_budget(&target) { return; }
        match ViewerState::new(target.clone(), self.viewer_slots, self.config.default_slot) {
            Some(state) => self.open_viewer(state),
            None => self.mark_archive_invalid(&target),
        }
    }

    /// 現PWDの既存JPEG群とGUI設定サイズを照合し、キャッシュプローブ後の生成可否を更新する。
    pub(crate) fn refresh_thumbnail_generation_state(&mut self) {
        let requested_edge_changed =
            self.thumb_generation_state.requested_edge != self.config.thumb_size
                || self.thumb_generation_state.requested_filter != self.config.thumb_filter.thumbnail_cache_id();
        self.thumb_generation_state = self.cache_db.as_ref().map_or(
            neko_dir::ThumbnailGenerationState {
                requested_edge: self.config.thumb_size,
                requested_filter: self.config.thumb_filter.thumbnail_cache_id(),
            },
            |db| neko_dir::thumbnail_generation_state(
                db,
                self.config.thumb_size,
                self.config.thumb_filter.thumbnail_cache_id(),
            ),
        );
        if requested_edge_changed {
            self.thumb_session.fetch_add(1, Ordering::AcqRel);
            if let Some(db) = &self.cache_db {
                let _ = neko_dir::invalidate_processing_for_profile(
                    db,
                    self.config.thumb_size,
                    self.config.thumb_filter.thumbnail_cache_id(),
                );
            }
            self.thumb_pending.clear();
            self.thumb_failed.clear();
            self.rebuild_thumbnail_queue();
            if self.viewing_dir.as_ref() == Some(&self.current_dir) {
                self.cd_summary_rx = Some(spawn_summary_worker(
                    self.current_dir.clone(),
                    self.archive_filenames(),
                    self.cache_db.clone(),
                    self.config.thumb_size,
                    self.config.thumb_filter.thumbnail_cache_id(),
                    HashSet::new(),
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

    fn is_thumb_missing(&self, path: &PathBuf) -> bool {
        path.file_name().and_then(|n| n.to_str())
            .and_then(|name| self.cache_db.as_ref()
                .and_then(|db| neko_dir::read_thumbnail_state(db, name)))
            .is_none_or(|state| state.status == neko_dir::ThumbnailStatus::Missing)
    }

    /// バックグラウンド先読みキューを作り直す（フォルダ再入場・フィルタ変更時）。
    /// 全件を積むのではなく空にするだけ：実際の投入は毎フレーム
    /// [[update_thumbnail_lookahead]] が可視範囲＋進行方向1画面ぶんだけ行う。
    pub(super) fn rebuild_thumbnail_queue(&mut self) {
        self.thumb_queue.clear();
        self.thumb_priority_queue.clear();
        self.thumb_queued.clear();
        self.thumb_missing_queued.clear();
        self.thumb_priority_queued.clear();
        self.thumb_visible_order_range = None;
        self.thumb_queue_built_at = std::time::Instant::now();
    }

    pub(super) fn prioritize_thumbnail_path(&mut self, path: &PathBuf) {
        if self.thumb_queued.contains(path) && self.thumb_priority_queued.insert(path.clone()) {
            self.thumb_priority_queue.push_back(path.clone());
            self.egui_ctx.request_repaint();
        }
    }

    /// 毎フレーム、グリッドの可視範囲（`visible`内のposition `lo..=hi`）を受け取り、
    /// バックグラウンド先読みキューをその場所に追従させる。
    /// 可視セル自体は描画ループが直接requestするので、ここではスクロールの
    /// 進行方向へ1画面ぶんだけ先読みを足し、逆側にはみ出た未送信ぶんは捨てる。
    pub(super) fn update_thumbnail_lookahead(
        &mut self,
        visible: &[(usize, PathBuf)],
        lo: usize,
        hi: usize,
    ) {
        if self.folder_pane_tab != FolderPaneTab::RealTree
            || self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
            || visible.is_empty()
        {
            return;
        }
        let screen_len = hi - lo + 1;
        let prev = self.thumb_visible_order_range;
        self.thumb_visible_order_range = Some((lo, hi));
        let forward = match prev {
            Some((prev_lo, _)) if lo > prev_lo => true,
            Some((prev_lo, _)) if lo < prev_lo => false,
            // 初回・スクロールなしは方向不明。先読みウィンドウは前回のまま動かさない。
            _ => return,
        };
        let want_range = if forward {
            let want_lo = hi + 1;
            if want_lo >= visible.len() { None } else {
                Some((want_lo, (want_lo + screen_len - 1).min(visible.len() - 1)))
            }
        } else if lo == 0 {
            None
        } else {
            let want_hi = lo - 1;
            Some((want_hi.saturating_sub(screen_len - 1), want_hi))
        };
        let Some((want_lo, want_hi)) = want_range else {
            // これ以上先読みする方向がない（末尾/先頭）: 既存の先読みぶんを全部捨てる
            self.thumb_queue.clear();
            self.thumb_priority_queue.clear();
            self.thumb_queued.clear();
            self.thumb_missing_queued.clear();
            self.thumb_priority_queued.clear();
            return;
        };
        let desired: HashSet<&PathBuf> = visible[want_lo..=want_hi].iter().map(|(_, p)| p).collect();
        // ウィンドウ外へ外れた未送信ぶんはキャンセルする（送信済み＝thumb_pendingは対象外）。
        self.thumb_queue.retain(|p| desired.contains(p));
        self.thumb_priority_queue.retain(|p| desired.contains(p));
        self.thumb_queued.retain(|p| desired.contains(p));
        self.thumb_missing_queued.retain(|p| desired.contains(p));
        self.thumb_priority_queued.retain(|p| desired.contains(p));
        for (_, path) in &visible[want_lo..=want_hi] {
            if self.thumbnails.contains_key(path)
                || self.thumb_pending.contains(path)
                || self.thumb_failed.contains(path)
                || self.thumb_queued.contains(path)
            {
                continue;
            }
            if self.is_thumb_missing(path) {
                self.thumb_missing_queued.insert(path.clone());
            }
            self.thumb_queued.insert(path.clone());
            self.thumb_queue.push_back(path.clone());
        }
        if !self.thumb_queue.is_empty() {
            self.egui_ctx.request_repaint();
        }
    }

    fn next_queued_thumbnail(&mut self, allow_local: bool, allow_network: bool) -> Option<PathBuf> {
        take_allowed_thumbnail(
            &mut self.thumb_priority_queue, &self.thumb_queued, &self.thumb_missing_queued,
            true, allow_local, allow_network,
        ).or_else(|| take_allowed_thumbnail(
            &mut self.thumb_priority_queue, &self.thumb_queued, &self.thumb_missing_queued,
            false, allow_local, allow_network,
        )).or_else(|| take_allowed_thumbnail(
            &mut self.thumb_queue, &self.thumb_queued, &self.thumb_missing_queued,
            true, allow_local, allow_network,
        ).or_else(|| take_allowed_thumbnail(
            &mut self.thumb_queue, &self.thumb_queued, &self.thumb_missing_queued,
            false, allow_local, allow_network,
        )))
    }

    pub(super) fn pump_thumbnail_queue(&mut self, ctx: &egui::Context) {
        if self.folder_pane_tab != FolderPaneTab::RealTree
            || self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
        {
            return;
        }
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let since_input = self.thumb_last_user_activity.elapsed();
        let since_folder_open = self.thumb_queue_built_at.elapsed();
        let local_limit = thumbnail_local_limit(
            self.config.resolved_decode_threads(), cores, since_input, since_folder_open,
        );
        let mut active_local = self.thumb_pending.iter()
            .filter(|path| !crate::fs::dir::is_gvfs_path(path)).count();
        let mut active_network = self.thumb_pending.iter()
            .filter(|path| crate::fs::dir::is_gvfs_path(path)).count();
        loop {
            let allow_local = active_local < local_limit;
            let allow_network = active_network < 2;
            if !allow_local && !allow_network { break; }
            let Some(path) = self.next_queued_thumbnail(allow_local, allow_network) else { break };
            let is_network = crate::fs::dir::is_gvfs_path(&path);
            let selection = path.parent().and_then(|dir| {
                let filename = path.file_name()?.to_str()?;
                self.spread_db.as_ref().and_then(|db| {
                    crate::spread_state::read_thumbnail_selection(db, dir, filename)
                })
            });
            let request = ThumbRequest {
                archive_path: path.clone(),
                db: self.cache_db.clone(),
                is_raw_file: self.raw_image_files.contains(&path),
                thumbnail_selection: selection,
                requested_edge: self.config.thumb_size,
                requested_filter: self.config.thumb_filter,
                generation_token: None,
                session_id: self.thumb_session.load(Ordering::Acquire),
            };
            if self.thumb_req_tx.try_send(request).is_err() {
                self.thumb_queue.push_front(path);
                break;
            }
            self.thumb_queued.remove(&path);
            self.thumb_missing_queued.remove(&path);
            self.thumb_priority_queued.remove(&path);
            self.thumb_pending.insert(path);
            if is_network { active_network += 1; } else { active_local += 1; }
        }
        if !self.thumb_queued.is_empty() {
            let delay = std::time::Duration::from_secs(1).saturating_sub(since_input);
            ctx.request_repaint_after(delay.max(std::time::Duration::from_millis(16)));
        }
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

    /// フレームごとにツリー一括リロードの結果をポーリングして反映する。
    /// 取得前に tree_children をクリアしないため、更新中に子が消えて見えるチラつきは無い。
    pub(super) fn poll_tree_reload(&mut self) {
        let Some(ref pending) = self.tree_reload_pending else { return };
        let Ok(results) = pending.rx.try_recv() else { return };
        for (path, children) in results {
            self.tree_children.insert(path, children);
        }
        self.tree_reload_pending = None;
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
            let text = self.filter_text.clone();
            self.filtered_indices = self.archives.iter().enumerate()
                .filter(|(_, p)| {
                    let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return false };
                    dir::name_matches(&text, name)
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
    requested_edge: u32,
    requested_filter: u32,
    failed_filenames: HashSet<String>,
    ctx: egui::Context,
) -> mpsc::Receiver<(PathBuf, usize, usize, bool)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let total = filenames.len();
        let progress = db.map(|db| {
            let progress_filenames: Vec<String> = filenames.iter()
                .filter(|name| !failed_filenames.contains(name.as_str()))
                .cloned()
                .collect();
            neko_dir::thumbnail_progress(&db, &progress_filenames, requested_edge, requested_filter)
        }).unwrap_or_default();
        let _ = tx.send((path, progress.current, total, progress.replacing_old));
        // ROOT を起こして poll_workers に結果を回収させる
        ctx.request_repaint();
    });
    rx
}

/// tree_root から target までに辿るべき子ディレクトリ名の並びを返す。
/// - `None`  : target が tree_root 配下でない（自動追従は不能）
/// - 空の並び: target が tree_root 自身（追従不要、その場でカーソル確定）
/// - 非空    : 先頭から 1 階層ずつ展開していく経路
pub(super) fn tree_autofocus_components(
    tree_root: &std::path::Path,
    target: &std::path::Path,
) -> Option<std::collections::VecDeque<std::ffi::OsString>> {
    let rel = target.strip_prefix(tree_root).ok()?;
    Some(
        rel.components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_os_string()),
                _ => None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tree_autofocus_tests {
    use super::tree_autofocus_components;
    use std::path::Path;

    #[test]
    fn returns_component_chain_for_descendant() {
        let got = tree_autofocus_components(Path::new("/mnt/photos"), Path::new("/mnt/photos/2024/summer"))
            .expect("descendant path resolves");
        let chain: Vec<_> = got.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(chain, vec!["2024", "summer"]);
    }

    #[test]
    fn returns_empty_chain_when_target_is_root_itself() {
        let got = tree_autofocus_components(Path::new("/mnt/photos"), Path::new("/mnt/photos"))
            .expect("root == target resolves");
        assert!(got.is_empty(), "追従不要なので空の経路");
    }

    #[test]
    fn returns_none_when_target_outside_root() {
        assert!(
            tree_autofocus_components(Path::new("/mnt/photos"), Path::new("/home/user/pics")).is_none(),
            "tree_root 配下でなければ None（no-op）"
        );
    }
}

#[cfg(test)]
mod thumbnail_queue_tests {
    use super::{take_allowed_thumbnail, thumbnail_local_limit};
    use std::collections::{HashSet, VecDeque};
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn local_parallelism_stays_small_until_one_second_idle() {
        let open = Duration::from_secs(10);
        assert_eq!(thumbnail_local_limit(8, 16, Duration::from_millis(999), open), 2);
        assert_eq!(thumbnail_local_limit(8, 16, Duration::from_secs(1), open), 8);
        assert_eq!(thumbnail_local_limit(32, 12, Duration::from_secs(2), open), 12);
        assert_eq!(thumbnail_local_limit(1, 12, Duration::ZERO, open), 1);
    }

    #[test]
    fn local_parallelism_warms_up_after_folder_open() {
        let idle_input = Duration::from_secs(5);
        assert_eq!(thumbnail_local_limit(8, 16, idle_input, Duration::from_millis(100)), 1);
        assert_eq!(thumbnail_local_limit(8, 16, idle_input, Duration::from_millis(500)), 2);
        assert_eq!(thumbnail_local_limit(8, 16, idle_input, Duration::from_secs(2)), 8);
    }

    #[test]
    fn queue_prefers_missing_and_respects_network_capacity() {
        let local_stale = PathBuf::from("/data/stale.zip");
        let network_missing = PathBuf::from("/run/user/1000/gvfs/share/missing.zip");
        let local_missing = PathBuf::from("/data/missing.zip");
        let mut queue = VecDeque::from([
            local_stale.clone(), network_missing.clone(), local_missing.clone(),
        ]);
        let queued = HashSet::from([
            local_stale.clone(), network_missing.clone(), local_missing.clone(),
        ]);
        let missing = HashSet::from([network_missing, local_missing.clone()]);
        assert_eq!(
            take_allowed_thumbnail(&mut queue, &queued, &missing, true, true, false),
            Some(local_missing),
        );
    }
}
