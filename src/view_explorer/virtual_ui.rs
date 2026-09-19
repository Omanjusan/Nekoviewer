//! 仮想フォルダタブのUI（フェーズ3a-1: ツリーはDB実データ、登録/削除の実行は未接続）。
//!
//! 仮想ツリーは `virtual_folders`（DB）から読む。登録・削除の確定処理は3b/3cで接続するまで
//! 「未接続」トーストで止めている。登録ピッカー内の実ツリー（ドライブコンボ付き）は
//! 3bまでダミーデータ。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::i18n;

use super::*;

const GREEN: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const BLUE: egui::Color32 = egui::Color32::from_rgb(70, 130, 230);

/// 仮想ルート `/` を表す予約id（実データ層の `ROOT_ID` と同じ規約）。
const ROOT: u32 = 0;

const DUMMY_DRIVES: [&str; 2] = ["/mnt/data", "/mnt/nas"];

/// ツリー描画用のノード（仮想ノードとピッカー内のダミー実ツリーで共用）。
#[derive(Clone)]
struct TreeNode {
    id: u32,
    parent: u32,
    name: String,
    real: PathBuf,
}

enum PickerKind {
    /// 仮想ツリー上の右クリック「実フォルダ登録」から。実フォルダを選ぶ（登録先は `dest`）。
    RealSource { dest: u32 },
    /// 実ツリー上の右クリック「仮想フォルダに追加する」から。登録先の仮想フォルダを選ぶ。
    VirtualDest { src: PathBuf },
}

struct Picker {
    kind: PickerKind,
    drive: usize,
    real_nodes: Vec<TreeNode>,
    expanded: HashSet<u32>,
    selected: Option<u32>,
}

#[derive(Clone)]
struct Confirm {
    src: PathBuf,
    dest: u32,
}

pub(super) struct VirtualState {
    /// DBの仮想ノード（`order` 順）。仮想タブに入るたびに `refresh_virtual_nodes` で読み直す。
    nodes: Vec<TreeNode>,
    expanded: HashSet<u32>,
    selected: Option<u32>,
    real_pane_open: bool,
    /// アイテムカード欄の表示元。true=実ツリー選択（青枠）、false=仮想フォルダ選択（緑枠）。
    card_from_real: bool,
    picker: Option<Picker>,
    confirm: Option<Confirm>,
    delete: Option<u32>,
}

