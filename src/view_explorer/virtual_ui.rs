//! 仮想フォルダタブのUI（ツリー・ピッカー・確認/削除ダイアログ）。
//!
//! 仮想ツリーは `virtual_folders`（DB）から読む。登録の実処理は `register`、削除は `delete`、
//! キー操作は `keys` サブモジュール。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::fs::mount::MountEntry;

mod broken;
mod delete;
mod keys;
mod register;
mod rename;
mod sync;
use crate::gui_config::VirtualPosition;
use broken::BrokenCheck;
use delete::DeleteTarget;
use register::{LargeImport, OverlapInfo, PendingRegister};
use rename::RenameDialog;

use crate::i18n;

use super::*;

const GREEN: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const BLUE: egui::Color32 = egui::Color32::from_rgb(70, 130, 230);

/// 仮想ルート `/` を表す予約id（実データ層の `ROOT_ID` と同じ規約）。
const ROOT: u32 = 0;

/// 仮想ツリー描画用のノード（DBの仮想ノードから作る）。
#[derive(Clone)]
struct TreeNode {
    id: u32,
    parent: u32,
    name: String,
    real: PathBuf,
    /// 兄弟の中での登録順（DBの `order`）
    order: u32,
}

/// 登録ピッカー。OK/キャンセルは無く、ダブルクリックで確認ダイアログへ進む。
enum Picker {
    /// 仮想ツリー上の右クリック「実フォルダ登録」から。実フォルダを選ぶ（登録先は `dest`）。
    Real { dest: u32, tree: RealTreeState },
    /// 実ツリー上の右クリック「仮想フォルダに追加する」から。追加元 `src` の登録先の仮想フォルダを選ぶ。
    VirtualDest { src: PathBuf, expanded: HashSet<u32>, selected: Option<u32> },
}

/// ピッカー内の実フォルダツリー。メインの実ツリーとは独立した状態（ドライブ・展開・子フォルダ）を持つ。
/// 子フォルダは展開時に非同期で読む（`spawn_scan_subdirs`）。
struct RealTreeState {
    drive: usize,
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<PathBuf>>,
    loading: Vec<(PathBuf, mpsc::Receiver<Vec<PathBuf>>)>,
    selected: Option<PathBuf>,
}

impl RealTreeState {
    /// 初期ドライブは現在の実ツリーのルートに一致するドライブ（無ければ先頭）。ルート直下だけを展開する。
    fn new(drives: &[MountEntry], tree_root: &Path, ctx: &egui::Context) -> Self {
        let drive = drives.iter().position(|d| d.path == tree_root).unwrap_or(0);
        let mut t = Self::empty(drive, Self::drive_root(drives, drive, tree_root));
        t.request_children(t.root.clone(), ctx);
        t
    }

    fn drive_root(drives: &[MountEntry], drive: usize, fallback: &Path) -> PathBuf {
        drives.get(drive).map(|d| d.path.clone()).unwrap_or_else(|| fallback.to_path_buf())
    }

    fn empty(drive: usize, root: PathBuf) -> Self {
        Self {
            drive,
            expanded: HashSet::from([root.clone()]),
            root,
            children: HashMap::new(),
            loading: Vec::new(),
            selected: None,
        }
    }

    /// ドライブを切り替える。展開・子フォルダ・選択は捨てて、新しいルート直下を読み直す。
    fn set_drive(&mut self, drives: &[MountEntry], drive: usize, ctx: &egui::Context) {
        let root = Self::drive_root(drives, drive, &self.root);
        *self = Self::empty(drive, root);
        self.request_children(self.root.clone(), ctx);
    }

    /// path の子フォルダ一覧を非同期で読み始める（読み込み済み・読み込み中なら何もしない）。
    fn request_children(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.children.contains_key(&path) || self.loading.iter().any(|(p, _)| *p == path) {
            return;
        }
        let c = ctx.clone();
        let rx = crate::fs::dir::spawn_scan_subdirs(path.clone(), move || c.request_repaint());
        self.loading.push((path, rx));
    }

    /// 読み込み完了した子フォルダ一覧を取り込む。
    fn poll(&mut self) {
        let mut i = 0;
        while i < self.loading.len() {
            match self.loading[i].1.try_recv() {
                Ok(list) => {
                    let (path, _) = self.loading.remove(i);
                    self.children.insert(path, list);
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // 読み込みスレッドが結果を返さず終わった: 子なしとして扱う
                    let (path, _) = self.loading.remove(i);
                    self.children.insert(path, Vec::new());
                }
            }
        }
    }

    fn toggle_expand(&mut self, path: PathBuf, ctx: &egui::Context) {
        if !self.expanded.remove(&path) {
            self.request_children(path.clone(), ctx);
            self.expanded.insert(path);
        }
    }
}

#[derive(Clone)]
struct Confirm {
    src: PathBuf,
    dest: u32,
    /// 既存ノードとの重複関係（確認ダイアログの警告表示用）
    overlaps: OverlapInfo,
}

pub(super) struct VirtualState {
    /// DBの仮想ノード（`order` 順）。仮想タブに入るたびに `refresh_virtual_nodes` で読み直す。
    nodes: Vec<TreeNode>,
    expanded: HashSet<u32>,
    real_pane_open: bool,
    /// キー操作用のカーソル位置（実ツリーの tree_cursor 相当。開くのは Enter のとき）
    cursor: Option<u32>,
    /// キー移動した直後の1フレームだけ、カーソル行が見える位置へスクロールする
    scroll_to_cursor: bool,
    picker: Option<Picker>,
    confirm: Option<Confirm>,
    delete: Option<DeleteTarget>,
    /// 評価・スキャン中の登録（1件ずつ）
    register_pending: Option<PendingRegister>,
    /// 走査が終わって、取り込み方（全部／浅く）の選択待ちの大量登録
    large_import: Option<LargeImport>,
    rename: Option<RenameDialog>,
    /// リンク切れ（実パスがフォルダとして開けない）と判定されたノードid。目印の表示に使う。
    broken: HashSet<u32>,
    /// 実フォルダの更新日時（フォルダカードの日付ソート用）。リンク切れ判定と同じスレッドで集める。
    mtimes: HashMap<u32, std::time::SystemTime>,
    broken_check: Option<BrokenCheck>,
    broken_gen: u64,
}

