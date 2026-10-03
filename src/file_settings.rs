// フェーズ3b（タグ・お気に入り所属のID化）が接続するまでは、tags / favorite のスロットは未使用。
#![allow(dead_code)]

//! ファイル単位の保存設定（見開き・ソート・登録サムネ・タグ・お気に入り所属）を、ファイルID
//! （`file_identity`）をキーにした1レコードへ束ねる第2世代の保存（`file_settings_v2`）。
//!
//! 各フィールドは3状態（`Slot`）を持つ。
//! - `Unmigrated`: まだ旧v1（パスキー）から移していない。読むときは旧パスキー（`legacy_keys`）で引く
//! - `Set(値)`: v2が正。旧v1は見ない
//! - `Cleared`: ユーザーが解除した。旧v1が残っていても復活させない
//!
//! 書き込み時に、そのファイルの（このフェーズで扱う）フィールドをまとめて旧v1から移す。
//! 旧v1の行は消さないので、ID化前に作られた設定も、移動・リネーム後に旧パスキー経由で届く。
//! タグ・お気に入り所属のスロットは、フェーズ3bで同じ仕組みに乗せる（それまでは常に `Unmigrated`）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadTransaction, ReadableDatabase, TableDefinition};

use crate::file_identity::{put_str, Reader};
use crate::spread_state::{
    owner_tx, Owner, ARCHIVE_SORT_TABLE_V1, SPREAD_TABLE, THUMBNAIL_SELECTION_TABLE_V1,
    THUMBNAIL_SELECTION_TABLE_V2,
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

fn legacy_group(tx: &ReadTransaction, keys: &[&str]) -> Effective {
    Effective {
        spread: legacy_spread(tx, keys),
        sort: legacy_sort(tx, keys),
        thumb: legacy_thumb(tx, keys),
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
            let legacy = (stored.spread == Slot::Unmigrated
                || stored.sort == Slot::Unmigrated
                || stored.thumb == Slot::Unmigrated)
                .then(|| legacy_group(tx, &keys))
                .unwrap_or_default();
            Effective {
                spread: resolve(stored.spread, legacy.spread),
                sort: resolve(stored.sort, legacy.sort),
                thumb: resolve(stored.thumb, legacy.thumb),
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
        if cur.spread == Slot::Unmigrated || cur.sort == Slot::Unmigrated || cur.thumb == Slot::Unmigrated {
            let legacy = legacy_group(&rtx, &rec.legacy_keys());
            migrate(&mut cur.spread, legacy.spread);
            migrate(&mut cur.sort, legacy.sort);
            migrate(&mut cur.thumb, legacy.thumb);
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

/// 持ち主の設定を読む（読み取りトランザクションを開いて `effective_tx`）。
pub(crate) fn read_effective(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<Effective> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    Some(effective_tx(&tx, &owner_tx(&tx, dir, filename)))
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
}
