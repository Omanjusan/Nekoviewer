//! ファイル単位の保存設定（見開き・ソート・登録サムネ・タグ・お気に入り所属）を、ファイルID
//! （`file_identity`）をキーにした1レコードへ束ねる第2世代の保存（`file_settings_v2`）。
//!
//! 各フィールドは3状態（`Slot`）を持つ。
//! - `Unmigrated`: まだ旧v1（パスキー）から移していない。読むときは旧パスキー（`legacy_keys`）で引く
//! - `Set(値)`: v2が正。旧v1は見ない
//! - `Cleared`: ユーザーが解除した。旧v1が残っていても復活させない
//!
//! 書き込み時に、そのファイルの未移行フィールドをまとめて旧v1から移す。
//! 旧v1の行は消さないので、ID化前に作られた設定も、移動・リネーム後に旧パスキー経由で届く。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition};

use crate::file_identity::{put_str, Reader};
use crate::favorites::FAVORITE_MEMBERSHIP_TABLE;
use crate::spread_state::{
    decode_tag_ids, owner_tx, Owner, ARCHIVE_SORT_TABLE_V1, ARCHIVE_TAGS_TABLE_V1, SPREAD_TABLE,
    THUMBNAIL_SELECTION_TABLE_V1, THUMBNAIL_SELECTION_TABLE_V2,
};

/// ファイルID → `FileSettings::encode` のバイト列。
pub const FILE_SETTINGS_TABLE_V2: TableDefinition<u64, &[u8]> = TableDefinition::new("file_settings_v2");
const RECORD_VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Slot<T> {
    /// 旧v1から未移行（読み取りは旧パスキーで引く）。
    #[default]
    Unmigrated,
    Set(T),
    /// ユーザーが解除した（旧v1が残っていても復活させない）。
    Cleared,
}