impl VirtualState {
    pub(super) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            // 仮想ルート `/` は最初から展開しておく
            expanded: HashSet::from([ROOT]),
            real_pane_open: false,
            cursor: None,
            scroll_to_cursor: false,
            picker: None,
            confirm: None,
            delete: None,
            register_pending: None,
            large_import: None,
            rename: None,
            broken: HashSet::new(),
            mtimes: HashMap::new(),
            broken_check: None,
            broken_gen: 0,
        }
    }

    fn virtual_path(&self, id: u32) -> String {
        let mut names = Vec::new();
        let mut cur = id;
        while cur != ROOT {
            let Some(n) = self.nodes.iter().find(|n| n.id == cur) else { break };
            names.push(n.name.as_str());
            cur = n.parent;
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    fn subtree_ids(&self, id: u32) -> Vec<u32> {
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i];
            out.extend(self.nodes.iter().filter(|n| n.parent == cur).map(|n| n.id));
            i += 1;
        }
        out
    }
}

enum TreeEvent {
    Toggle(u32),
    Select(u32),
    DoubleClick(u32),
    Register(u32),
    Rename(u32),
    /// 実ツリーを、このノードの実パスまで展開して選択表示にする
    Sync(u32),
    /// フォルダタブへ移り、このノードの実フォルダを選択済みにして開く
    OpenInFolders(u32),
    Delete(u32),
    /// ツリー全体の並び条件（ノードには依存しない）
    SortSetting,
}

/// 並び条件で `nodes` を並べ替える。名前・日付は同じ親の兄弟どうしの相対順だけが意味を持つ。
/// 日付は実フォルダの更新日時で、不明（リンク切れなど）は昇降どちらでも末尾。
fn sort_tree_nodes(
    nodes: Vec<TreeNode>,
    sort: crate::tree_sort::TreeSort,
    mtimes: &HashMap<u32, std::time::SystemTime>,
) -> Vec<TreeNode> {
    use crate::tree_sort::TreeSortKey;
    use crate::types::ExplorerSortKey;
    match sort.key {
        TreeSortKey::Registration => {
            let mut nodes = nodes;
            nodes.sort_by_key(|n| (n.order, n.id));
            if !sort.ascending {
                nodes.reverse();
            }
            nodes
        }
        TreeSortKey::Name => {
            super::folder_sort::sort_folders(nodes, ExplorerSortKey::Name, sort.ascending, |n| (n.name.clone(), None))
        }
        TreeSortKey::Date => super::folder_sort::sort_folders(nodes, ExplorerSortKey::Date, sort.ascending, |n| {
            (n.name.clone(), mtimes.get(&n.id).copied())
        }),
    }
}

fn children_of(nodes: &[TreeNode], parent: u32) -> Vec<&TreeNode> {
    nodes.iter().filter(|n| n.parent == parent).collect()
}

/// 仮想ツリーの右クリックメニュー。`target` が行のid、行の外（ツリー内の余白）は None。
/// 行の外では、ツリー全体に効く「ソート条件設定」だけが有効で、ほかはグレーアウトする。
fn tree_context_menu(ui: &mut egui::Ui, target: Option<u32>, broken: &HashSet<u32>, out: &mut Vec<TreeEvent>) {
    // ルートは名前変更・削除の対象外。区切り線で「変更」「登録」「削除」「ソート」を分ける
    let node = target.filter(|id| *id != ROOT);
    if ui.add_enabled(node.is_some(), egui::Button::new(i18n::t().virtual_menu_rename())).clicked() {
        if let Some(id) = node {
            out.push(TreeEvent::Rename(id));
        }
        ui.close();
    }
    // 実パスへ辿れないノード（ルート・リンク切れ）は同期・フォルダタブで開くの対象外
    let syncable = node.filter(|id| !broken.contains(id));
    if ui.add_enabled(syncable.is_some(), egui::Button::new(i18n::t().virtual_menu_sync())).clicked() {
        if let Some(id) = syncable {
            out.push(TreeEvent::Sync(id));
        }
        ui.close();
    }
    if ui.add_enabled(syncable.is_some(), egui::Button::new(i18n::t().virtual_menu_open_in_folders())).clicked() {
        if let Some(id) = syncable {
            out.push(TreeEvent::OpenInFolders(id));
        }
        ui.close();
    }
    ui.separator();
    if ui.add_enabled(target.is_some(), egui::Button::new(i18n::t().virtual_menu_register())).clicked() {
        if let Some(id) = target {
            out.push(TreeEvent::Register(id));
        }
        ui.close();
    }
    ui.separator();
    if ui.add_enabled(node.is_some(), egui::Button::new(i18n::t().virtual_menu_delete())).clicked() {
        if let Some(id) = node {
            out.push(TreeEvent::Delete(id));
        }
        ui.close();
    }
    ui.separator();
    if ui.button(i18n::t().tree_sort_menu()).clicked() {
        out.push(TreeEvent::SortSetting);
        ui.close();
    }
}

