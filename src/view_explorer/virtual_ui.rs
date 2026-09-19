//! 仮想フォルダタブのUIモック（フェーズ3M）。
//!
//! ダミーデータだけで動く見た目確認用の実装で、DB（`virtual_folders`）・実FSの走査・
//! 登録/削除の実務APIには一切接続しない。3aで実データに差し替える前提のため、
//! 文字列は暫定でハードコード（i18nは文言確定後の3aで対応）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::*;

const GREEN: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const BLUE: egui::Color32 = egui::Color32::from_rgb(70, 130, 230);

/// 仮想ルート `/` を表す予約id（実データ層の `ROOT_ID` と同じ規約）。
const ROOT: u32 = 0;

const DUMMY_DRIVES: [&str; 2] = ["/mnt/data", "/mnt/nas"];

#[derive(Clone)]
struct MockNode {
    id: u32,
    parent: u32,
    name: String,
    real: PathBuf,
}

impl MockNode {
    fn new(id: u32, parent: u32, name: &str, real: &str) -> Self {
        Self { id, parent, name: name.to_string(), real: PathBuf::from(real) }
    }
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
    real_nodes: Vec<MockNode>,
    expanded: HashSet<u32>,
    selected: Option<u32>,
}

#[derive(Clone)]
struct Confirm {
    src: PathBuf,
    dest: u32,
}

pub(super) struct VirtualMock {
    nodes: Vec<MockNode>,
    next_id: u32,
    expanded: HashSet<u32>,
    selected: Option<u32>,
    real_pane_open: bool,
    /// アイテムカード欄の表示元。true=実ツリー選択（青枠）、false=仮想フォルダ選択（緑枠）。
    card_from_real: bool,
    picker: Option<Picker>,
    confirm: Option<Confirm>,
    delete: Option<u32>,
}

impl VirtualMock {
    pub(super) fn new() -> Self {
        let nodes = vec![
            MockNode::new(1, ROOT, "漫画", "/mnt/data/Manga"),
            MockNode::new(2, 1, "少年", "/mnt/data/Manga/shonen"),
            MockNode::new(3, 1, "青年", "/mnt/data/Manga/seinen"),
            MockNode::new(4, ROOT, "写真集", "/mnt/data/Photo"),
            MockNode::new(5, 4, "2025", "/mnt/data/Photo/2025"),
            MockNode::new(6, ROOT, "NAS", "/mnt/nas/share"),
            MockNode::new(7, ROOT, "削除エラー再現", "/mnt/broken"),
        ];
        Self {
            nodes,
            next_id: 8,
            expanded: HashSet::from([1]),
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

    /// 「アクセス不可」を含むパスは異常系の見た目確認用に登録失敗させる。
    fn register(&mut self, src: &Path, dest: u32) -> Result<(), &'static str> {
        if src.to_string_lossy().contains("アクセス不可") {
            return Err("フォルダにアクセスできません");
        }
        if self.nodes.iter().any(|n| n.parent == dest && n.real == src) {
            return Err("同じ実フォルダが既に登録されています");
        }
        let name = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| src.display().to_string());
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(MockNode { id, parent: dest, name, real: src.to_path_buf() });
        self.expanded.insert(dest);
        Ok(())
    }

    /// 「削除エラー再現」ノードは異常系の見た目確認用に削除失敗させる。
    fn remove(&mut self, id: u32) -> Result<usize, &'static str> {
        if self.nodes.iter().any(|n| n.id == id && n.name == "削除エラー再現") {
            return Err("削除できませんでした");
        }
        let ids = self.subtree_ids(id);
        self.nodes.retain(|n| !ids.contains(&n.id));
        self.expanded.retain(|e| !ids.contains(e));
        if self.selected.is_some_and(|s| ids.contains(&s)) {
            self.selected = None;
        }
        Ok(ids.len())
    }
}

impl Picker {
    fn new(kind: PickerKind, expanded: HashSet<u32>) -> Self {
        Self { kind, drive: 0, real_nodes: dummy_real_tree(0), expanded, selected: None }
    }
}

/// ドライブごとのダミー実ツリー。`(深さ, 名前)` の先行順リストから組む。
fn dummy_real_tree(drive: usize) -> Vec<MockNode> {
    let spec: &[(usize, &str)] = match drive {
        0 => &[
            (0, "data"), (1, "Manga"), (2, "shonen"), (2, "seinen"),
            (1, "Photo"), (2, "2024"), (2, "2025"), (1, "Work"),
        ],
        _ => &[(0, "nas"), (1, "share"), (2, "album"), (1, "アクセス不可")],
    };
    let mut out: Vec<MockNode> = Vec::new();
    let mut stack: Vec<(usize, u32, PathBuf)> = Vec::new();
    for (i, (depth, name)) in spec.iter().enumerate() {
        stack.truncate(*depth);
        let (parent, path) = match stack.last() {
            Some((_, id, path)) => (*id, path.join(name)),
            None => (ROOT, Path::new("/mnt").join(name)),
        };
        let id = i as u32 + 1;
        out.push(MockNode { id, parent, name: (*name).to_string(), real: path.clone() });
        stack.push((*depth, id, path));
    }
    out
}