impl VirtualState {
    pub(super) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            // 仮想ルート `/` は最初から展開しておく
            expanded: HashSet::from([ROOT]),
            selected: None,
            real_pane_open: false,
            card_from_real: false,
            picker: None,
            confirm: None,
            delete: None,
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

impl Picker {
    fn new(kind: PickerKind, expanded: HashSet<u32>) -> Self {
        Self { kind, drive: 0, real_nodes: dummy_real_tree(0), expanded, selected: None }
    }
}

/// ドライブごとのダミー実ツリー。`(深さ, 名前)` の先行順リストから組む。
fn dummy_real_tree(drive: usize) -> Vec<TreeNode> {
    let spec: &[(usize, &str)] = match drive {
        0 => &[
            (0, "data"), (1, "Manga"), (2, "shonen"), (2, "seinen"),
            (1, "Photo"), (2, "2024"), (2, "2025"), (1, "Work"),
        ],
        _ => &[(0, "nas"), (1, "share"), (2, "album"), (1, "アクセス不可")],
    };
    let mut out: Vec<TreeNode> = Vec::new();
    let mut stack: Vec<(usize, u32, PathBuf)> = Vec::new();
    for (i, (depth, name)) in spec.iter().enumerate() {
        stack.truncate(*depth);
        let (parent, path) = match stack.last() {
            Some((_, id, path)) => (*id, path.join(name)),
            None => (ROOT, Path::new("/mnt").join(name)),
        };
        let id = i as u32 + 1;
        out.push(TreeNode { id, parent, name: (*name).to_string(), real: path.clone() });
        stack.push((*depth, id, path));
    }
    out
}

enum TreeEvent {
    Toggle(u32),
    Select(u32),
    DoubleClick(u32),
    Register(u32),
    Delete(u32),
    /// デバッグビルド限定: 現在の実フォルダを選択ノードの下に登録する（3bで撤去）
    #[cfg(debug_assertions)]
    DebugSeed(u32),
}

fn children_of(nodes: &[TreeNode], parent: u32) -> Vec<&TreeNode> {
    nodes.iter().filter(|n| n.parent == parent).collect()
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
    out: &mut Vec<TreeEvent>,
) {
    match root_label {
        Some(label) => draw_tree_row(ui, nodes, ROOT, label, None, 0, expanded, selected, menu_on, ring, out),
        None => {
            for n in children_of(nodes, ROOT) {
                draw_tree_row(ui, nodes, n.id, &n.name, Some(&n.real), 0, expanded, selected, menu_on, ring, out);
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
        let mut r = ui.selectable_label(selected == Some(id), label);
        if let Some(p) = real {
            r = r.on_hover_text(p.display().to_string());
        }
        if ring == Some(id) {
            super::panels::draw_cursor_ring(ui, r.rect);
        }
        if r.clicked() || r.secondary_clicked() {
            out.push(TreeEvent::Select(id));
        }
        if r.double_clicked() {
            out.push(TreeEvent::DoubleClick(id));
        }
        if menu_on {
            r.context_menu(|ui| {
                if ui.button(i18n::t().virtual_menu_register()).clicked() {
                    out.push(TreeEvent::Register(id));
                    ui.close();
                }
                // ルートは削除対象外（グレーアウト）
                if ui.add_enabled(id != ROOT, egui::Button::new(i18n::t().virtual_menu_delete())).clicked() {
                    out.push(TreeEvent::Delete(id));
                    ui.close();
                }
                #[cfg(debug_assertions)]
                {
                    ui.separator();
                    if ui.button("[DEBUG] 現在の実フォルダを登録").clicked() {
                        out.push(TreeEvent::DebugSeed(id));
                        ui.close();
                    }
                }
            });
        }
    });
    if is_expanded {
        for c in children_of(nodes, id) {
            draw_tree_row(ui, nodes, c.id, &c.name, Some(&c.real), depth + 1, expanded, selected, menu_on, ring, out);
        }
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
                Some(if self.virtual_state.card_from_real { BLUE } else { GREEN })
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
        let from_real = self.virtual_state.card_from_real;
        if from_real != real_pane {
            return;
        }
        let color = if real_pane { BLUE } else { GREEN };
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Inside);
    }

    pub(super) fn virtual_real_pane_open(&self) -> bool {
        self.virtual_state.real_pane_open
    }

    /// 仮想タブ中に実ツリー側でナビゲートしたことをカード欄の枠色に反映する。
    pub(super) fn mark_card_from_real(&mut self) {
        self.virtual_state.card_from_real = true;
    }

    /// 実ツリー右クリック「仮想フォルダに追加する」。追加先の仮想フォルダを選ぶピッカーを開く。
    pub(super) fn open_virtual_dest_picker(&mut self, src: PathBuf) {
        let expanded = self.virtual_state.expanded.clone();
        self.virtual_state.confirm = None;
        self.virtual_state.picker = Some(Picker::new(PickerKind::VirtualDest { src }, expanded));
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
            .map(|n| TreeNode { id: n.id, parent: n.parent_id, name: n.name, real: n.real_path })
            .collect();
        let ids: HashSet<u32> = self.virtual_state.nodes.iter().map(|n| n.id).collect();
        self.virtual_state.expanded.retain(|id| *id == ROOT || ids.contains(id));
        if self.virtual_state.selected.is_some_and(|id| !ids.contains(&id)) {
            self.virtual_state.selected = None;
        }
    }

    /// デバッグビルド限定の投入手段。3aの目視確認用で、3bの本登録が入ったら撤去する。
    /// `current_dir` 配下の実サブフォルダ構造を、選択ノードの下にスナップショット登録する。
    #[cfg(debug_assertions)]
    fn debug_seed_register(&mut self, dest: u32) {
        let Some(db) = self.spread_db.clone() else {
            self.set_toast("[DEBUG] DBが開けていません");
            return;
        };
        let scan = crate::virtual_folder_scan::scan_subtree(
            &self.current_dir,
            crate::virtual_folder_scan::SCAN_HARD_CAP,
        );
        let msg = match crate::virtual_folders::add_subtree(&db, dest, &scan.spec) {
            Ok(nodes) => format!("[DEBUG] {} 件を登録しました（capped={}）", nodes.len(), scan.capped),
            Err(e) => format!("[DEBUG] 登録に失敗しました: {e:?}"),
        };
        self.refresh_virtual_nodes();
        self.virtual_state.expanded.insert(dest);
        self.set_toast(msg);
    }

    pub(super) fn draw_virtual_folder_pane(&mut self, ui: &mut egui::Ui) {
        let mut events = Vec::new();
        egui::ScrollArea::both()
            .id_salt("virtual_tree_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let m = &self.virtual_state;
                // フォーカス中は選択ノード（未選択なら `/`）にカーソルリングを出す
                let ring = (self.focused_pane == FocusPane::VirtualTab).then(|| m.selected.unwrap_or(ROOT));
                draw_tree(ui, &m.nodes, Some("/"), &m.expanded, m.selected, true, ring, &mut events);
            });
        if !events.is_empty() {
            self.focused_pane = FocusPane::VirtualTab;
        }
        for ev in events {
            match ev {
                TreeEvent::Toggle(id) => toggle(&mut self.virtual_state.expanded, id),
                TreeEvent::Select(id) => {
                    self.virtual_state.selected = Some(id);
                    self.virtual_state.card_from_real = false;
                }
                TreeEvent::DoubleClick(_) => {}
                TreeEvent::Register(id) => {
                    self.virtual_state.confirm = None;
                    self.virtual_state.picker = Some(Picker::new(
                        PickerKind::RealSource { dest: id },
                        HashSet::from([1]),
                    ));
                }
                TreeEvent::Delete(id) => {
                    if id != ROOT {
                        self.virtual_state.delete = Some(id);
                    }
                }
                #[cfg(debug_assertions)]
                TreeEvent::DebugSeed(id) => self.debug_seed_register(id),
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
        self.draw_virtual_picker(ctx);
        self.draw_virtual_confirm(ctx);
        self.draw_virtual_delete(ctx);
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.app_toast = Some((msg.into(), std::time::Instant::now()));
    }

    /// 実フォルダ／追加先仮想フォルダの選択ダイアログ。OK/キャンセルは無く、
    /// ダブルクリックで確認ダイアログへ進む。右上のXで閉じる。
    fn draw_virtual_picker(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.virtual_state.picker.take() else { return };
        let confirm_open = self.virtual_state.confirm.is_some();
        let is_real = matches!(p.kind, PickerKind::RealSource { .. });
        let title = if is_real {
            i18n::t().virtual_picker_title_real()
        } else {
            i18n::t().virtual_picker_title_dest()
        };
        let mut open = true;
        let mut events = Vec::new();
        egui::Window::new(title)
            .id(egui::Id::new("virtual_picker_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([360.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.add_enabled_ui(!confirm_open, |ui| {
                    if is_real {
                        let mut picked = None;
                        egui::ComboBox::from_id_salt("virtual_picker_drive")
                            .selected_text(DUMMY_DRIVES[p.drive])
                            .show_ui(ui, |ui| {
                                for (i, label) in DUMMY_DRIVES.iter().enumerate() {
                                    if ui.selectable_label(p.drive == i, *label).clicked() {
                                        picked = Some(i);
                                    }
                                }
                            });
                        if let Some(i) = picked {
                            p.drive = i;
                            p.real_nodes = dummy_real_tree(i);
                            p.expanded = HashSet::from([1]);
                            p.selected = None;
                        }
                        ui.separator();
                    }
                    egui::ScrollArea::both()
                        .id_salt("virtual_picker_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if is_real {
                                draw_tree(ui, &p.real_nodes, None, &p.expanded, p.selected, false, None, &mut events);
                            } else {
                                draw_tree(ui, &self.virtual_state.nodes, Some("/"), &p.expanded, p.selected, false, None, &mut events);
                            }
                        });
                });
            });
        if !open {
            self.virtual_state.confirm = None;
            return;
        }
        for ev in events {
            match ev {
                TreeEvent::Toggle(id) => toggle(&mut p.expanded, id),
                TreeEvent::Select(id) => p.selected = Some(id),
                TreeEvent::DoubleClick(id) => {
                    self.virtual_state.confirm = match &p.kind {
                        PickerKind::RealSource { dest } => p
                            .real_nodes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| Confirm { src: n.real.clone(), dest: *dest }),
                        PickerKind::VirtualDest { src } => Some(Confirm { src: src.clone(), dest: id }),
                    };
                }
                _ => {}
            }
        }
        self.virtual_state.picker = Some(p);
    }

    /// 登録確認の固定ダイアログ。3bで OK → 評価 → 登録 or 異常トースト に接続する。
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
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ok = ui.button(i18n::t().virtual_ok()).clicked();
                    cancel = ui.button(i18n::t().favorite_dialog_cancel()).clicked();
                });
            });
        if cancel {
            self.virtual_state.confirm = None;
        } else if ok {
            // TODO(3b): 評価 → add_subtree に接続する。それまでは実行せずに終了する。
            self.virtual_state.confirm = None;
            self.virtual_state.picker = None;
            self.set_toast("（登録処理は未接続です。次フェーズで接続予定）");
        }
    }

    /// 仮想フォルダ削除の固定ダイアログ。子孫は常に連動削除（実フォルダには触れない）。
    fn draw_virtual_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.virtual_state.delete else { return };
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
            // TODO(3c): remove_node に接続する。それまでは実行せずに終了する。
            self.virtual_state.delete = None;
            self.set_toast("（削除処理は未接続です。次フェーズで接続予定）");
        }
    }
}