/// ダミーツリー描画。`root_label` が Some なら仮想ルート行（id=ROOT）を先頭に描く。
/// `menu_on` は仮想ツリー本体用の右クリックメニュー（実フォルダ登録／仮想フォルダ削除）。
fn draw_tree(
    ui: &mut egui::Ui,
    nodes: &[TreeNode],
    root_label: Option<&str>,
    expanded: &HashSet<u32>,
    selected: Option<u32>,
    menu_on: bool,
    ring: Option<u32>,
    scroll_to_ring: bool,
    broken: &HashSet<u32>,
    out: &mut Vec<TreeEvent>,
) {
    match root_label {
        Some(label) => draw_tree_row(ui, nodes, ROOT, label, None, 0, expanded, selected, menu_on, ring, scroll_to_ring, broken, out),
        None => {
            for n in children_of(nodes, ROOT) {
                draw_tree_row(ui, nodes, n.id, &n.name, Some(&n.real), 0, expanded, selected, menu_on, ring, scroll_to_ring, broken, out);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tree_row(
    ui: &mut egui::Ui,
    nodes: &[TreeNode],
    id: u32,
    label: &str,
    real: Option<&Path>,
    depth: usize,
    expanded: &HashSet<u32>,
    selected: Option<u32>,
    menu_on: bool,
    ring: Option<u32>,
    scroll_to_ring: bool,
    broken: &HashSet<u32>,
    out: &mut Vec<TreeEvent>,
) {
    let has_children = nodes.iter().any(|n| n.parent == id);
    let is_expanded = expanded.contains(&id);
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 12.0);
        if has_children {
            let arrow = if is_expanded { "▼" } else { "▶" };
            if ui.add(egui::Label::new(arrow).sense(egui::Sense::click())).clicked() {
                out.push(TreeEvent::Toggle(id));
            }
        } else {
            ui.add_space(12.0);
        }
        ui.add_space(4.0);
        // リンク切れは「⚠ 名前」と薄い色で示す
        let is_broken = broken.contains(&id);
        let text = if is_broken {
            egui::RichText::new(format!("⚠ {label}")).weak()
        } else {
            egui::RichText::new(label)
        };
        let mut r = ui.selectable_label(selected == Some(id), text);
        if let Some(p) = real {
            let mut tip = p.display().to_string();
            if is_broken {
                tip.push('\n');
                tip.push_str(i18n::t().virtual_link_broken_label());
            }
            r = r.on_hover_text(tip);
        }
        if ring == Some(id) {
            super::panels::draw_cursor_ring(ui, r.rect);
            if scroll_to_ring {
                r.scroll_to_me(None);
            }
        }
        if r.clicked() || r.secondary_clicked() {
            out.push(TreeEvent::Select(id));
        }
        if r.double_clicked() {
            out.push(TreeEvent::DoubleClick(id));
        }
        if menu_on {
            r.context_menu(|ui| tree_context_menu(ui, Some(id), broken, out));
        }
    });
    if is_expanded {
        for c in children_of(nodes, id) {
            draw_tree_row(ui, nodes, c.id, &c.name, Some(&c.real), depth + 1, expanded, selected, menu_on, ring, scroll_to_ring, broken, out);
        }
    }
}

/// 仮想ノード `id` を表示中のグリッド先頭部分（「↑」→ 仮想の子）の純粋な組み立て。
/// 「↑」は仮想の親（`/` では無し）、子はファイルカードと同じソートキー・昇降（名前は仮想名、日付は実フォルダの更新日時）。
fn virtual_folder_entries(
    nodes: &[TreeNode],
    id: u32,
    show_hidden: bool,
    key: crate::types::ExplorerSortKey,
    ascending: bool,
    mtimes: &HashMap<u32, std::time::SystemTime>,
) -> Vec<GridEntry> {
    let mut out = Vec::new();
    if id != ROOT {
        if let Some(n) = nodes.iter().find(|n| n.id == id) {
            out.push(GridEntry::VirtualUp(n.parent));
        }
    }
    let kids: Vec<&TreeNode> = children_of(nodes, id)
        .into_iter()
        .filter(|n| {
            show_hidden
                || !n.real.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with('.'))
        })
        .collect();
    // ファイルカードと同じソートキー・昇降に従う（サイズは名前順、日付は実フォルダの更新日時）
    let kids = super::folder_sort::sort_folders(kids, key, ascending, |n| (n.name.clone(), mtimes.get(&n.id).copied()));
    out.extend(kids.into_iter().map(|n| GridEntry::VirtualSubdir(n.id)));
    out
}

/// 保存された仮想ノードを、idと実パスが一致するときだけ採用する（idの再利用で別のノードを開かない）。
/// 一致しない・未保存は `/`。
fn resolve_virtual(saved: Option<&VirtualPosition>, nodes: &[TreeNode]) -> u32 {
    match saved {
        Some(p) if nodes.iter().any(|n| n.id == p.id && n.real == p.path) => p.id,
        _ => ROOT,
    }
}

fn toggle(set: &mut HashSet<u32>, id: u32) {
    if !set.remove(&id) {
        set.insert(id);
    }
}

impl NekoviewApp {
    /// アイテムカード欄の外枠色。仮想タブ限定: 実ツリー選択=青、仮想フォルダ選択=緑。他タブは枠なし。
    pub(super) fn card_border_color(&self) -> Option<egui::Color32> {
        match self.folder_pane_tab {
            FolderPaneTab::VirtualFolders => {
                Some(if self.viewing_virtual_node.is_some() { GREEN } else { BLUE })
            }
            _ => None,
        }
    }