impl<T> Slot<T> {
    fn is_set(&self) -> bool {
        matches!(self, Self::Set(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FileSettings {
    /// (page_mode, spread_offset)
    pub spread: Slot<(u8, i32)>,
    /// (sort_key, ascending)
    pub sort: Slot<(u8, bool)>,
    /// (entry_name, source_kind)
    pub thumb: Slot<(String, u8)>,
    pub tags: Slot<Vec<u64>>,
    pub favorite: Slot<Vec<u8>>,
}

impl FileSettings {
    /// 値を持つスロットがあるか（空のIDの判定用。全て Cleared/Unmigrated なら false）。
    pub fn any_set(&self) -> bool {
        self.spread.is_set()
            || self.sort.is_set()
            || self.thumb.is_set()
            || self.tags.is_set()
            || self.favorite.is_set()
    }

    fn encode(&self) -> Vec<u8> {
        let mut b = vec![RECORD_VERSION];
        put_slot(&mut b, &self.spread, |b, (m, o)| {
            b.push(*m);
            b.extend(o.to_le_bytes());
        });
        put_slot(&mut b, &self.sort, |b, (k, a)| {
            b.push(*k);
            b.push(u8::from(*a));
        });
        put_slot(&mut b, &self.thumb, |b, (e, k)| {
            put_str(b, e);
            b.push(*k);
        });
        put_slot(&mut b, &self.tags, |b, ids| {
            b.extend((ids.len() as u32).to_le_bytes());
            for id in ids {
                b.extend(id.to_le_bytes());
            }
        });
        put_slot(&mut b, &self.favorite, |b, ids| {
            b.extend((ids.len() as u32).to_le_bytes());
            b.extend(ids);
        });
        b
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { b: bytes, pos: 0 };
        if r.u8()? != RECORD_VERSION {
            return None;
        }
        let spread = get_slot(&mut r, |r| Some((r.u8()?, i32::from_le_bytes(r.take(4)?.try_into().ok()?))))?;
        let sort = get_slot(&mut r, |r| Some((r.u8()?, r.u8()? != 0)))?;
        let thumb = get_slot(&mut r, |r| Some((r.string()?, r.u8()?)))?;
        let tags = get_slot(&mut r, |r| {
            let n = r.u32()? as usize;
            (0..n).map(|_| r.u64()).collect::<Option<Vec<u64>>>()
        })?;
        let favorite = get_slot(&mut r, |r| {
            let n = r.u32()? as usize;
            Some(r.take(n)?.to_vec())
        })?;
        Some(Self { spread, sort, thumb, tags, favorite })
    }
}

fn put_slot<T>(b: &mut Vec<u8>, slot: &Slot<T>, put: impl FnOnce(&mut Vec<u8>, &T)) {
    match slot {
        Slot::Unmigrated => b.push(0),
        Slot::Set(v) => {
            b.push(1);
            put(b, v);
        }
        Slot::Cleared => b.push(2),
    }
}

fn get_slot<T>(r: &mut Reader, get: impl FnOnce(&mut Reader) -> Option<T>) -> Option<Slot<T>> {
    Some(match r.u8()? {
        0 => Slot::Unmigrated,
        1 => Slot::Set(get(r)?),
        2 => Slot::Cleared,
        _ => return None,
    })
}

/// 現在有効な値（`Unmigrated` は旧v1で解決済み、`Cleared` は None）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Effective {
    pub spread: Option<(u8, i32)>,
    pub sort: Option<(u8, bool)>,
    pub thumb: Option<(String, u8)>,
    pub tags: Option<Vec<u64>>,
    /// 所属お気に入りフォルダID。Some(空) は未整理のお気に入り、None はお気に入りではない。
    pub favorite: Option<Vec<u8>>,
}

fn legacy_spread(tx: &ReadTransaction, keys: &[&str]) -> Option<(u8, i32)> {
    let t = tx.open_table(SPREAD_TABLE).ok()?;
    keys.iter().find_map(|k| t.get(*k).ok().flatten().map(|v| v.value()))
}

fn legacy_sort(tx: &ReadTransaction, keys: &[&str]) -> Option<(u8, bool)> {
    let t = tx.open_table(ARCHIVE_SORT_TABLE_V1).ok()?;
    keys.iter().find_map(|k| t.get(*k).ok().flatten().map(|v| v.value()))
}

/// 登録サムネ。v2（生成方法つき）を優先し、無ければv1（entry_nameのみ。生成方法は全体）。
fn legacy_thumb(tx: &ReadTransaction, keys: &[&str]) -> Option<(String, u8)> {
    let v2 = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2).ok();
    let v1 = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok();
    keys.iter().find_map(|k| {
        if let Some(g) = v2.as_ref().and_then(|t| t.get(*k).ok().flatten()) {
            let (entry, kind) = g.value();
            return Some((entry.to_string(), kind));
        }
        v1.as_ref()
            .and_then(|t| t.get(*k).ok().flatten())
            .map(|g| (g.value().to_string(), 0))
    })
}

fn legacy_tags(tx: &ReadTransaction, keys: &[&str]) -> Option<Vec<u64>> {
    let t = tx.open_table(ARCHIVE_TAGS_TABLE_V1).ok()?;
    keys.iter().find_map(|k| t.get(*k).ok().flatten().map(|v| decode_tag_ids(v.value())))
}

fn legacy_favorite(tx: &ReadTransaction, keys: &[&str]) -> Option<Vec<u8>> {
    let t = tx.open_table(FAVORITE_MEMBERSHIP_TABLE).ok()?;
    keys.iter().find_map(|k| t.get(*k).ok().flatten().map(|v| v.value().to_vec()))
}

fn legacy_group(tx: &ReadTransaction, keys: &[&str]) -> Effective {
    Effective {
        spread: legacy_spread(tx, keys),
        sort: legacy_sort(tx, keys),
        thumb: legacy_thumb(tx, keys),
        tags: legacy_tags(tx, keys),
        favorite: legacy_favorite(tx, keys),
    }
}

impl FileSettings {
    /// 旧v1から未移行のスロットがあるか。
    fn has_unmigrated(&self) -> bool {
        matches!(self.spread, Slot::Unmigrated)
            || matches!(self.sort, Slot::Unmigrated)
            || matches!(self.thumb, Slot::Unmigrated)
            || matches!(self.tags, Slot::Unmigrated)
            || matches!(self.favorite, Slot::Unmigrated)
    }
}

fn stored_tx(tx: &ReadTransaction, id: u64) -> Option<FileSettings> {
    let t = tx.open_table(FILE_SETTINGS_TABLE_V2).ok()?;
    FileSettings::decode(t.get(id).ok().flatten()?.value())
}

/// 持ち主の現在有効な設定。IDが解決済みならv2（未移行のスロットは旧パスキーで補う）、
/// 未解決ならパスキーの旧v1。
pub(crate) fn effective_tx(tx: &ReadTransaction, owner: &Owner) -> Effective {
    match owner {
        Owner::Path(key) => legacy_group(tx, &[key.as_str()]),
        Owner::Id(rec) => {
            let stored = stored_tx(tx, rec.id).unwrap_or_default();
            let keys = rec.legacy_keys();
            // 未移行のスロットがある時だけ旧v1を引く。
            let legacy = stored.has_unmigrated().then(|| legacy_group(tx, &keys)).unwrap_or_default();
            Effective {
                spread: resolve(stored.spread, legacy.spread),
                sort: resolve(stored.sort, legacy.sort),
                thumb: resolve(stored.thumb, legacy.thumb),
                tags: resolve(stored.tags, legacy.tags),
                favorite: resolve(stored.favorite, legacy.favorite),
            }
        }
    }
}

fn resolve<T>(slot: Slot<T>, legacy: Option<T>) -> Option<T> {
    match slot {
        Slot::Set(v) => Some(v),
        Slot::Cleared => None,
        Slot::Unmigrated => legacy,
    }
}

/// `dir` 配下（接頭辞 "dir\0"）の設定を、ファイル名ごとにまとめて返す。ID未解決のファイルは
/// 旧v1のパスキーで、解決済みのファイルはID経由で引く。何も設定が無いファイルは含まない。
pub(crate) fn dir_effective_tx(tx: &ReadTransaction, prefix: &str) -> HashMap<String, Effective> {
    let mut out: HashMap<String, Effective> = HashMap::new();
    if let Ok(t) = tx.open_table(SPREAD_TABLE) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().spread = Some(e.1.value());
            }
        }
    }
    if let Ok(t) = tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().sort = Some(e.1.value());
            }
        }
    }
    // 登録サムネはv1を先に入れ、v2で上書きする。
    if let Ok(t) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().thumb =
                    Some((e.1.value().to_string(), 0));
            }
        }
    }
    if let Ok(t) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                let (entry, kind) = e.1.value();
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().thumb =
                    Some((entry.to_string(), kind));
            }
        }
    }
    if let Ok(t) = tx.open_table(ARCHIVE_TAGS_TABLE_V1) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().tags =
                    Some(decode_tag_ids(e.1.value()));
            }
        }
    }
    if let Ok(t) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) {
        if let Ok(range) = t.range(prefix..) {
            for e in range.flatten() {
                if !e.0.value().starts_with(prefix) {
                    break;
                }
                out.entry(e.0.value()[prefix.len()..].to_string()).or_default().favorite =
                    Some(e.1.value().to_vec());
            }
        }
    }
    for rec in crate::file_identity::dir_records_tx(tx, prefix) {
        let name = rec.path_key[prefix.len()..].to_string();
        let eff = effective_tx(tx, &Owner::Id(rec));
        if eff == Effective::default() {
            out.remove(&name);
        } else {
            out.insert(name, eff);
        }
    }
    out.retain(|_, e| *e != Effective::default());
    out
}

