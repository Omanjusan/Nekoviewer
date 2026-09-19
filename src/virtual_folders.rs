// フェーズ3（仮想ビュー）以降で接続するまでの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

/// 仮想ルート `/` のノードid。DBには保存しない（親idとしてのみ現れる）。
pub const ROOT_ID: u32 = 0;
/// 仮想ツリー全体のノード数上限。
pub const MAX_NODES: usize = 5000;
/// ノード表示名の文字数上限。
pub const MAX_NAME_CHARS: usize = 200;

/// 仮想フォルダのノード表（第1世代）。
///
/// キー = node_id（u32、1始まり。0 は仮想ルートとして予約）
/// 値 = (parent_id, real_path, name, order)
/// 同じ real_path を複数ノードが持ってよい（実パスは一意キーにしない）。
/// 値形式を将来変更する場合はこの定義を変更せず、`virtual_folder_nodes_v2` のような
/// 新しいテーブルを追加して移行すること。
pub const VIRTUAL_FOLDER_TABLE_V1: TableDefinition<u32, (u32, &str, &str, u32)> =
    TableDefinition::new("virtual_folder_nodes_v1");

type NodeValue = (u32, &'static str, &'static str, u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualNode {
    pub id: u32,
    pub parent_id: u32,
    pub real_path: PathBuf,
    pub name: String,
    pub order: u32,
}

/// 登録時に取り込む実フォルダのスナップショット（フェーズ2のスキャン結果もこの形で渡す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtreeSpec {
    pub real_path: PathBuf,
    pub name: String,
    pub children: Vec<SubtreeSpec>,
}

impl SubtreeSpec {
    pub fn leaf(real_path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            real_path: real_path.into(),
            name: name.into(),
            children: Vec::new(),
        }
    }

    /// 自分自身を含むノード総数。
    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    fn validate_names(&self) -> Result<(), VirtualFolderError> {
        validate_name(&self.name)?;
        self.children.iter().try_for_each(Self::validate_names)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VirtualFolderError {
    NameEmpty,
    NameTooLong,
    LimitReached,
    NotFound,
    ParentNotFound,
    CycleDetected,
    Db,
}

/// 既存の spread_state 用 DB にノード表を追加する。テーブルが無ければ自動作成される。
pub fn init_virtual_folder_tables(db: &Arc<Mutex<Database>>) -> Option<()> {
    let db = db.lock().ok()?;
    let tx = db.begin_write().ok()?;
    tx.open_table(VIRTUAL_FOLDER_TABLE_V1).ok()?;
    tx.commit().ok()?;
    Some(())
}

/// 実パスの正規化。存在しなければ親だけ正規化して繋ぎ、それも駄目ならそのまま返す
/// （移動・削除後の旧パスをプレフィックス指定するケースを想定）。
fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(p) = parent.canonicalize() {
            return p.join(name);
        }
    }
    path.to_path_buf()
}

fn validate_name(name: &str) -> Result<(), VirtualFolderError> {
    if name.is_empty() {
        return Err(VirtualFolderError::NameEmpty);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(VirtualFolderError::NameTooLong);
    }
    Ok(())
}

fn load_nodes(table: &impl ReadableTable<u32, NodeValue>) -> Result<Vec<VirtualNode>, VirtualFolderError> {
    let iter = table.iter().map_err(|_| VirtualFolderError::Db)?;
    let mut out = Vec::new();
    for entry in iter {
        let Ok((k, v)) = entry else { continue };
        let (parent_id, real_path, name, order) = v.value();
        out.push(VirtualNode {
            id: k.value(),
            parent_id,
            real_path: PathBuf::from(real_path),
            name: name.to_string(),
            order,
        });
    }
    out.sort_by_key(|n| (n.parent_id, n.order, n.id));
    Ok(out)
}

