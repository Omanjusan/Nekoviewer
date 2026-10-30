//! サムネのdHash（`thumb_hash`）の保存。「似ている画像」マーカーの照合に使う。
//!
//! - 表 `thumb_hash_v1`: ファイルID → (世代, サムネの更新日時, ハッシュ)。ID層と同じ共有DBに置く
//!   （フォルダをまたいで照合するため。サムネ本体は各フォルダのDBにあるので、そこには置かない）
//! - サムネが作り直された（更新日時が違う）か、ハッシュの世代が違う行は、次の走査で計算し直す
//! - ハッシュが作れないサムネ（のっぺり・壊れ）も「作れなかった」行として残し、毎回計算し直さない
//! - 表は最初の保存で作る（旧DBの構造を起動だけで変えない）。読み取りと `forget_id_tx` は作らない
//! - IDが消えたら（`file_identity::delete_id` など）行を連動して消す

use std::path::Path;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, TableDefinition, TableHandle, WriteTransaction};

use crate::thumb_hash;

const HASH_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("thumb_hash_v1");

/// 1ファイルぶんの保存内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashRow {
    /// `thumb_hash::HASH_VERSION`。
    pub version: u8,
    /// ハッシュを作った時のサムネの更新日時（フォルダ別DBの `thumbs` に入っている値）。
    pub thumb_mtime: i64,
    /// ハッシュ。作れなかったサムネは None。
    pub hash: Option<u64>,
}

impl HashRow {
    fn encode(&self) -> [u8; 18] {
        let mut out = [0_u8; 18];
        out[0] = self.version;
        out[1..9].copy_from_slice(&self.thumb_mtime.to_le_bytes());
        out[9..17].copy_from_slice(&self.hash.unwrap_or(0).to_le_bytes());
        out[17] = u8::from(self.hash.is_some());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; 18] = bytes.try_into().ok()?;
        let hash = match bytes[17] {
            0 => None,
            1 => Some(u64::from_le_bytes(bytes[9..17].try_into().ok()?)),
            _ => return None,
        };
        Some(Self { version: bytes[0], thumb_mtime: i64::from_le_bytes(bytes[1..9].try_into().ok()?), hash })
    }

    /// このサムネ（更新日時）に対して、今の世代で計算済みか。
    fn is_current(&self, thumb_mtime: i64) -> bool {
        self.version == thumb_hash::HASH_VERSION && self.thumb_mtime == thumb_mtime
    }
}

/// そのIDの保存内容。無ければ（表が無くても）None。
#[allow(dead_code)] // 照合（Phase 3）で使う。
pub fn get(db: &Arc<Mutex<Database>>, id: u64) -> Option<HashRow> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    let table = tx.open_table(HASH_TABLE).ok()?;
    HashRow::decode(table.get(id).ok()??.value())
}

/// 行をまとめて書く（1トランザクション）。成功したら true。
fn put_all(db: &Arc<Mutex<Database>>, rows: &[(u64, HashRow)]) -> bool {
    if rows.is_empty() {
        return true;
    }
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    {
        let Ok(mut table) = tx.open_table(HASH_TABLE) else { return false };
        for (id, row) in rows {
            if table.insert(*id, row.encode().as_slice()).is_err() {
                return false;
            }
        }
    }
    tx.commit().is_ok()
}

/// IDが消える時の連動（`delete_id` などの書き込みトランザクションの中で呼ぶ）。表が無ければ何もしない。Some = 成功。
pub(crate) fn forget_id_tx(tx: &WriteTransaction, id: u64) -> Option<()> {
    let exists = tx.list_tables().ok()?.any(|h| h.name() == HASH_TABLE.name());
    if !exists {
        return Some(());
    }
    tx.open_table(HASH_TABLE).ok()?.remove(id).ok()?;
    Some(())
}

/// 書き込みをまとめる件数（1件ごとにコミットすると遅いため）。
const WRITE_BATCH: usize = 64;