    /// 仮想タブ限定で、実ツリー（real_pane=true）／仮想ツリー（false）ペインを囲う2px枠を描く。
    /// 今のカード欄の表示元と同じ側のペインだけに付く（実=青、仮想=緑）。
    pub(super) fn paint_pane_border(&self, ui: &egui::Ui, rect: egui::Rect, real_pane: bool) {
        if self.folder_pane_tab != FolderPaneTab::VirtualFolders {
            return;
        }
        let from_real = self.viewing_virtual_node.is_none();
        if from_real != real_pane {
            return;
        }
        let color = if real_pane { BLUE } else { GREEN };
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Inside);
    }

    pub(super) fn virtual_real_pane_open(&self) -> bool {
        self.virtual_state.real_pane_open
    }

    /// 仮想フォルダのテキスト入力ダイアログ（名前変更）が開いている間は、毎フレームの
    /// ネイティブフォーカス解除を止める（解除すると入力欄がフォーカスを保てない）。
    pub(super) fn virtual_text_input_open(&self) -> bool {
        self.virtual_state.rename.is_some()
    }

    /// 実ツリー右クリック「仮想フォルダに追加する」。追加先の仮想フォルダを選ぶピッカーを開く。
    pub(super) fn open_virtual_dest_picker(&mut self, src: PathBuf) {
        let expanded = self.virtual_state.expanded.clone();
        self.virtual_state.confirm = None;
        self.virtual_state.picker = Some(Picker::VirtualDest { src, expanded, selected: None });
    }

    /// DBから仮想ノードを読み直す（兄弟は `order` 順）。DBが無ければ空のツリー。
    /// 消えたノードを指す展開・選択状態は捨てる。
    pub(super) fn refresh_virtual_nodes(&mut self) {
        let mut list = self
            .spread_db
            .as_ref()
            .map(crate::virtual_folders::list_nodes)
            .unwrap_or_default();
        list.sort_by_key(|n| n.order);
        self.virtual_state.nodes = list
            .into_iter()
            .map(|n| TreeNode { id: n.id, parent: n.parent_id, name: n.name, real: n.real_path, order: n.order })
            .collect();
        self.resort_virtual_nodes();
        let ids: HashSet<u32> = self.virtual_state.nodes.iter().map(|n| n.id).collect();
        self.virtual_state.expanded.retain(|id| *id == ROOT || ids.contains(id));
        // 表示中のノードがDBから消えていたら実表示に戻す
        if self.viewing_virtual_node.is_some_and(|id| id != ROOT && !ids.contains(&id)) {
            self.exit_virtual_view();
        }
        // ノードの顔ぶれが変わったので、リンク切れの判定をやり直す（別スレッド）
        self.start_broken_check();
    }

    /// 仮想ツリーの並び条件（ソート条件設定）で `nodes` を並べ直す。兄弟の並びは `nodes` の順序で決まり、
    /// 描画・キー移動・ピッカーがそれに従う。更新日時が届いたとき・設定を変えたときにも呼ぶ。
    pub(super) fn resort_virtual_nodes(&mut self) {
        let sort = self.tree_sorts.virtual_tree_or_default();
        let nodes = std::mem::take(&mut self.virtual_state.nodes);
        self.virtual_state.nodes = sort_tree_nodes(nodes, sort, &self.virtual_state.mtimes);
    }

    /// 仮想ノードを選んで中央グリッドをその表示にする（ツリークリック・フォルダカード・Enter共通）。
    /// 既にそのノードを表示中なら何もしない（実ツリー側に移っていれば viewing_virtual_node は None なので通る）。
    pub(super) fn select_virtual_node(&mut self, id: u32) {
        // キー操作のカーソルも開いたノードに揃える
        self.virtual_state.cursor = Some(id);
        if self.viewing_virtual_node == Some(id) {
            return;
        }
        // ツリー上で見えるよう、祖先ノードを展開しておく
        let mut cur = self.virtual_state.nodes.iter().find(|n| n.id == id).map(|n| n.parent);
        while let Some(parent) = cur {
            self.virtual_state.expanded.insert(parent);
            cur = if parent == ROOT {
                None
            } else {
                self.virtual_state.nodes.iter().find(|n| n.id == parent).map(|n| n.parent)
            };
        }
        self.enter_virtual_node(id);
    }

    /// 仮想ノードの表示を開く（同じノードでも開き直す）。構造は仮想が正、ファイルは実パスを実スキャン。
    fn enter_virtual_node(&mut self, id: u32) {
        if id == ROOT {
            // 仮想ルート `/` は実パスを持たない。最上位ノードのフォルダカードだけを出す
            let changed = self.remember_virtual_position(None);
            self.viewing_virtual_node = Some(ROOT);
            self.virtual_link_broken = false;
            self.show_empty_listing();
            if changed {
                self.persist_state();
            }
            return;
        }
        let Some(real) = self.virtual_state.nodes.iter().find(|n| n.id == id).map(|n| n.real.clone()) else {
            return;
        };
        // 仮想タブの最後の位置（次回の復元用）。到達できる場合は下の persist_state で一緒に書かれる
        let changed = self.remember_virtual_position(Some(VirtualPosition { id, path: real.clone() }));
        self.viewing_virtual_node = Some(id);
        // ネットワークマウント配下は同期I/Oを避け、確認済みの到達可否で判定する（リロードと同じ方針）
        if self.path_reachable(&real) {
            self.virtual_link_broken = false;
            // current_dir が仮想ノードの実パスに移る。実ツリータブ自身の位置は退避しておく
            self.real_dir_stash.stash_if_empty(&self.current_dir);
            self.begin_dir_view(real);
            self.persist_state();
        } else {
            self.virtual_link_broken = true;
            self.show_empty_listing();
            if changed {
                self.persist_state();
            }
        }
    }

    /// 仮想タブの最後の位置を更新する（`/` は既定なので None）。変わったら true（書き込みは呼び出し側）。
    fn remember_virtual_position(&mut self, pos: Option<VirtualPosition>) -> bool {
        if self.tab_positions.virtual_node == pos {
            return false;
        }
        self.tab_positions.virtual_node = pos;
        true
    }

    /// 仮想タブに入ったとき: 保存されたノード（idと実パスが一致するもの。無ければ `/`）を開く。
    pub(super) fn restore_virtual_position(&mut self) {
        let id = resolve_virtual(self.tab_positions.virtual_node.as_ref(), &self.virtual_state.nodes);
        self.select_virtual_node(id);
    }

    /// 仮想ノード経由の表示を終えて実ディレクトリ表示に戻す（exit_favorite_view と同じ考え方）。
    /// current_dir はそのまま実スキャンし直す。
    pub(super) fn exit_virtual_view(&mut self) {
        if self.viewing_virtual_node.is_none() {
            return;
        }
        self.viewing_virtual_node = None;
        self.virtual_link_broken = false;
        // 仮想ノードの実パスに移っていた current_dir を、実ツリータブ自身の位置に戻す
        if let Some(real) = self.real_dir_stash.take() {
            self.current_dir = real;
        }
        self.viewing_dir = Some(self.current_dir.clone());
        self.start_scan();
    }

    /// リロード用。仮想ノード経由の表示中はDBから読み直して同じノードを開き直し、
    /// そうでなければ通常どおり current_dir を実スキャンする。
    pub(super) fn reload_virtual_or_scan(&mut self) {
        if self.viewing_virtual_node.is_none() {
            self.start_scan();
            return;
        }
        self.refresh_virtual_nodes();
        match self.viewing_virtual_node {
            Some(id) => self.enter_virtual_node(id),
            // 表示中のノードが消えていた場合は refresh 側で exit_virtual_view 済み
            None => {}
        }
    }

    /// 中央グリッドのヘッダ（仮想パス → 実パス）。仮想ノード経由の表示でなければ None。
    pub(super) fn virtual_header_text(&self) -> Option<String> {
        let id = self.viewing_virtual_node?;
        let vpath = self.virtual_state.virtual_path(id);
        if id == ROOT {
            return Some(vpath);
        }
        let real = self.virtual_state.nodes.iter().find(|n| n.id == id)?.real.display().to_string();
        let mut text = format!("{vpath}  →  {real}");
        if self.virtual_link_broken {
            text.push_str("  ");
            text.push_str(i18n::t().virtual_link_broken_label());
        }
        Some(text)
    }

    /// 仮想ノード表示中のグリッド先頭部分: 「↑」（仮想の親。`/` では無し）→ 仮想の子（名前順）。
    /// 非表示（ドット始まり）フォルダの扱いは実表示と同じ（実フォルダ名で判定）。
    pub(super) fn virtual_folder_grid_entries(&self, id: u32) -> Vec<GridEntry> {
        virtual_folder_entries(
            &self.virtual_state.nodes,
            id,
            self.show_hidden,
            self.sort_key,
            self.sort_ascending,
            &self.virtual_state.mtimes,
        )
    }

    /// 仮想ノード表示中の「↑」カード。ダブルクリックされたら true。
    pub(super) fn draw_virtual_up_card(
        &mut self,
        ui: &mut egui::Ui,
        cell_w: f32,
        cell_h: f32,
        target: u32,
        grid_focused: bool,
    ) -> bool {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(cell_w, cell_h), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            ui.painter().rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
            nav_icons::draw_up_icon(ui.painter(), rect, nav_icons::NAV_ICON_COLOR);
        }
        self.finish_virtual_card(ui, rect, &response, GridEntry::VirtualUp(target), grid_focused)
    }

    /// 仮想ノード表示中のフォルダカード。ダブルクリックされたら true。
    pub(super) fn draw_virtual_folder_card(
        &mut self,
        ui: &mut egui::Ui,
        cell_w: f32,
        cell_h: f32,
        node_id: u32,
        grid_focused: bool,
    ) -> bool {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(cell_w, cell_h), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            if let Some(n) = self.virtual_state.nodes.iter().find(|n| n.id == node_id).cloned() {
                let is_broken = self.virtual_state.broken.contains(&node_id);
                // 1秒ホバーのツールチップは「仮想名＋実パス（＋リンク切れ）」
                let mut tooltip = format!("{}\n{}", n.name, n.real.display());
                if is_broken {
                    tooltip.push('\n');
                    tooltip.push_str(i18n::t().virtual_link_broken_label());
                }
                self.draw_folder_card_face(ui, rect, &response, cell_w, cell_h, &n.name, &tooltip, &n.real);
                if is_broken {
                    // リンク切れの目印: 右上（下端はラベルの位置）。ネットワーク切れの右下マーカーと同じ色
                    ui.painter().text(
                        egui::pos2(rect.max.x - 4.0, rect.min.y + 4.0),
                        egui::Align2::RIGHT_TOP,
                        "⚠",
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_rgb(220, 160, 40),
                    );
                }
            }
        }
        self.finish_virtual_card(ui, rect, &response, GridEntry::VirtualSubdir(node_id), grid_focused)
    }

    /// 仮想カード共通の後処理（カーソルリング・クリック選択・右クリックメニュー）。
    fn finish_virtual_card(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
        entry: GridEntry,
        grid_focused: bool,
    ) -> bool {
        if grid_focused && self.grid_cursor.as_ref() == Some(&entry) {
            super::panels::draw_cursor_ring(ui, rect);
        }
        if response.clicked() {
            self.focused_pane = FocusPane::Grid;
            self.grid_cursor = Some(entry);
            self.selected_archive_index = None;
            self.selected_archive_meta = None;
        }
        // 実パスを持たない表示（`/`・リンク切れ）では viewing_dir が None で、開く対象が無い
        if let Some(dir) = self.viewing_dir.clone() {
            response.context_menu(|ui| {
                if ui.button(i18n::t().explorer_open_folder_menu()).clicked() {
                    crate::translate::open_in_file_manager(&dir);
                    ui.close();
                }
            });
        }
        response.double_clicked()
    }

    pub(super) fn draw_virtual_folder_pane(&mut self, ui: &mut egui::Ui) {
        let mut events = Vec::new();
        let scroll_to_cursor = std::mem::take(&mut self.virtual_state.scroll_to_cursor);
        // ツリー領域の全面を先に右クリック対象にしておく。行はこの上に描かれるので行が優先され、
        // 行のない余白（行の右側・下側）への右クリックだけがここに届く。
        let bg = ui.interact(ui.available_rect_before_wrap(), ui.id().with("virtual_tree_bg"), egui::Sense::click());
        egui::ScrollArea::both()
            .id_salt("virtual_tree_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let m = &self.virtual_state;
                // 表示中のノードを選択表示にする。フォーカス中はカーソル位置にカーソルリングを出す
                let viewing = self.viewing_virtual_node;
                let ring = (self.focused_pane == FocusPane::VirtualTab).then(|| self.virtual_cursor());
                draw_tree(ui, &m.nodes, Some("/"), &m.expanded, viewing, true, ring, scroll_to_cursor, &m.broken, &mut events);
            });
        bg.context_menu(|ui| tree_context_menu(ui, None, &self.virtual_state.broken, &mut events));
        if !events.is_empty() {
            self.focused_pane = FocusPane::VirtualTab;
        }
        for ev in events {
            match ev {
                TreeEvent::Toggle(id) => toggle(&mut self.virtual_state.expanded, id),
                TreeEvent::Select(id) => self.select_virtual_node(id),
                TreeEvent::DoubleClick(_) => {}
                TreeEvent::Rename(id) => self.open_rename_dialog(id),
                TreeEvent::Sync(id) => self.sync_real_tree(id),
                TreeEvent::OpenInFolders(id) => self.open_in_folders_tab(id),
                TreeEvent::SortSetting => {
                    self.virtual_state.rename = None;
                    self.open_tree_sort_dialog(super::tree_sort_ui::TreeSortTarget::Virtual);
                }
                TreeEvent::Register(id) => {
                    self.virtual_state.confirm = None;
                    self.virtual_state.rename = None;
                    let tree = RealTreeState::new(&self.drives, &self.tree_root, &self.egui_ctx);
                    self.virtual_state.picker = Some(Picker::Real { dest: id, tree });
                }
                TreeEvent::Delete(id) => {
                    self.virtual_state.rename = None;
                    if id != ROOT {
                        if let Some(n) = self.virtual_state.nodes.iter().find(|n| n.id == id) {
                            self.virtual_state.delete =
                                Some(DeleteTarget { id, real: n.real.clone(), name: n.name.clone() });
                        }
                    }
                }
            }
        }
    }

    /// 仮想ツリーと中央エリアの境界に置く伸縮グリップ。クリックで実ツリーペインを開閉する。
    pub(super) fn draw_virtual_grip(&mut self, ui: &mut egui::Ui, avail_h: f32) {
        const GRIP_W: f32 = 12.0;
        const GRIP_H: f32 = 56.0;
        ui.add_space(((avail_h - GRIP_H) / 2.0).max(0.0));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(GRIP_W, GRIP_H), egui::Sense::click());
        let open = self.virtual_state.real_pane_open;
        let resp = resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if open { i18n::t().virtual_grip_close() } else { i18n::t().virtual_grip_open() });
        let visuals = ui.visuals();
        let fill = if resp.hovered() { visuals.widgets.hovered.bg_fill } else { visuals.widgets.inactive.bg_fill };
        ui.painter().rect_filled(rect, 3.0, fill);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if open { "◀" } else { "▶" },
            egui::FontId::proportional(10.0),
            visuals.text_color(),
        );
        if resp.clicked() {
            self.virtual_state.real_pane_open = !open;
            // 実ツリー側にフォーカスがあるまま閉じると行き場がなくなるため仮想ツリーへ戻す
            if open && matches!(self.focused_pane, FocusPane::TreeTab | FocusPane::Drives) {
                self.focused_pane = FocusPane::VirtualTab;
            }
        }
    }

    pub(super) fn draw_virtual_dialogs(&mut self, ctx: &egui::Context) {
        self.poll_broken_check();
        self.poll_virtual_register();
        self.draw_virtual_picker(ctx);
        self.draw_virtual_confirm(ctx);
        self.draw_virtual_large_import(ctx);
        self.draw_virtual_rename(ctx);
        self.draw_virtual_delete(ctx);
    }

    pub(super) fn set_toast(&mut self, msg: impl Into<String>) {
        self.app_toast = Some((msg.into(), std::time::Instant::now()));
    }

    /// 実フォルダ／追加先仮想フォルダの選択ダイアログ。OK/キャンセルは無く、
    /// ダブルクリックで確認ダイアログへ進む。右上のXで閉じる。
    fn draw_virtual_picker(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.virtual_state.picker.take() else { return };
        if let Picker::Real { tree, .. } = &mut p {
            tree.poll();
        }
        let confirm_open = self.virtual_state.confirm.is_some();
        let title = match p {
            Picker::Real { .. } => i18n::t().virtual_picker_title_real(),
            Picker::VirtualDest { .. } => i18n::t().virtual_picker_title_dest(),
        };
        let mut open = true;
        let mut events = Vec::new();
        let mut real_action = TreeAction::None;
        let mut picked_drive = None;
        egui::Window::new(title)
            .id(egui::Id::new("virtual_picker_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([360.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.add_enabled_ui(!confirm_open, |ui| {
                    if let Picker::Real { tree, .. } = &p {
                        let current = self.drives.get(tree.drive).map_or(String::new(), |d| d.label.clone());
                        egui::ComboBox::from_id_salt("virtual_picker_drive")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (i, d) in self.drives.iter().enumerate() {
                                    if ui.selectable_label(tree.drive == i, &d.label).clicked() {
                                        picked_drive = Some(i);
                                    }
                                }
                            });
                        ui.separator();
                    }
                    egui::ScrollArea::both()
                        .id_salt("virtual_picker_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match &p {
                            Picker::Real { tree, .. } => {
                                let mut scroll_pending = false;
                                super::panels::show_tree_node(
                                    ui,
                                    &tree.root,
                                    0,
                                    &tree.selected,
                                    &None,
                                    false,
                                    &tree.expanded,
                                    &tree.children,
                                    false,
                                    super::panels::TreeMenu::NONE,
                                    &mut real_action,
                                    &mut scroll_pending,
                                );
                            }
                            Picker::VirtualDest { expanded, selected, .. } => {
                                draw_tree(ui, &self.virtual_state.nodes, Some("/"), expanded, *selected, false, None, false, &self.virtual_state.broken, &mut events);
                            }
                        });
                });
            });
        if !open {
            self.virtual_state.confirm = None;
            return;
        }
        match &mut p {
            Picker::Real { dest, tree } => {
                if let Some(i) = picked_drive {
                    tree.set_drive(&self.drives, i, ctx);
                }
                match real_action {
                    TreeAction::ToggleExpand(path) => tree.toggle_expand(path, ctx),
                    TreeAction::Navigate(path) => tree.selected = Some(path),
                    TreeAction::DoubleClick(path) => {
                        tree.selected = Some(path.clone());
                        self.begin_register(path, *dest);
                    }
                    TreeAction::AddToVirtual(_) | TreeAction::SortSetting | TreeAction::None => {}
                }
            }
            Picker::VirtualDest { src, expanded, selected } => {
                for ev in events {
                    match ev {
                        TreeEvent::Toggle(id) => toggle(expanded, id),
                        TreeEvent::Select(id) => *selected = Some(id),
                        TreeEvent::DoubleClick(id) => self.begin_register(src.clone(), id),
                        _ => {}
                    }
                }
            }
        }
        self.virtual_state.picker = Some(p);
    }

    /// 登録確認の固定ダイアログ。OK → 評価 → 登録 or 異常トースト（`register` サブモジュール）。
    fn draw_virtual_confirm(&mut self, ctx: &egui::Context) {
        let Some(c) = self.virtual_state.confirm.clone() else { return };
        let dest_path = self.virtual_state.virtual_path(c.dest);
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(i18n::t().virtual_confirm_title())
            .id(egui::Id::new("virtual_confirm_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().virtual_confirm_body());
                ui.add_space(6.0);
                ui.label(i18n::t().virtual_confirm_path(&c.src.display().to_string()));
                ui.label(i18n::t().virtual_confirm_dest(&dest_path));
                // 重複関係の警告（拒否はしない。同じ登録先の重複だけは登録時に拒否される）
                let o = &c.overlaps;
                let warn = egui::Color32::from_rgb(230, 160, 40);
                if o.same_here {
                    ui.colored_label(ui.visuals().error_fg_color, i18n::t().virtual_overlap_same_here());
                } else if o.same > 0 {
                    ui.colored_label(warn, i18n::t().virtual_overlap_same(o.same));
                }
                if o.ancestors > 0 {
                    ui.colored_label(warn, i18n::t().virtual_overlap_ancestor(o.ancestors));
                }
                if o.descendants > 0 {
                    ui.colored_label(warn, i18n::t().virtual_overlap_descendant(o.descendants));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ok = ui.button(i18n::t().virtual_ok()).clicked();
                    cancel = ui.button(i18n::t().favorite_dialog_cancel()).clicked();
                });
            });
        if cancel {
            self.virtual_state.confirm = None;
        } else if ok {
            // 正常・異常どちらでも確認とピッカーは閉じる（異常時は登録キャンセル扱いでトースト）
            self.virtual_state.confirm = None;
            self.virtual_state.picker = None;
            self.start_virtual_register(c);
        }
    }

    /// 仮想フォルダ削除の固定ダイアログ。子孫は常に連動削除（実フォルダには触れない）。
    fn draw_virtual_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.virtual_state.delete.as_ref().map(|t| t.id) else { return };
        let path = self.virtual_state.virtual_path(id);
        let descendants = self.virtual_state.subtree_ids(id).len().saturating_sub(1);
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(i18n::t().virtual_delete_title())
            .id(egui::Id::new("virtual_delete_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().virtual_delete_path(&path));
                if descendants > 0 {
                    ui.label(i18n::t().virtual_delete_descendants(descendants));
                }
                ui.label(i18n::t().virtual_delete_real_untouched());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ok = ui.button(i18n::t().virtual_ok()).clicked();
                    cancel = ui.button(i18n::t().favorite_dialog_cancel()).clicked();
                });
            });
        if cancel {
            self.virtual_state.delete = None;
        } else if ok {
            if let Some(target) = self.virtual_state.delete.take() {
                self.run_virtual_delete(target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ExplorerSortKey;

    fn node(id: u32, parent: u32, name: &str, real: &str) -> TreeNode {
        TreeNode { id, parent, name: name.to_string(), real: PathBuf::from(real), order: id }
    }

    fn sample() -> Vec<TreeNode> {
        vec![
            node(1, ROOT, "a_漫画", "/m/manga"),
            node(2, 1, "b_青年", "/m/manga/seinen"),
            node(3, 1, "a_少年", "/m/manga/shonen"),
            node(4, 1, "隠し", "/m/manga/.hidden"),
            node(5, ROOT, "b_写真", "/m/photo"),
        ]
    }

    fn saved(id: u32, path: &str) -> VirtualPosition {
        VirtualPosition { id, path: PathBuf::from(path) }
    }

    #[test]
    fn saved_virtual_node_is_restored_only_when_id_and_path_match() {
        let nodes = sample();
        assert_eq!(resolve_virtual(Some(&saved(2, "/m/manga/seinen")), &nodes), 2);
        // idは同じでも実パスが違う（idが再利用された別のノード）
        assert_eq!(resolve_virtual(Some(&saved(2, "/other")), &nodes), ROOT);
        // ノードが消えている・未保存は `/`
        assert_eq!(resolve_virtual(Some(&saved(99, "/m/manga")), &nodes), ROOT);
        assert_eq!(resolve_virtual(None, &nodes), ROOT);
    }

    #[test]
    fn root_has_no_up_and_lists_top_level_by_name() {
        let e = virtual_folder_entries(&sample(), ROOT, false, ExplorerSortKey::Name, true, &HashMap::new());
        assert_eq!(e, vec![GridEntry::VirtualSubdir(1), GridEntry::VirtualSubdir(5)]);
    }

    #[test]
    fn node_has_up_to_virtual_parent_and_name_sorted_children() {
        let e = virtual_folder_entries(&sample(), 1, false, ExplorerSortKey::Name, true, &HashMap::new());
        // 「↑」は仮想の親（ROOT=0）。子は仮想名の昇順、ドット始まりの実フォルダは非表示
        assert_eq!(e, vec![GridEntry::VirtualUp(ROOT), GridEntry::VirtualSubdir(3), GridEntry::VirtualSubdir(2)]);
    }

    #[test]
    fn descending_reverses_children_but_keeps_up_first() {
        let e = virtual_folder_entries(&sample(), 1, false, ExplorerSortKey::Name, false, &HashMap::new());
        assert_eq!(e, vec![GridEntry::VirtualUp(ROOT), GridEntry::VirtualSubdir(2), GridEntry::VirtualSubdir(3)]);
    }

    fn ids(nodes: &[TreeNode]) -> Vec<u32> {
        nodes.iter().map(|n| n.id).collect()
    }

    /// 兄弟（親1の子 2,3,4）だけを取り出した並び
    fn kids_of_1(nodes: &[TreeNode]) -> Vec<u32> {
        children_of(nodes, 1).iter().map(|n| n.id).collect()
    }

    #[test]
    fn tree_sort_registration_follows_order_and_reverses() {
        use crate::tree_sort::{TreeSort, TreeSortKey};
        let mut shuffled = sample();
        shuffled.reverse();
        let asc = sort_tree_nodes(shuffled.clone(), TreeSort { key: TreeSortKey::Registration, ascending: true }, &HashMap::new());
        assert_eq!(ids(&asc), vec![1, 2, 3, 4, 5]);
        let desc = sort_tree_nodes(shuffled, TreeSort { key: TreeSortKey::Registration, ascending: false }, &HashMap::new());
        assert_eq!(kids_of_1(&desc), vec![4, 3, 2]);
    }

    #[test]
    fn tree_sort_name_orders_siblings_by_virtual_name() {
        use crate::tree_sort::{TreeSort, TreeSortKey};
        // 子 2="b_青年" 3="a_少年" 4="隠し"
        let asc = sort_tree_nodes(sample(), TreeSort { key: TreeSortKey::Name, ascending: true }, &HashMap::new());
        assert_eq!(kids_of_1(&asc), vec![3, 2, 4]);
        let desc = sort_tree_nodes(sample(), TreeSort { key: TreeSortKey::Name, ascending: false }, &HashMap::new());
        assert_eq!(kids_of_1(&desc), vec![4, 2, 3]);
    }

    #[test]
    fn tree_sort_date_orders_by_mtime_with_unknown_last() {
        use crate::tree_sort::{TreeSort, TreeSortKey};
        use std::time::{Duration, SystemTime};
        let at = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        // 4は日時なし（リンク切れなど）
        let mtimes = HashMap::from([(2, at(30)), (3, at(10))]);
        let asc = sort_tree_nodes(sample(), TreeSort { key: TreeSortKey::Date, ascending: true }, &mtimes);
        assert_eq!(kids_of_1(&asc), vec![3, 2, 4]);
        let desc = sort_tree_nodes(sample(), TreeSort { key: TreeSortKey::Date, ascending: false }, &mtimes);
        assert_eq!(kids_of_1(&desc), vec![2, 3, 4]);
    }

    #[test]
    fn date_sort_uses_real_folder_mtime_and_puts_unknown_last() {
        use std::time::{Duration, SystemTime};
        let at = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        // ノード1の子は 2 と 3（表示名 "a"/"b" 順ではなく更新日時順になる）。3だけ日時なし（リンク切れなど）
        let mtimes = HashMap::from([(2, at(50))]);
        let kids = |asc| virtual_folder_entries(&sample(), 1, false, ExplorerSortKey::Date, asc, &mtimes);
        assert_eq!(kids(true), vec![GridEntry::VirtualUp(ROOT), GridEntry::VirtualSubdir(2), GridEntry::VirtualSubdir(3)]);
        assert_eq!(kids(false), vec![GridEntry::VirtualUp(ROOT), GridEntry::VirtualSubdir(2), GridEntry::VirtualSubdir(3)]);
    }

    #[test]
    fn show_hidden_includes_dot_folders() {
        let e = virtual_folder_entries(&sample(), 1, true, ExplorerSortKey::Name, true, &HashMap::new());
        assert_eq!(e.len(), 4);
        assert!(e.contains(&GridEntry::VirtualSubdir(4)));
    }

    #[test]
    fn child_up_points_to_its_own_parent_node() {
        let e = virtual_folder_entries(&sample(), 2, false, ExplorerSortKey::Name, true, &HashMap::new());
        assert_eq!(e, vec![GridEntry::VirtualUp(1)]);
    }

    #[test]
    fn unknown_node_yields_no_entries_except_children() {
        assert!(virtual_folder_entries(&sample(), 99, false, ExplorerSortKey::Name, true, &HashMap::new()).is_empty());
    }
}