fn write_node(
    table: &mut redb::Table<u32, NodeValue>,
    node: &VirtualNode,
) -> Result<(), VirtualFolderError> {
    table
        .insert(
            node.id,
            (
                node.parent_id,
                node.real_path.to_string_lossy().as_ref(),
                node.name.as_str(),
                node.order,
            ),
        )
        .map_err(|_| VirtualFolderError::Db)?;
    Ok(())
}

/// 全ノードを (parent_id, order, id) 昇順で返す。
pub fn list_nodes(db: &Arc<Mutex<Database>>) -> Vec<VirtualNode> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(VIRTUAL_FOLDER_TABLE_V1) else {
        return Vec::new();
    };
    load_nodes(&table).unwrap_or_default()
}

/// 指定親の直下の子を order 昇順で返す。ROOT_ID を渡すと `/` 直下。
pub fn list_children(db: &Arc<Mutex<Database>>, parent_id: u32) -> Vec<VirtualNode> {
    list_nodes(db)
        .into_iter()
        .filter(|n| n.parent_id == parent_id)
        .collect()
}

pub fn get_node(db: &Arc<Mutex<Database>>, id: u32) -> Option<VirtualNode> {
    list_nodes(db).into_iter().find(|n| n.id == id)
}

/// 実フォルダを1つ、指定親の末尾に登録する。
pub fn add_node(
    db: &Arc<Mutex<Database>>,
    parent_id: u32,
    real_path: &Path,
    name: &str,
) -> Result<VirtualNode, VirtualFolderError> {
    let spec = SubtreeSpec::leaf(real_path, name);
    add_subtree(db, parent_id, &spec).map(|mut nodes| nodes.remove(0))
}

