//! リストの「仮想フォルダ」列の移動アイコン。そのファイルの場所（親の実フォルダ）を登録している仮想ノードを逆引きして、
//! 仮想フォルダタブでそのノードを開き、ファイルを選択して表示する。
//!
//! - 登録が1つなら、そこへ移る。複数なら、仮想ツリーでのパスを並べた選択メニューを出す。無ければアイコンは灰色で押せない。
//! - 逆引き（実フォルダ → ノードのID）は DB から読む。タブを移ったとき・仮想ノードを読み直したときに引き直す
//!   （仮想タブを開いていなくても、登録の変更を反映できる）。
//! - 移動の後の「ファイルを選んで見える位置へスクロール」は、実フォルダへの移動と同じ仕組み（`similar_return`）を使う。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::i18n;
use crate::virtual_folders::{self, VirtualNode, ROOT_ID};

use super::{FocusPane, FolderPaneTab, NekoviewApp};

/// 移動先の仮想フォルダが複数あるときの選択メニュー。
pub(super) struct GotoVirtualMenu {
    pub(super) file: PathBuf,
    /// （ノードのID, 仮想ツリーでのパス）。登録順。
    pub(super) entries: Vec<(u32, String)>,
    anchor: egui::Pos2,
}

impl GotoVirtualMenu {
    /// 選択肢の数（自動テスト用の観測にも使う）。
    #[cfg_attr(not(feature = "test-control"), allow(dead_code))]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 実フォルダ → それを登録している仮想ノードのID（IDの小さい順＝登録順）。
pub(super) fn build_lookup(nodes: &[VirtualNode]) -> HashMap<PathBuf, Vec<u32>> {
    let mut map: HashMap<PathBuf, Vec<u32>> = HashMap::new();
    for n in nodes {
        map.entry(n.real_path.clone()).or_default().push(n.id);
    }
    for ids in map.values_mut() {
        ids.sort_unstable();
    }
    map
}

/// 仮想ツリーでのパス（`/親/子`）。祖先の名前を根からたどる。途中が欠けていれば、たどれたところまで。
pub(super) fn node_path_label(nodes: &[VirtualNode], id: u32) -> String {
    let mut names: Vec<&str> = Vec::new();
    let mut cur = id;
    // 循環があっても止まるよう、ノード数で打ち切る。
    for _ in 0..=nodes.len() {
        if cur == ROOT_ID {
            break;
        }
        let Some(n) = nodes.iter().find(|n| n.id == cur) else { break };
        names.push(n.name.as_str());
        cur = n.parent_id;
    }
    names.reverse();
    format!("/{}", names.join("/"))
}

impl NekoviewApp {
    /// 逆引きが古ければ引き直す（描画の前に呼ぶ）。DB が無ければ空。
    pub(super) fn ensure_virtual_lookup(&mut self) {
        if self.virtual_lookup_loaded {
            return;
        }
        let nodes = self.spread_db.as_ref().map(virtual_folders::list_nodes).unwrap_or_default();
        self.virtual_lookup = build_lookup(&nodes);
        self.virtual_lookup_loaded = true;
    }