/// 設定を読み→変更→書く。IDを同期で解決でき、未移行のスロットは旧v1から一括で引き継いでから `f` を適用する。
/// IDを解決できない（ファイルが無い等）場合は None を返し、呼び出し側が従来どおり旧v1へ書く。
/// Some(成否)。
pub(crate) fn modify(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    f: impl FnOnce(&mut FileSettings),
) -> Option<bool> {
    let rec = crate::file_identity::ensure_record(db, dir, filename)?;
    let guard = db.lock().ok()?;
    let ok = (|| {
        let rtx = guard.begin_read().ok()?;
        let mut cur = stored_tx(&rtx, rec.id).unwrap_or_default();
        if cur.has_unmigrated() {
            let legacy = legacy_group(&rtx, &rec.legacy_keys());
            migrate(&mut cur.spread, legacy.spread);
            migrate(&mut cur.sort, legacy.sort);
            migrate(&mut cur.thumb, legacy.thumb);
            migrate(&mut cur.tags, legacy.tags);
            migrate(&mut cur.favorite, legacy.favorite);
        }
        drop(rtx);
        f(&mut cur);
        let wtx = guard.begin_write().ok()?;
        {
            let mut t = wtx.open_table(FILE_SETTINGS_TABLE_V2).ok()?;
            t.insert(rec.id, cur.encode().as_slice()).ok()?;
        }
        wtx.commit().ok()
    })()
    .is_some();
    Some(ok)
}