/// スナップショット（部分木）を1トランザクションで登録する。
/// 戻り値は先行順（先頭 = 部分木の根）。1件でも失敗したら何も登録しない。
pub fn add_subtree(
    db: &Arc<Mutex<Database>>,
    parent_id: u32,
    spec: &SubtreeSpec,
) -> Result<Vec<VirtualNode>, VirtualFolderError> {
    spec.validate_names()?;
    let Ok(db) = db.lock() else {
        return Err(VirtualFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| VirtualFolderError::Db)?;
    let inserted;
    {
        let mut table = tx
            .open_table(VIRTUAL_FOLDER_TABLE_V1)
            .map_err(|_| VirtualFolderError::Db)?;
        let existing = load_nodes(&table)?;
        if existing.len() + spec.node_count() > MAX_NODES {
            return Err(VirtualFolderError::LimitReached);
        }
        if parent_id != ROOT_ID && !existing.iter().any(|n| n.id == parent_id) {
            return Err(VirtualFolderError::ParentNotFound);
        }
        let mut next_id = existing
            .iter()
            .map(|n| n.id)
            .max()
            .map_or(Some(1), |m| m.checked_add(1))
            .ok_or(VirtualFolderError::LimitReached)?;
        let root_order = existing
            .iter()
            .filter(|n| n.parent_id == parent_id)
            .map(|n| n.order + 1)
            .max()
            .unwrap_or(0);
        let mut out = Vec::with_capacity(spec.node_count());
        insert_spec(&mut table, spec, parent_id, root_order, &mut next_id, &mut out)?;
        inserted = out;
    }
    tx.commit().map_err(|_| VirtualFolderError::Db)?;
    Ok(inserted)
}

fn insert_spec(
    table: &mut redb::Table<u32, NodeValue>,
    spec: &SubtreeSpec,
    parent_id: u32,
    order: u32,
    next_id: &mut u32,
    out: &mut Vec<VirtualNode>,
) -> Result<(), VirtualFolderError> {
    let id = *next_id;
    *next_id = next_id.checked_add(1).ok_or(VirtualFolderError::LimitReached)?;
    let node = VirtualNode {
        id,
        parent_id,
        real_path: normalize_path(&spec.real_path),
        name: spec.name.clone(),
        order,
    };
    write_node(table, &node)?;
    out.push(node);
    for (i, child) in spec.children.iter().enumerate() {
        insert_spec(table, child, id, i as u32, next_id, out)?;
    }
    Ok(())
}

/// id と全子孫を返す（自分自身を先頭に含む）。
fn collect_subtree_ids(nodes: &[VirtualNode], id: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for n in nodes {
        children.entry(n.parent_id).or_default().push(n.id);
    }
    let mut out = vec![id];
    let mut cursor = 0;
    while cursor < out.len() {
        if let Some(kids) = children.get(&out[cursor]) {
            out.extend(kids);
        }
        cursor += 1;
    }
    out
}

/// 削除確認ダイアログ用。指定ノードの子孫数（自分自身は含まない）。
pub fn count_descendants(db: &Arc<Mutex<Database>>, id: u32) -> usize {
    collect_subtree_ids(&list_nodes(db), id).len().saturating_sub(1)
}

/// ノードを削除する。全子孫も連動して消える。戻り値は削除した総数（自分自身を含む）。
/// 実ファイル・実フォルダには一切触れない。
pub fn remove_node(db: &Arc<Mutex<Database>>, id: u32) -> Result<usize, VirtualFolderError> {
    let Ok(db) = db.lock() else {
        return Err(VirtualFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| VirtualFolderError::Db)?;
    let removed;
    {
        let mut table = tx
            .open_table(VIRTUAL_FOLDER_TABLE_V1)
            .map_err(|_| VirtualFolderError::Db)?;
        let nodes = load_nodes(&table)?;
        if !nodes.iter().any(|n| n.id == id) {
            return Err(VirtualFolderError::NotFound);
        }
        let ids = collect_subtree_ids(&nodes, id);
        for target in &ids {
            table.remove(*target).map_err(|_| VirtualFolderError::Db)?;
        }
        removed = ids.len();
    }
    tx.commit().map_err(|_| VirtualFolderError::Db)?;
    Ok(removed)
}

/// ノードを別の親の末尾へ付け替える（子孫は追従）。ROOT_ID を渡すと `/` 直下へ。
/// 自分自身・自分の子孫を新しい親にする移動は CycleDetected。
pub fn move_node(
    db: &Arc<Mutex<Database>>,
    id: u32,
    new_parent_id: u32,
) -> Result<(), VirtualFolderError> {
    let Ok(db) = db.lock() else {
        return Err(VirtualFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| VirtualFolderError::Db)?;
    {
        let mut table = tx
            .open_table(VIRTUAL_FOLDER_TABLE_V1)
            .map_err(|_| VirtualFolderError::Db)?;
        let nodes = load_nodes(&table)?;
        let Some(node) = nodes.iter().find(|n| n.id == id) else {
            return Err(VirtualFolderError::NotFound);
        };
        if new_parent_id != ROOT_ID && !nodes.iter().any(|n| n.id == new_parent_id) {
            return Err(VirtualFolderError::ParentNotFound);
        }
        if collect_subtree_ids(&nodes, id).contains(&new_parent_id) {
            return Err(VirtualFolderError::CycleDetected);
        }
        if node.parent_id == new_parent_id {
            return Ok(());
        }
        let new_order = nodes
            .iter()
            .filter(|n| n.parent_id == new_parent_id)
            .map(|n| n.order + 1)
            .max()
            .unwrap_or(0);
        let moved = VirtualNode {
            parent_id: new_parent_id,
            order: new_order,
            ..node.clone()
        };
        write_node(&mut table, &moved)?;
    }
    tx.commit().map_err(|_| VirtualFolderError::Db)?;
    Ok(())
}

/// 実フォルダが存在しない（消えた・リネームされた・未マウント）ノードか。
pub fn is_link_broken(node: &VirtualNode) -> bool {
    !node.real_path.is_dir()
}

/// リンク切れノードの一覧。
pub fn list_broken(db: &Arc<Mutex<Database>>) -> Vec<VirtualNode> {
    list_nodes(db).into_iter().filter(is_link_broken).collect()
}

/// 実パスのプレフィックスを一括で書き換える。実フォルダの移動・リネーム後に、
/// 該当パス配下を指す全ノードを新しい実パスへ追従させる用途（将来のファイル操作
/// トランザクションからの呼び出しを想定）。比較はパス要素単位で、`/a/b` は `/a/bc` に一致しない。
/// 戻り値は書き換えたノード数。
pub fn rewrite_path_prefix(
    db: &Arc<Mutex<Database>>,
    old_prefix: &Path,
    new_prefix: &Path,
) -> Result<usize, VirtualFolderError> {
    let old_prefix = normalize_path(old_prefix);
    let new_prefix = normalize_path(new_prefix);
    let Ok(db) = db.lock() else {
        return Err(VirtualFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| VirtualFolderError::Db)?;
    let mut rewritten = 0;
    {
        let mut table = tx
            .open_table(VIRTUAL_FOLDER_TABLE_V1)
            .map_err(|_| VirtualFolderError::Db)?;
        let nodes = load_nodes(&table)?;
        for node in nodes {
            let Ok(rest) = node.real_path.strip_prefix(&old_prefix) else {
                continue;
            };
            let real_path = if rest.as_os_str().is_empty() {
                new_prefix.clone()
            } else {
                new_prefix.join(rest)
            };
            write_node(&mut table, &VirtualNode { real_path, ..node })?;
            rewritten += 1;
        }
    }
    if rewritten > 0 {
        tx.commit().map_err(|_| VirtualFolderError::Db)?;
    }
    Ok(rewritten)
}

/// 全ノードの (id → 子idリスト) を返す。ビュー構築用の補助。
pub fn children_index(nodes: &[VirtualNode]) -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    let known: HashSet<u32> = nodes.iter().map(|n| n.id).collect();
    for n in nodes {
        if n.parent_id == ROOT_ID || known.contains(&n.parent_id) {
            map.entry(n.parent_id).or_default().push(n.id);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> Arc<Mutex<Database>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nekoviewer_virtual_folders_test_{}_{}.redb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = Database::create(&path).unwrap();
        let db = Arc::new(Mutex::new(db));
        init_virtual_folder_tables(&db).unwrap();
        db
    }

    fn spec(path: &str, children: Vec<SubtreeSpec>) -> SubtreeSpec {
        SubtreeSpec {
            real_path: PathBuf::from(path),
            name: path.rsplit('/').next().unwrap().to_string(),
            children,
        }
    }

    #[test]
    fn add_node_to_root_and_list() {
        let db = temp_db();
        let a = add_node(&db, ROOT_ID, Path::new("/vt/a"), "a").unwrap();
        let b = add_node(&db, ROOT_ID, Path::new("/vt/b"), "b").unwrap();
        assert_eq!((a.id, a.order), (1, 0));
        assert_eq!((b.id, b.order), (2, 1));
        let names: Vec<_> = list_children(&db, ROOT_ID).into_iter().map(|n| n.name).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn add_node_rejects_missing_parent_and_bad_names() {
        let db = temp_db();
        assert_eq!(
            add_node(&db, 99, Path::new("/vt/a"), "a").unwrap_err(),
            VirtualFolderError::ParentNotFound
        );
        assert_eq!(
            add_node(&db, ROOT_ID, Path::new("/vt/a"), "").unwrap_err(),
            VirtualFolderError::NameEmpty
        );
        let long = "あ".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(
            add_node(&db, ROOT_ID, Path::new("/vt/a"), &long).unwrap_err(),
            VirtualFolderError::NameTooLong
        );
        assert!(list_nodes(&db).is_empty());
    }

    #[test]
    fn duplicate_real_path_is_allowed() {
        let db = temp_db();
        let a1 = add_node(&db, ROOT_ID, Path::new("/vt/a"), "a").unwrap();
        let a2 = add_node(&db, a1.id, Path::new("/vt/a"), "a").unwrap();
        assert_ne!(a1.id, a2.id);
        assert_eq!(a1.real_path, a2.real_path);
    }

    #[test]
    fn add_subtree_preserves_hierarchy_in_preorder() {
        let db = temp_db();
        add_node(&db, ROOT_ID, Path::new("/vt/x"), "x").unwrap();
        let tree = spec(
            "/vt/A",
            vec![spec("/vt/A/b", vec![spec("/vt/A/b/c", vec![])]), spec("/vt/A/d", vec![])],
        );
        let nodes = add_subtree(&db, ROOT_ID, &tree).unwrap();
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["A", "b", "c", "d"]);
        // 根は既存の兄弟(x)の後ろ、子は先頭から採番
        assert_eq!(nodes[0].order, 1);
        assert_eq!(nodes[0].parent_id, ROOT_ID);
        assert_eq!(nodes[1].parent_id, nodes[0].id);
        assert_eq!(nodes[2].parent_id, nodes[1].id);
        assert_eq!((nodes[1].order, nodes[3].order), (0, 1));
        assert_eq!(count_descendants(&db, nodes[0].id), 3);
    }

    #[test]
    fn add_subtree_over_limit_inserts_nothing() {
        let db = temp_db();
        let children = (0..MAX_NODES).map(|i| spec(&format!("/vt/A/{i}"), vec![])).collect();
        let tree = spec("/vt/A", children);
        assert_eq!(tree.node_count(), MAX_NODES + 1);
        assert_eq!(
            add_subtree(&db, ROOT_ID, &tree).unwrap_err(),
            VirtualFolderError::LimitReached
        );
        assert!(list_nodes(&db).is_empty());
    }

    #[test]
    fn remove_node_cascades_to_descendants_only() {
        let db = temp_db();
        let a = add_subtree(
            &db,
            ROOT_ID,
            &spec("/vt/A", vec![spec("/vt/A/b", vec![spec("/vt/A/b/c", vec![])])]),
        )
        .unwrap();
        let other = add_node(&db, ROOT_ID, Path::new("/vt/o"), "o").unwrap();
        // 中間ノード b を消すと c も消え、A と o は残る
        assert_eq!(remove_node(&db, a[1].id).unwrap(), 2);
        let ids: Vec<_> = list_nodes(&db).into_iter().map(|n| n.id).collect();
        assert_eq!(ids, [a[0].id, other.id]);
        assert_eq!(remove_node(&db, a[1].id).unwrap_err(), VirtualFolderError::NotFound);
        // 根を消すと配下すべて
        assert_eq!(remove_node(&db, a[0].id).unwrap(), 1);
    }

    #[test]
    fn move_node_reparents_with_descendants() {
        let db = temp_db();
        let a = add_subtree(&db, ROOT_ID, &spec("/vt/A", vec![spec("/vt/A/b", vec![])])).unwrap();
        let z = add_node(&db, ROOT_ID, Path::new("/vt/z"), "z").unwrap();
        let existing = add_node(&db, z.id, Path::new("/vt/z/e"), "e").unwrap();
        move_node(&db, a[0].id, z.id).unwrap();
        let moved = get_node(&db, a[0].id).unwrap();
        assert_eq!(moved.parent_id, z.id);
        assert_eq!(moved.order, existing.order + 1);
        // 子孫は親idを保ったまま追従
        assert_eq!(get_node(&db, a[1].id).unwrap().parent_id, a[0].id);
        // ルートへ戻せる
        move_node(&db, a[0].id, ROOT_ID).unwrap();
        assert_eq!(get_node(&db, a[0].id).unwrap().parent_id, ROOT_ID);
    }

    #[test]
    fn move_node_rejects_cycles_and_unknown_ids() {
        let db = temp_db();
        let a = add_subtree(
            &db,
            ROOT_ID,
            &spec("/vt/A", vec![spec("/vt/A/b", vec![spec("/vt/A/b/c", vec![])])]),
        )
        .unwrap();
        assert_eq!(move_node(&db, a[0].id, a[0].id).unwrap_err(), VirtualFolderError::CycleDetected);
        assert_eq!(move_node(&db, a[0].id, a[2].id).unwrap_err(), VirtualFolderError::CycleDetected);
        assert_eq!(move_node(&db, a[0].id, 99).unwrap_err(), VirtualFolderError::ParentNotFound);
        assert_eq!(move_node(&db, 99, ROOT_ID).unwrap_err(), VirtualFolderError::NotFound);
        // 失敗した移動は状態を変えない
        assert_eq!(get_node(&db, a[0].id).unwrap().parent_id, ROOT_ID);
    }

    #[test]
    fn move_node_to_same_parent_is_noop() {
        let db = temp_db();
        let a = add_node(&db, ROOT_ID, Path::new("/vt/a"), "a").unwrap();
        add_node(&db, ROOT_ID, Path::new("/vt/b"), "b").unwrap();
        move_node(&db, a.id, ROOT_ID).unwrap();
        assert_eq!(get_node(&db, a.id).unwrap().order, 0);
    }

    #[test]
    fn link_broken_detection() {
        let db = temp_db();
        let alive = add_node(&db, ROOT_ID, &std::env::temp_dir(), "tmp").unwrap();
        let gone = add_node(&db, ROOT_ID, Path::new("/vt/definitely/missing"), "gone").unwrap();
        assert!(!is_link_broken(&alive));
        assert!(is_link_broken(&gone));
        let broken: Vec<_> = list_broken(&db).into_iter().map(|n| n.id).collect();
        assert_eq!(broken, [gone.id]);
    }

    #[test]
    fn rewrite_path_prefix_matches_by_component() {
        let db = temp_db();
        let tree = spec("/vt/A", vec![spec("/vt/A/sub", vec![])]);
        let nodes = add_subtree(&db, ROOT_ID, &tree).unwrap();
        let ab = add_node(&db, ROOT_ID, Path::new("/vt/AB"), "AB").unwrap();
        let changed = rewrite_path_prefix(&db, Path::new("/vt/A"), Path::new("/vt/Z")).unwrap();
        assert_eq!(changed, 2);
        assert_eq!(get_node(&db, nodes[0].id).unwrap().real_path, PathBuf::from("/vt/Z"));
        assert_eq!(get_node(&db, nodes[1].id).unwrap().real_path, PathBuf::from("/vt/Z/sub"));
        // 名前が前方一致するだけの別フォルダは対象外
        assert_eq!(get_node(&db, ab.id).unwrap().real_path, PathBuf::from("/vt/AB"));
        // 表示名・親子関係は変わらない
        assert_eq!(get_node(&db, nodes[1].id).unwrap().parent_id, nodes[0].id);
        assert_eq!(get_node(&db, nodes[0].id).unwrap().name, "A");
    }

    #[test]
    fn rewrite_path_prefix_without_match_changes_nothing() {
        let db = temp_db();
        add_node(&db, ROOT_ID, Path::new("/vt/a"), "a").unwrap();
        assert_eq!(rewrite_path_prefix(&db, Path::new("/vt/none"), Path::new("/vt/z")).unwrap(), 0);
    }

    #[test]
    fn children_index_groups_by_parent() {
        let db = temp_db();
        let a = add_subtree(&db, ROOT_ID, &spec("/vt/A", vec![spec("/vt/A/b", vec![])])).unwrap();
        let index = children_index(&list_nodes(&db));
        assert_eq!(index[&ROOT_ID], [a[0].id]);
        assert_eq!(index[&a[0].id], [a[1].id]);
    }
}