    /// この実フォルダを登録している仮想ノードのID。`ensure_virtual_lookup` の後に使う。
    pub(super) fn virtual_nodes_for(&self, dir: &Path) -> &[u32] {
        self.virtual_lookup.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    /// 「仮想フォルダ」アイコンが押された: 登録が1つならそこへ移り、複数なら選択メニューを出す。無ければ何もしない。
    pub(super) fn start_goto_virtual(&mut self, file: PathBuf, anchor: egui::Pos2) {
        self.ensure_virtual_lookup();
        let Some(dir) = file.parent() else { return };
        let ids = self.virtual_nodes_for(dir).to_vec();
        match ids.as_slice() {
            [] => {}
            [id] => self.open_file_location_in_virtual_tab(*id, file),
            _ => {
                let nodes = self.spread_db.as_ref().map(virtual_folders::list_nodes).unwrap_or_default();
                let entries = ids.iter().map(|&id| (id, node_path_label(&nodes, id))).collect();
                self.goto_virtual_menu = Some(GotoVirtualMenu { file, entries, anchor });
            }
        }
    }

    /// 仮想ノード `id` を仮想フォルダタブで開き、ファイル `file` を選択して見える位置へスクロールする。
    /// 届かない場所はトーストで知らせて、何も変えない。
    pub(in crate::view_explorer) fn open_file_location_in_virtual_tab(&mut self, id: u32, file: PathBuf) {
        let Some(db) = self.spread_db.clone() else { return };
        let Some(node) = virtual_folders::get_node(&db, id) else { return };
        if !self.path_reachable(&node.real_path) {
            self.set_toast(i18n::t().virtual_folder_unreachable());
            return;
        }
        self.goto_virtual_menu = None;
        // 類似ファイル一覧などを開いていれば閉じる（同じタブ・同じノードのままだと、再スキャンが起きないため）
        self.exit_similar_view();
        self.switch_folder_tab(FolderPaneTab::VirtualFolders);
        self.focused_pane = FocusPane::VirtualTab;
        self.select_virtual_node(id);
        self.similar_return = Some(file);
        self.persist_state();
    }

    /// 選択メニューを描く。項目を押すとその仮想フォルダへ移る。外側の押下かEscで閉じる。
    pub(super) fn draw_goto_virtual_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = &self.goto_virtual_menu else { return };
        let (file, entries, anchor) = (menu.file.clone(), menu.entries.clone(), menu.anchor);
        let mut chosen: Option<u32> = None;
        let area = egui::Area::new(egui::Id::new("goto_virtual_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(egui::RichText::new(i18n::t().goto_virtual_menu_title()).strong());
                    for (id, label) in &entries {
                        if ui.button(label).clicked() {
                            chosen = Some(*id);
                        }
                    }
                });
            });
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p));
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if let Some(id) = chosen {
            self.open_file_location_in_virtual_tab(id, file);
        } else if pressed_outside || escape {
            self.goto_virtual_menu = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, parent_id: u32, name: &str, real: &str) -> VirtualNode {
        VirtualNode { id, parent_id, real_path: PathBuf::from(real), name: name.to_string(), order: id }
    }

    #[test]
    fn lookup_groups_nodes_by_real_folder_in_registration_order() {
        let nodes = vec![
            node(5, ROOT_ID, "late", "/data/a"),
            node(2, ROOT_ID, "x", "/data/b"),
            node(1, ROOT_ID, "early", "/data/a"),
        ];
        let map = build_lookup(&nodes);
        assert_eq!(map.get(Path::new("/data/a")), Some(&vec![1, 5]), "IDの小さい順");
        assert_eq!(map.get(Path::new("/data/b")), Some(&vec![2]));
        assert!(map.get(Path::new("/data/none")).is_none());
        assert!(build_lookup(&[]).is_empty());
    }

    #[test]
    fn path_label_walks_up_to_the_root() {
        let nodes = vec![
            node(1, ROOT_ID, "Comics", "/c"),
            node(2, 1, "Author", "/c/a"),
            node(3, 2, "Series", "/c/a/s"),
        ];
        assert_eq!(node_path_label(&nodes, 3), "/Comics/Author/Series");
        assert_eq!(node_path_label(&nodes, 1), "/Comics");
        assert_eq!(node_path_label(&nodes, 99), "/", "見つからなければ根だけ");
    }

    #[test]
    fn path_label_stops_on_a_broken_chain_or_a_cycle() {
        let broken = vec![node(2, 77, "orphan", "/o")];
        assert_eq!(node_path_label(&broken, 2), "/orphan", "親が欠けていても、たどれたところまで");
        let cycle = vec![node(1, 2, "a", "/a"), node(2, 1, "b", "/b")];
        let label = node_path_label(&cycle, 1);
        assert!(label.starts_with('/') && label.len() < 40, "循環しても止まる: {label}");
    }
}