fn migrate<T>(slot: &mut Slot<T>, legacy: Option<T>) {
    if matches!(slot, Slot::Unmigrated) {
        *slot = match legacy {
            Some(v) => Slot::Set(v),
            None => Slot::Cleared,
        };
    }
}

/// 解決済みのIDレコードの現在有効な設定を読む（持ち主＝そのID）。
pub(crate) fn read_effective_for_record(
    db: &Arc<Mutex<Database>>,
    rec: &crate::file_identity::FileRecord,
) -> Option<Effective> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    Some(effective_tx(&tx, &Owner::Id(rec.clone())))
}

/// 持ち主の設定を読む（読み取りトランザクションを開いて `effective_tx`）。
pub(crate) fn read_effective(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<Effective> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    Some(effective_tx(&tx, &owner_tx(&tx, dir, filename)))
}

/// お気に入りを全ディレクトリ横断で列挙する（お気に入り一覧表示用）。戻り値: (dir, filename, 所属フォルダID)。
/// IDが解決済みのファイルは現在のパスで、旧v1の行しか無いファイルはその旧パスで返す。
/// IDレコードを全件走査するので、頻繁に呼ぶ用途には使わない。
pub(crate) fn all_favorites_tx(tx: &ReadTransaction) -> Vec<(PathBuf, String, Vec<u8>)> {
    // 旧v1の行（キー→所属）。お気に入りは少数なので全件をメモリに載せる。
    let mut legacy: HashMap<String, Vec<u8>> = HashMap::new();
    if let Ok(t) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) {
        if let Ok(iter) = t.iter() {
            for e in iter.flatten() {
                legacy.insert(e.0.value().to_string(), e.1.value().to_vec());
            }
        }
    }
    // v2で確定している所属（Set/Cleared）。Unmigrated のIDは含まれない。
    let mut decided: HashMap<u64, Option<Vec<u8>>> = HashMap::new();
    if let Ok(t) = tx.open_table(FILE_SETTINGS_TABLE_V2) {
        if let Ok(iter) = t.iter() {
            for e in iter.flatten() {
                match FileSettings::decode(e.1.value()).map(|s| s.favorite) {
                    Some(Slot::Set(v)) => {
                        decided.insert(e.0.value(), Some(v));
                    }
                    Some(Slot::Cleared) => {
                        decided.insert(e.0.value(), None);
                    }
                    _ => {}
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut consumed: HashSet<String> = HashSet::new();
    for rec in crate::file_identity::all_records_tx(tx) {
        // このIDの旧キーにあるv1の行は、IDの所属として扱う（旧パスのまま二重に出さない）。
        let legacy_hit = rec
            .legacy_keys()
            .into_iter()
            .find_map(|k| legacy.get(k).map(|v| (k.to_owned(), v.clone())));
        for k in rec.legacy_keys() {
            if legacy.contains_key(k) {
                consumed.insert(k.to_owned());
            }
        }
        let membership = match decided.get(&rec.id) {
            Some(decided) => decided.clone(),
            None => legacy_hit.map(|(_, v)| v),
        };
        if let Some(ids) = membership {
            if let Some((dir, name)) = rec.path_key.split_once('\0') {
                out.push((PathBuf::from(dir), name.to_owned(), ids));
            }
        }
    }
    // どのIDにも結び付かない旧v1の行（ID未解決のファイル）は、そのパスで返す。
    for (key, ids) in legacy {
        if consumed.contains(&key) {
            continue;
        }
        if let Some((dir, name)) = key.split_once('\0') {
            out.push((PathBuf::from(dir), name.to_owned(), ids));
        }
    }
    out
}

/// 全てのファイル設定から、お気に入りフォルダ `folder_id` の所属を外す（フォルダ削除用）。
/// 旧v1の行は呼び出し側が別に直す。IDを持つファイルのv2だけを書き換える。
pub(crate) fn remove_folder_from_settings(
    tx: &redb::WriteTransaction,
    folder_id: u8,
) -> Result<(), redb::Error> {
    let mut t = tx.open_table(FILE_SETTINGS_TABLE_V2)?;
    let updates: Vec<(u64, FileSettings)> = {
        let mut out = Vec::new();
        for e in t.iter()?.flatten() {
            let Some(mut s) = FileSettings::decode(e.1.value()) else { continue };
            if let Slot::Set(ids) = &mut s.favorite {
                if ids.contains(&folder_id) {
                    ids.retain(|&f| f != folder_id);
                    out.push((e.0.value(), s));
                }
            }
        }
        out
    };
    for (id, s) in updates {
        t.insert(id, s.encode().as_slice())?;
    }
    Ok(())
}

/// そのIDに値のあるスロットがあるか（`id_has_user_data` 用）。
pub fn id_has_settings(db: &Database, id: u64) -> bool {
    let Ok(tx) = db.begin_read() else { return false };
    stored_tx(&tx, id).is_some_and(|s| s.any_set())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips_all_slot_states() {
        let s = FileSettings {
            spread: Slot::Set((2, -1)),
            sort: Slot::Cleared,
            thumb: Slot::Set(("日本語/cover.jpg".to_owned(), 1)),
            tags: Slot::Set(vec![3, 1, u64::MAX]),
            favorite: Slot::Set(vec![]),
        };
        assert_eq!(FileSettings::decode(&s.encode()), Some(s.clone()));
        assert_eq!(FileSettings::decode(&FileSettings::default().encode()), Some(FileSettings::default()));
        let bytes = s.encode();
        assert_eq!(FileSettings::decode(&bytes[..bytes.len() - 1]), None);
        let mut bad_tag = bytes;
        bad_tag[1] = 9;
        assert_eq!(FileSettings::decode(&bad_tag), None);
        assert_eq!(FileSettings::decode(&[]), None);
    }

    #[test]
    fn any_set_ignores_cleared_and_unmigrated() {
        assert!(!FileSettings::default().any_set());
        let cleared = FileSettings { spread: Slot::Cleared, sort: Slot::Cleared, ..Default::default() };
        assert!(!cleared.any_set());
        let one = FileSettings { sort: Slot::Set((1, true)), ..cleared };
        assert!(one.any_set());
    }

    #[test]
    fn migrate_turns_unmigrated_into_set_or_cleared_and_keeps_existing() {
        let mut a: Slot<u8> = Slot::Unmigrated;
        migrate(&mut a, Some(5));
        assert_eq!(a, Slot::Set(5));
        let mut b: Slot<u8> = Slot::Unmigrated;
        migrate(&mut b, None);
        assert_eq!(b, Slot::Cleared);
        let mut c: Slot<u8> = Slot::Set(1);
        migrate(&mut c, Some(9));
        assert_eq!(c, Slot::Set(1));
        let mut d: Slot<u8> = Slot::Cleared;
        migrate(&mut d, Some(9));
        assert_eq!(d, Slot::Cleared, "解除済みは旧v1が残っていても復活しない");
    }

    // ---- タグ・お気に入り所属のID化（3b）----

    use crate::favorites::{self, FAVORITE_MEMBERSHIP_TABLE};
    use crate::spread_state::{
        open_spread_db, read_archive_tags, write_archive_tags, ARCHIVE_TAGS_TABLE_V1,
    };

    fn real_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("nekoviewer_file_settings_test_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn real_file(dir: &Path, name: &str, len: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        p
    }

    fn new_db(dir: &Path) -> Arc<Mutex<Database>> {
        let db = open_spread_db(dir).unwrap();
        favorites::init_favorite_tables(&db).unwrap();
        db
    }

    fn seed_v1_tags(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, ids: &[u64]) {
        let key = crate::spread_state::make_key(dir, name);
        let bytes: Vec<u8> = ids.iter().flat_map(|i| i.to_le_bytes()).collect();
        let g = db.lock().unwrap();
        let tx = g.begin_write().unwrap();
        {
            let mut t = tx.open_table(ARCHIVE_TAGS_TABLE_V1).unwrap();
            t.insert(key.as_str(), bytes.as_slice()).unwrap();
        }
        tx.commit().unwrap();
    }

    fn seed_v1_favorite(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, folders: &[u8]) {
        let key = crate::spread_state::make_key(dir, name);
        let g = db.lock().unwrap();
        let tx = g.begin_write().unwrap();
        {
            let mut t = tx.open_table(FAVORITE_MEMBERSHIP_TABLE).unwrap();
            t.insert(key.as_str(), folders).unwrap();
        }
        tx.commit().unwrap();
    }

    fn canon(p: &Path) -> PathBuf {
        p.canonicalize().unwrap()
    }

    #[test]
    fn tags_follow_a_moved_file_and_empty_clears() {
        let root = real_dir("tags_move");
        let db = new_db(&root);
        let (d1, d2) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let p = real_file(&d1, "x.zip", 5000);
        assert!(write_archive_tags(&db, &d1, "x.zip", &[3, 1, 2]));
        assert_eq!(read_archive_tags(&db, &d1, "x.zip"), vec![3, 1, 2]);
        std::fs::rename(&p, d2.join("y.zip")).unwrap();
        crate::file_identity::ensure_record(&db, &d2, "y.zip").unwrap();
        assert_eq!(read_archive_tags(&db, &d2, "y.zip"), vec![3, 1, 2]);
        assert!(read_archive_tags(&db, &d1, "x.zip").is_empty());
        assert!(write_archive_tags(&db, &d2, "y.zip", &[]));
        assert!(read_archive_tags(&db, &d2, "y.zip").is_empty());
    }

    #[test]
    fn legacy_tags_migrate_and_cleared_tags_do_not_resurrect() {
        let root = real_dir("tags_legacy");
        let db = new_db(&root);
        real_file(&root, "a.zip", 3000);
        real_file(&root, "b.zip", 4000);
        seed_v1_tags(&db, &root, "a.zip", &[7, 8]);
        seed_v1_tags(&db, &root, "b.zip", &[9]);
        // IDが無いうちは旧v1を読む。
        assert_eq!(read_archive_tags(&db, &root, "a.zip"), vec![7, 8]);
        // 別のスロットの書き込みでも、タグは旧v1から引き継がれて残る。
        favorites::set_membership(&db, &root, "a.zip", &[1]);
        assert_eq!(read_archive_tags(&db, &root, "a.zip"), vec![7, 8]);
        // 解除したタグは、旧v1が残っていても復活しない。
        assert!(write_archive_tags(&db, &root, "b.zip", &[]));
        assert!(read_archive_tags(&db, &root, "b.zip").is_empty());
    }

    #[test]
    fn favorite_membership_round_trip_and_follows_a_move() {
        let root = real_dir("fav_move");
        let db = new_db(&root);
        let (d1, d2) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let p = real_file(&d1, "x.zip", 5000);
        assert_eq!(favorites::get_membership(&db, &d1, "x.zip"), None);
        favorites::set_membership(&db, &d1, "x.zip", &[]);
        assert_eq!(favorites::get_membership(&db, &d1, "x.zip"), Some(vec![]));
        favorites::set_membership(&db, &d1, "x.zip", &[2, 5]);
        std::fs::rename(&p, d2.join("y.zip")).unwrap();
        crate::file_identity::ensure_record(&db, &d2, "y.zip").unwrap();
        assert_eq!(favorites::get_membership(&db, &d2, "y.zip"), Some(vec![2, 5]));
        assert_eq!(favorites::get_membership(&db, &d1, "x.zip"), None);
        favorites::remove_favorite(&db, &d2, "y.zip");
        assert_eq!(favorites::get_membership(&db, &d2, "y.zip"), None);
    }

    #[test]
    fn dir_favorites_and_memberships_mix_resolved_and_legacy() {
        let root = real_dir("fav_list");
        let db = new_db(&root);
        let id = real_file(&root, "id.zip", 3000);
        let legacy = real_file(&root, "legacy.zip", 4000);
        favorites::set_membership(&db, &root, "id.zip", &[1]);
        seed_v1_favorite(&db, &root, "legacy.zip", &[]);
        let mut listed = favorites::list_dir_favorites(&db, &root);
        listed.sort();
        assert_eq!(listed, vec![("id.zip".to_owned(), vec![1]), ("legacy.zip".to_owned(), vec![])]);
        let got = favorites::memberships_for_paths(&db, &[id.clone(), legacy.clone()]);
        assert_eq!(got[&id], vec![1]);
        assert_eq!(got[&legacy], Vec::<u8>::new());
    }

    #[test]
    fn cross_view_lists_moved_files_at_their_new_path_and_skips_cleared() {
        let root = real_dir("fav_cross");
        let db = new_db(&root);
        let (d1, d2) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let moved = real_file(&d1, "moved.zip", 5000);
        real_file(&d1, "stay.zip", 6000);
        real_file(&d1, "gone.zip", 7000);
        favorites::set_membership(&db, &d1, "moved.zip", &[4]);
        favorites::set_membership(&db, &d1, "stay.zip", &[4]);
        favorites::set_membership(&db, &d1, "gone.zip", &[4]);
        favorites::remove_favorite(&db, &d1, "gone.zip");
        std::fs::rename(&moved, d2.join("renamed.zip")).unwrap();
        crate::file_identity::ensure_record(&db, &d2, "renamed.zip").unwrap();

        let mut got = favorites::list_files_in_folder(&db, 4);
        got.sort();
        assert_eq!(
            got,
            vec![
                (canon(&d1), "stay.zip".to_owned()),
                (canon(&d2), "renamed.zip".to_owned()),
            ]
        );
    }

    #[test]
    fn cross_view_reaches_unmigrated_legacy_favorite_of_a_moved_file() {
        let root = real_dir("fav_cross_legacy");
        let db = new_db(&root);
        let (d1, d2) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let p = real_file(&d1, "x.zip", 5000);
        seed_v1_favorite(&db, &d1, "x.zip", &[]);
        // IDだけ作られ（お気に入りは旧v1のまま）、その後に移動された。
        crate::file_identity::ensure_record(&db, &d1, "x.zip").unwrap();
        std::fs::rename(&p, d2.join("y.zip")).unwrap();
        crate::file_identity::ensure_record(&db, &d2, "y.zip").unwrap();
        // 未整理のお気に入りが、旧パスではなく移動先のパスで列挙される（二重にも出ない）。
        assert_eq!(favorites::list_unsorted_files(&db), vec![(canon(&d2), "y.zip".to_owned())]);
    }

    #[test]
    fn cross_view_keeps_unresolved_legacy_rows_at_their_own_path() {
        let root = real_dir("fav_cross_unresolved");
        let db = new_db(&root);
        // 実体の無いファイル（ID化できない）の旧v1の行は、そのパスのまま返す。
        seed_v1_favorite(&db, &root, "ghost.zip", &[2]);
        assert_eq!(favorites::list_files_in_folder(&db, 2), vec![(canon(&root), "ghost.zip".to_owned())]);
    }

    #[test]
    fn deleting_a_favorite_folder_removes_it_from_ids_and_legacy_rows() {
        let root = real_dir("fav_delete");
        let db = new_db(&root);
        let folder = favorites::create_folder(&db, "f", "x", 0).unwrap();
        let other = favorites::create_folder(&db, "g", "x", 0).unwrap();
        real_file(&root, "id.zip", 3000);
        real_file(&root, "legacy.zip", 4000);
        favorites::set_membership(&db, &root, "id.zip", &[folder.id, other.id]);
        seed_v1_favorite(&db, &root, "legacy.zip", &[folder.id]);
        favorites::delete_folder(&db, folder.id).unwrap();
        assert_eq!(favorites::get_membership(&db, &root, "id.zip"), Some(vec![other.id]));
        // 旧v1の行は、フォルダが外れて「未整理のお気に入り」として残る。
        assert_eq!(favorites::get_membership(&db, &root, "legacy.zip"), Some(vec![]));
    }

    #[test]
    fn tags_and_favorites_count_as_user_data_for_the_id() {
        let root = real_dir("tags_userdata");
        let db = new_db(&root);
        real_file(&root, "a.zip", 3000);
        let rec = crate::file_identity::ensure_record(&db, &root, "a.zip").unwrap();
        assert!(!crate::spread_state::id_has_user_data(&db, rec.id));
        favorites::set_membership(&db, &root, "a.zip", &[]);
        assert!(crate::spread_state::id_has_user_data(&db, rec.id));
        favorites::remove_favorite(&db, &root, "a.zip");
        assert!(!crate::spread_state::id_has_user_data(&db, rec.id));
        assert!(write_archive_tags(&db, &root, "a.zip", &[1]));
        assert!(crate::spread_state::id_has_user_data(&db, rec.id));
    }

    #[test]
    fn legacy_tag_and_favorite_rows_count_as_legacy_data_for_overlay_and_backfill() {
        let root = real_dir("tags_legacy_detect");
        let db = new_db(&root);
        let tagged = real_file(&root, "tagged.zip", 3000);
        let fav = real_file(&root, "fav.zip", 3100);
        let plain = real_file(&root, "plain.zip", 3200);
        seed_v1_tags(&db, &root, "tagged.zip", &[1]);
        seed_v1_favorite(&db, &root, "fav.zip", &[]);
        let overlay = crate::spread_state::paths_without_identity_or_legacy(
            &db,
            &[tagged.clone(), fav.clone(), plain.clone()],
        );
        assert_eq!(overlay, std::iter::once(plain).collect());
        let keys = crate::spread_state::legacy_data_keys_without_id(&db);
        assert!(keys.iter().any(|k| k.ends_with("tagged.zip")));
        assert!(keys.iter().any(|k| k.ends_with("fav.zip")));
        assert!(!keys.iter().any(|k| k.ends_with("plain.zip")));
    }
}