/// フォルダ `dir` の `names` について、サムネのハッシュが無い・古いものを計算して保存する。
/// 戻り値は、新しく書いた件数（「作れなかった」行を含む）。
///
/// - `read_thumb(ファイル名)` はサムネの (更新日時, JPEG)。サムネが無いファイルは None（生成後の走査で拾う）
/// - ファイルIDがまだ無いものは飛ばす（ID解決が済んでから呼ぶ）
/// - `keep_going` が false になったら、次のファイルの前で打ち切る（溜まった分は書く）
pub fn hash_files(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    names: &[String],
    read_thumb: &dyn Fn(&str) -> Option<(i64, Vec<u8>)>,
    keep_going: &dyn Fn() -> bool,
) -> usize {
    let prefix = crate::spread_state::dir_prefix(dir);
    let mut pending: Vec<(u64, HashRow)> = Vec::new();
    let mut written = 0;
    for name in names {
        if !keep_going() {
            break;
        }
        let Some(rec) = crate::file_identity::lookup(db, &format!("{prefix}{name}")) else { continue };
        let Some((thumb_mtime, jpeg)) = read_thumb(name) else { continue };
        if get(db, rec.id).is_some_and(|row| row.is_current(thumb_mtime)) {
            continue;
        }
        let hash = thumb_hash::dhash_from_image_bytes(&jpeg);
        pending.push((rec.id, HashRow { version: thumb_hash::HASH_VERSION, thumb_mtime, hash }));
        if pending.len() >= WRITE_BATCH {
            if put_all(db, &pending) {
                written += pending.len();
            }
            pending.clear();
        }
    }
    if put_all(db, &pending) {
        written += pending.len();
    }
    written
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::spread_state::open_spread_db;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root =
                std::env::temp_dir().join(format!("nekoviewer_thumb_hash_store_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn register(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, fill: u8) -> u64 {
        std::fs::write(dir.join(name), vec![fill; 64]).unwrap();
        // 書いたばかりのファイルは書き込み中を疑ってFPを取らないので、古い更新日時にする。
        let f = std::fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        crate::file_identity::ensure_record(db, dir, name).unwrap().id
    }

    /// 構図のあるJPEG（市松状のブロック。`seed` で模様が変わる）。
    fn jpeg(seed: u8) -> Vec<u8> {
        let (w, h) = (96_u32, 64_u32);
        let raw: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w / 12, i / w / 8);
                (x * 37 + y * 91 + u32::from(seed) * 53) as u8
            })
            .collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode(&raw, w, h, image::ExtendedColorType::L8)
            .unwrap();
        out
    }

    fn thumbs(entries: &[(&str, i64, Vec<u8>)]) -> HashMap<String, (i64, Vec<u8>)> {
        entries.iter().map(|(n, m, j)| ((*n).to_owned(), (*m, j.clone()))).collect()
    }

    fn run(db: &Arc<Mutex<Database>>, dir: &Path, names: &[&str], store: &HashMap<String, (i64, Vec<u8>)>) -> usize {
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        hash_files(db, dir, &names, &|n| store.get(n).cloned(), &|| true)
    }

    #[test]
    fn row_round_trips_and_rejects_corrupt_bytes() {
        for hash in [Some(0xDEAD_BEEF_0123_4567), Some(0), None] {
            let row = HashRow { version: 1, thumb_mtime: -42, hash };
            assert_eq!(HashRow::decode(&row.encode()), Some(row));
        }
        assert_eq!(HashRow::decode(&[0; 17]), None, "length");
        let mut bad = HashRow { version: 1, thumb_mtime: 1, hash: Some(1) }.encode();
        bad[17] = 7;
        assert_eq!(HashRow::decode(&bad), None, "flag");
    }

    #[test]
    fn hashes_a_file_once_and_skips_it_while_the_thumbnail_is_unchanged() {
        let t = TempRoot::new("once");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        let store = thumbs(&[("a.zip", 100, jpeg(1))]);
        assert_eq!(run(&db, &t.0, &["a.zip"], &store), 1);
        let row = get(&db, id).unwrap();
        assert_eq!((row.version, row.thumb_mtime), (thumb_hash::HASH_VERSION, 100));
        assert!(row.hash.is_some());
        assert_eq!(run(&db, &t.0, &["a.zip"], &store), 0, "同じサムネは計算し直さない");
    }

    #[test]
    fn recomputes_when_the_thumbnail_was_regenerated() {
        let t = TempRoot::new("regen");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        run(&db, &t.0, &["a.zip"], &thumbs(&[("a.zip", 100, jpeg(1))]));
        let before = get(&db, id).unwrap().hash;
        assert_eq!(run(&db, &t.0, &["a.zip"], &thumbs(&[("a.zip", 200, jpeg(9))])), 1);
        let after = get(&db, id).unwrap();
        assert_eq!(after.thumb_mtime, 200);
        assert_ne!(after.hash, before, "別の絵のサムネなら別のハッシュ");
    }

    #[test]
    fn recomputes_rows_of_an_older_hash_version() {
        let t = TempRoot::new("version");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        assert!(put_all(&db, &[(id, HashRow { version: thumb_hash::HASH_VERSION + 1, thumb_mtime: 100, hash: Some(5) })]));
        assert_eq!(run(&db, &t.0, &["a.zip"], &thumbs(&[("a.zip", 100, jpeg(1))])), 1);
        assert_eq!(get(&db, id).unwrap().version, thumb_hash::HASH_VERSION);
    }

    #[test]
    fn skips_files_without_a_thumbnail_or_an_id() {
        let t = TempRoot::new("skip");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        // サムネ無し → 飛ばす（生成後の走査で拾う）。IDが無い名前 → 飛ばす。
        let store = thumbs(&[("ghost.zip", 100, jpeg(1))]);
        assert_eq!(run(&db, &t.0, &["a.zip", "ghost.zip"], &store), 0);
        assert!(get(&db, id).is_none());
    }

    #[test]
    fn an_unhashable_thumbnail_is_remembered_and_not_retried() {
        let t = TempRoot::new("unhashable");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        let store = thumbs(&[("a.zip", 100, b"not a jpeg".to_vec())]);
        assert_eq!(run(&db, &t.0, &["a.zip"], &store), 1);
        assert_eq!(get(&db, id).unwrap().hash, None);
        assert_eq!(run(&db, &t.0, &["a.zip"], &store), 0);
    }

    #[test]
    fn stops_when_asked_and_writes_what_it_has() {
        let t = TempRoot::new("stop");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        let store = thumbs(&[("a.zip", 100, jpeg(1)), ("b.zip", 100, jpeg(2))]);
        let calls = RefCell::new(0);
        let names = vec!["a.zip".to_owned(), "b.zip".to_owned()];
        let n = hash_files(&db, &t.0, &names, &|n| store.get(n).cloned(), &|| {
            *calls.borrow_mut() += 1;
            *calls.borrow() <= 1
        });
        assert_eq!(n, 1);
        assert!(get(&db, a).is_some() && get(&db, b).is_none());
    }

    #[test]
    fn forgetting_an_id_removes_its_row() {
        let t = TempRoot::new("forget");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        run(&db, &t.0, &["a.zip"], &thumbs(&[("a.zip", 100, jpeg(1))]));
        assert!(get(&db, id).is_some());
        crate::file_identity::delete_id(&db, id).unwrap();
        assert!(get(&db, id).is_none());
    }

    #[test]
    fn reads_and_forget_never_create_the_table_in_a_legacy_db() {
        let t = TempRoot::new("legacy");
        let db = open_spread_db(&t.0).unwrap();
        let id = register(&db, &t.0, "a.zip", 1);
        assert!(get(&db, id).is_none());
        crate::file_identity::delete_id(&db, id).unwrap();
        let guard = db.lock().unwrap();
        let tx = guard.begin_write().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name() == HASH_TABLE.name()));
    }
}