enum MockEvent {
    Toggle(u32),
    Select(u32),
    DoubleClick(u32),
    Register(u32),
    Delete(u32),
}

fn children_of(nodes: &[MockNode], parent: u32) -> Vec<&MockNode> {
    nodes.iter().filter(|n| n.parent == parent).collect()
}

/// ダミーツリー描画。`root_label` が Some なら仮想ルート行（id=ROOT）を先頭に描く。
/// `menu_on` は仮想ツリー本体用の右クリックメニュー（実フォルダ登録／仮想フォルダ削除）。
fn draw_mock_tree(
    ui: &mut egui::Ui,
    nodes: &[MockNode],
    root_label: Option<&str>,
    expanded: &HashSet<u32>,
    selected: Option<u32>,
    menu_on: bool,
    ring: Option<u32>,
    out: &mut Vec<MockEvent>,
) {
    match root_label {
        Some(label) => draw_mock_row(ui, nodes, ROOT, label, None, 0, expanded, selected, menu_on, ring, out),
        None => {
            for n in children_of(nodes, ROOT) {
                draw_mock_row(ui, nodes, n.id, &n.name, Some(&n.real), 0, expanded, selected, menu_on, ring, out);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_mock_row(
    ui: &mut egui::Ui,
    nodes: &[MockNode],
    id: u32,
    label: &str,
    real: Option<&Path>,
    depth: usize,
    expanded: &HashSet<u32>,
    selected: Option<u32>,
    menu_on: bool,
    ring: Option<u32>,
    out: &mut Vec<MockEvent>,
) {
    let has_children = nodes.iter().any(|n| n.parent == id);
    let is_expanded = expanded.contains(&id);
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 12.0);
        if has_children {
            let arrow = if is_expanded { "▼" } else { "▶" };
            if ui.add(egui::Label::new(arrow).sense(egui::Sense::click())).clicked() {
                out.push(MockEvent::Toggle(id));
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
            out.push(MockEvent::Select(id));
        }
        if r.double_clicked() {
            out.push(MockEvent::DoubleClick(id));
        }
        if menu_on {
            r.context_menu(|ui| {
                if ui.button("実フォルダ登録").clicked() {
                    out.push(MockEvent::Register(id));
                    ui.close();
                }
                // ルートは削除対象外（グレーアウト）
                if ui.add_enabled(id != ROOT, egui::Button::new("仮想フォルダ削除")).clicked() {
                    out.push(MockEvent::Delete(id));
                    ui.close();
                }
            });
        }
    });
    if is_expanded {
        for c in children_of(nodes, id) {
            draw_mock_row(ui, nodes, c.id, &c.name, Some(&c.real), depth + 1, expanded, selected, menu_on, ring, out);
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
                Some(if self.virtual_mock.card_from_real { BLUE } else { GREEN })
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
        let from_real = self.virtual_mock.card_from_real;
        if from_real != real_pane {
            return;
        }
        let color = if real_pane { BLUE } else { GREEN };
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Inside);
    }

    pub(super) fn virtual_mock_real_pane_open(&self) -> bool {
        self.virtual_mock.real_pane_open
    }

    /// 仮想タブ中に実ツリー側でナビゲートしたことをカード欄の枠色に反映する。
    pub(super) fn mark_card_from_real(&mut self) {
        self.virtual_mock.card_from_real = true;
    }

    /// 実ツリー右クリック「仮想フォルダに追加する」。追加先の仮想フォルダを選ぶピッカーを開く。
    pub(super) fn open_virtual_dest_picker(&mut self, src: PathBuf) {
        let expanded = self.virtual_mock.expanded.clone();
        self.virtual_mock.confirm = None;
        self.virtual_mock.picker = Some(Picker::new(PickerKind::VirtualDest { src }, expanded));
    }

    pub(super) fn draw_virtual_folder_pane(&mut self, ui: &mut egui::Ui) {
        let mut events = Vec::new();
        egui::ScrollArea::both()
            .id_salt("virtual_tree_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let m = &self.virtual_mock;
                // フォーカス中は選択ノード（未選択なら `/`）にカーソルリングを出す
                let ring = (self.focused_pane == FocusPane::VirtualTab).then(|| m.selected.unwrap_or(ROOT));
                draw_mock_tree(ui, &m.nodes, Some("/"), &m.expanded, m.selected, true, ring, &mut events);
            });
        if !events.is_empty() {
            self.focused_pane = FocusPane::VirtualTab;
        }
        for ev in events {
            match ev {
                MockEvent::Toggle(id) => toggle(&mut self.virtual_mock.expanded, id),
                MockEvent::Select(id) => {
                    self.virtual_mock.selected = Some(id);
                    self.virtual_mock.card_from_real = false;
                }
                MockEvent::DoubleClick(_) => {}
                MockEvent::Register(id) => {
                    self.virtual_mock.confirm = None;
                    self.virtual_mock.picker = Some(Picker::new(
                        PickerKind::RealSource { dest: id },
                        HashSet::from([1]),
                    ));
                }
                MockEvent::Delete(id) => {
                    if id != ROOT {
                        self.virtual_mock.delete = Some(id);
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
        let open = self.virtual_mock.real_pane_open;
        let resp = resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if open { "実ツリーを閉じる" } else { "実ツリーを開く" });
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
            self.virtual_mock.real_pane_open = !open;
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
        let Some(mut p) = self.virtual_mock.picker.take() else { return };
        let confirm_open = self.virtual_mock.confirm.is_some();
        let is_real = matches!(p.kind, PickerKind::RealSource { .. });
        let title = if is_real {
            "登録したいフォルダをダブルクリックで確定"
        } else {
            "追加先の仮想フォルダをダブルクリックで確定"
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
                                draw_mock_tree(ui, &p.real_nodes, None, &p.expanded, p.selected, false, None, &mut events);
                            } else {
                                draw_mock_tree(ui, &self.virtual_mock.nodes, Some("/"), &p.expanded, p.selected, false, None, &mut events);
                            }
                        });
                });
            });
        if !open {
            self.virtual_mock.confirm = None;
            return;
        }
        for ev in events {
            match ev {
                MockEvent::Toggle(id) => toggle(&mut p.expanded, id),
                MockEvent::Select(id) => p.selected = Some(id),
                MockEvent::DoubleClick(id) => {
                    self.virtual_mock.confirm = match &p.kind {
                        PickerKind::RealSource { dest } => p
                            .real_nodes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| Confirm { src: n.real.clone(), dest: *dest }),
                        PickerKind::VirtualDest { src } => Some(Confirm { src: src.clone(), dest: id }),
                    };
                }
                MockEvent::Register(_) | MockEvent::Delete(_) => {}
            }
        }
        self.virtual_mock.picker = Some(p);
    }

    /// 登録確認の固定ダイアログ。OKで評価（モックでは擬似）→ 登録 or 異常トースト。
    fn draw_virtual_confirm(&mut self, ctx: &egui::Context) {
        let Some(c) = self.virtual_mock.confirm.clone() else { return };
        let dest_path = self.virtual_mock.virtual_path(c.dest);
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("登録の確認")
            .id(egui::Id::new("virtual_confirm_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("次のフォルダを仮想フォルダに登録します");
                ui.add_space(6.0);
                ui.label(format!("登録パス: {}", c.src.display()));
                ui.label(format!("登録先: {dest_path}"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("キャンセル").clicked();
                });
            });
        if cancel {
            self.virtual_mock.confirm = None;
        } else if ok {
            let msg = match self.virtual_mock.register(&c.src, c.dest) {
                Ok(()) => "仮想フォルダに正常に登録されました".to_string(),
                Err(reason) => format!("仮想フォルダへの登録に失敗しました（{reason}）"),
            };
            // 正常・異常どちらでも登録操作は終了（異常時は登録キャンセル扱い）
            self.virtual_mock.confirm = None;
            self.virtual_mock.picker = None;
            self.set_toast(msg);
        }
    }

    /// 仮想フォルダ削除の固定ダイアログ。子孫は常に連動削除（実フォルダには触れない）。
    fn draw_virtual_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.virtual_mock.delete else { return };
        let path = self.virtual_mock.virtual_path(id);
        let descendants = self.virtual_mock.subtree_ids(id).len().saturating_sub(1);
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("仮想フォルダの削除")
            .id(egui::Id::new("virtual_delete_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("削除する仮想パス: {path}"));
                if descendants > 0 {
                    ui.label(format!("配下 {descendants} 件のフォルダも削除されます"));
                }
                ui.label("実フォルダには影響しません");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("キャンセル").clicked();
                });
            });
        if cancel {
            self.virtual_mock.delete = None;
        } else if ok {
            let msg = match self.virtual_mock.remove(id) {
                Ok(_) => "仮想フォルダを削除しました".to_string(),
                Err(reason) => format!("仮想フォルダの削除に失敗しました（{reason}）"),
            };
            self.virtual_mock.delete = None;
            self.set_toast(msg);
        }
    }
}
