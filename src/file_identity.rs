// フェーズ2（解決ワーカー・各テーブルのID化）で接続するまでの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

//! ファイルのID層（フィンガープリント管理）。UIにはまだ接続しない純ロジック。
//!
//! - 同一性は内部ID（`file_id_v1`）。FPは照合キーで、同一内容のコピーはFPが同じでもIDは別
//! - 解決順: パス一致→同一ID（FPが違えば内容更新としてFPを更新）／パス不一致→FP一致で、
//!   元のパスが「消えている」IDがあれば移動として引き継ぐ／どれでもなければ新規ID
//! - 現存するID（コピー元）と、オフラインのボリューム上のIDは移動候補から外す。候補が複数なら
//!   mtime一致 > ファイル名一致 > 登録が古い順（ID昇順）で割り当てる
//! - 書き込み中のファイルはFPを取らず後回しにする（`Observed::Unstable`）。FP更新時に、
//!   ユーザーデータが空のIDは同FPの孤児IDに統合される（別FS移動の途中を拾った場合の救済）
//! - テーブルは `nekoviewer_spread.redb` に置く（キャッシュ整理の対象外）。`open_spread_db` では
//!   作らず、最初の書き込みで作る。最初のID作成で FP仕様マーカー（`spread_state::is_identity_spec`）を立てる
//!
//! DBロックは「読み取り→（ロック外で）ファイルの存在確認→書き込み」の3段階で短く握る。
//! 遅いファイルシステムへの `stat` 中に `spread_db` を塞がないため。書き込み時に読み取り時点の
//! 前提が崩れていれば `Conflict` で最初からやり直す。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableTable,
    TableDefinition, TableError,
};
use sha2::{Digest, Sha256};

use crate::spread_state::{IDENTITY_ENABLED_KEY, IDENTITY_META_TABLE};

/// ID → レコード（`FileRecord::encode` のバイト列）。
const FILE_ID_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("file_id_v1");
/// パスキー（`spread_state::make_key` 形式の "dir\0filename"）→ ID。
const FILE_PATH_INDEX_TABLE: TableDefinition<&str, u64> = TableDefinition::new("file_path_index_v1");
/// FP（16バイト）→ ID（同一内容のコピーは複数のIDを持つ）。
const FILE_FP_INDEX_TABLE: MultimapTableDefinition<&[u8], u64> =
    MultimapTableDefinition::new("file_fp_index_v1");
/// 採番カウンタ。値は次に払い出すID。
pub(crate) const IDENTITY_COUNTER_TABLE: TableDefinition<&str, u64> = TableDefinition::new("identity_counters_v1");
const NEXT_ID_KEY: &str = "next_id";

/// 全体ハッシュにするサイズの上限。これ以下はファイル全体をハッシュする。
pub const FP_WHOLE_MAX: u64 = 256 * 1024;
/// サンプル窓1つの大きさ。
const FP_WINDOW: u64 = 32 * 1024;
/// mtimeがこの秒数以内のファイルは書き込み中の疑いがあるとして後回しにする。
pub const SETTLE_SECS: i64 = 3;
/// 直近の旧パスを持つ件数（誤採用の巻き戻しと旧パス探索の足がかり）。
const HISTORY_MAX: usize = 3;
const RECORD_VERSION: u8 = 1;
const MAX_RESOLVE_ATTEMPTS: usize = 3;

pub type Fp = [u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Confirmed = 0,
    /// 前回の確認でファイルが見つからなかった（削除はしない。復活すれば Confirmed に戻る）。
    Unconfirmed = 1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRecord {
    pub id: u64,
    /// 現在のパスキー。
    pub path_key: String,
    /// ID作成時のパスキー。旧v1テーブル（パスキー）を引く橋渡しなので以後変えない。
    pub origin_key: String,
    pub size: u64,
    /// 更新日時（unix秒）。
    pub mtime: i64,
    /// 0バイトのファイルは None（パスのみで識別する）。
    pub fp: Option<Fp>,
    /// ファイルが載っているボリュームのマウントルート（オフライン判定用）。
    pub volume: String,
    pub status: FileStatus,
    pub last_seen: i64,
    /// 直近の旧パスキー（新しい順、最大 `HISTORY_MAX` 件）。
    pub history: Vec<String>,
}

impl FileRecord {
    fn encode(&self) -> Vec<u8> {
        let mut b = vec![RECORD_VERSION];
        b.extend(self.id.to_le_bytes());
        b.extend(self.size.to_le_bytes());
        b.extend(self.mtime.to_le_bytes());
        b.push(self.status as u8);
        b.extend(self.last_seen.to_le_bytes());
        match self.fp {
            Some(fp) => {
                b.push(1);
                b.extend(fp);
            }
            None => b.push(0),
        }
        put_str(&mut b, &self.path_key);
        put_str(&mut b, &self.origin_key);
        put_str(&mut b, &self.volume);
        b.push(self.history.len().min(HISTORY_MAX) as u8);
        for h in self.history.iter().take(HISTORY_MAX) {
            put_str(&mut b, h);
        }
        b
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { b: bytes, pos: 0 };
        if r.u8()? != RECORD_VERSION {
            return None;
        }
        let id = r.u64()?;
        let size = r.u64()?;
        let mtime = r.i64()?;
        let status = match r.u8()? {
            0 => FileStatus::Confirmed,
            _ => FileStatus::Unconfirmed,
        };
        let last_seen = r.i64()?;
        let fp = if r.u8()? == 1 { Some(r.take(16)?.try_into().ok()?) } else { None };
        let path_key = r.string()?;
        let origin_key = r.string()?;
        let volume = r.string()?;
        let n = r.u8()? as usize;
        let mut history = Vec::with_capacity(n);
        for _ in 0..n {
            history.push(r.string()?);
        }
        Some(Self { id, path_key, origin_key, size, mtime, fp, volume, status, last_seen, history })
    }

    fn file_name(&self) -> &str {
        file_name_of(&self.path_key)
    }
}

pub(crate) fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend((s.len() as u32).to_le_bytes());
    b.extend(s.as_bytes());
}

pub(crate) struct Reader<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub(crate) fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub(crate) fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// "dir\0filename" のファイル名部分。
fn file_name_of(path_key: &str) -> &str {
    path_key.rsplit('\0').next().unwrap_or(path_key)
}

/// "dir\0filename" を実パスへ戻す。
pub(crate) fn path_of_key(path_key: &str) -> PathBuf {
    match path_key.split_once('\0') {
        Some((dir, name)) => Path::new(dir).join(name),
        None => PathBuf::from(path_key),
    }
}

// ---------------------------------------------------------------------------
// FP算出・観測（ファイルを読む側。DBには触れない）
// ---------------------------------------------------------------------------

/// FPの計算に読む範囲（オフセット, 長さ）。`FP_WHOLE_MAX` 以下は全体、それ以上は
/// 先頭・25%・50%・75%・末尾の各 `FP_WINDOW` バイト。ZIPの中央ディレクトリは末尾窓に入る。
fn fp_windows(len: u64) -> Vec<(u64, u64)> {
    if len <= FP_WHOLE_MAX {
        return vec![(0, len)];
    }
    let mid = |num: u64, den: u64| (len / den * num).saturating_sub(FP_WINDOW / 2).min(len - FP_WINDOW);
    vec![
        (0, FP_WINDOW),
        (mid(1, 4), FP_WINDOW),
        (mid(1, 2), FP_WINDOW),
        (mid(3, 4), FP_WINDOW),
        (len - FP_WINDOW, FP_WINDOW),
    ]
}

/// ファイルのFPを計算する。0バイトは None。open→read→close で完結し、ハンドルは保持しない。
pub fn compute_fp(path: &Path) -> std::io::Result<Option<Fp>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"nekoviewer-fp1");
    hasher.update(len.to_le_bytes());
    let mut buf = Vec::new();
    for (offset, n) in fp_windows(len) {
        buf.resize(n as usize, 0);
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(Some(hasher.finalize()[..16].try_into().expect("16 bytes")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub size: u64,
    pub mtime: i64,
    pub volume: String,
    pub fp: Option<Fp>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Observed {
    Ready(Observation),
    /// 書き込み中の疑い（mtimeが新しい、または読む前後でサイズ/mtimeが変わった）。FPを使わず後回しにする。
    Unstable,
    /// ファイルが存在しない（NotFound、またはファイルでない）。
    Gone,
    /// 権限などで読めない。存在しないとは限らないので、IDの状態は変えない。
    Unreadable(String),
}

fn mtime_secs(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn classify_io(e: std::io::Error) -> Observed {
    if e.kind() == std::io::ErrorKind::NotFound {
        Observed::Gone
    } else {
        Observed::Unreadable(e.to_string())
    }
}

/// パスを観測して、サイズ・mtime・ボリューム・FPを返す。`now_unix` は書き込み中判定の基準時刻。
pub fn observe(path: &Path, now_unix: i64) -> Observed {
    let m1 = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Observed::Gone,
        Err(e) => return classify_io(e),
    };
    let mtime = mtime_secs(&m1);
    // mtimeが未来（時計ずれ）の場合は待っても落ち着かないので対象にしない。
    if (0..SETTLE_SECS).contains(&(now_unix - mtime)) {
        return Observed::Unstable;
    }
    let fp = match compute_fp(path) {
        Ok(fp) => fp,
        Err(e) => return classify_io(e),
    };
    match std::fs::metadata(path) {
        Ok(m2) if m2.len() == m1.len() && mtime_secs(&m2) == mtime => {}
        Ok(_) => return Observed::Unstable,
        Err(e) => return classify_io(e),
    }
    Observed::Ready(Observation { size: m1.len(), mtime, volume: mount_root(path), fp })
}

/// ファイルが載っているボリュームのマウントルート。unixは、親をたどってデバイス番号が
/// 変わる手前のディレクトリ（=マウントポイント）。デバイス番号そのものは再マウントで変わりうるので
/// 保存せず、パスだけを持つ。
#[cfg(unix)]
pub fn mount_root(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let full = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Some(mut cur) = full.parent().map(Path::to_path_buf) else {
        return full.to_string_lossy().into_owned();
    };
    let Ok(dev) = std::fs::metadata(&cur).map(|m| m.dev()) else {
        return cur.to_string_lossy().into_owned();
    };
    while let Some(up) = cur.parent() {
        match std::fs::metadata(up) {
            Ok(m) if m.dev() == dev => cur = up.to_path_buf(),
            _ => break,
        }
    }
    cur.to_string_lossy().into_owned()
}

/// マウントルートが今マウントされているか。unixは「親とデバイス番号が違う」で判定する
/// （アンマウント中のマウントポイントは親と同じデバイスの空ディレクトリに見える）。
#[cfg(unix)]
pub fn volume_online(root: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let p = Path::new(root);
    let Some(parent) = p.parent() else { return true }; // "/" は常にオンライン
    match (std::fs::metadata(p), std::fs::metadata(parent)) {
        (Ok(a), Ok(b)) => a.dev() != b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
pub fn mount_root(path: &Path) -> String {
    use std::path::Component;
    let mut root = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(c.as_os_str()),
            _ => break,
        }
    }
    root.to_string_lossy().into_owned()
}

#[cfg(not(unix))]
pub fn volume_online(root: &str) -> bool {
    Path::new(root).exists()
}

// ---------------------------------------------------------------------------
// 解決ロジック
// ---------------------------------------------------------------------------

/// 移動候補のレコードの、いまの状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// 元のパスにまだファイルがある（=コピー元。移動候補にしない）。
    Present,
    /// ボリュームは見えているのにファイルが無い（=移動・削除された。移動候補）。
    Missing,
    /// ボリュームがオフライン、または確認できない（移動候補にしない）。
    Offline,
}

/// 実ファイルでの存在確認。
pub fn probe_record(rec: &FileRecord) -> Presence {
    match std::fs::metadata(path_of_key(&rec.path_key)) {
        Ok(m) if m.is_file() => Presence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if volume_online(&rec.volume) {
                Presence::Missing
            } else {
                Presence::Offline
            }
        }
        // ディレクトリに変わった等も「消えた」扱い。権限エラーなど確認できないものは候補にしない。
        Ok(_) => Presence::Missing,
        Err(_) => Presence::Offline,
    }
}

pub struct ResolveEnv<'a> {
    pub now: i64,
    /// 移動候補の存在確認（DBロックの外で呼ばれる）。
    pub probe: &'a dyn Fn(&FileRecord) -> Presence,
    /// そのIDにユーザーデータ（評価・タグ・しおり等）があるか。ID化済みの各テーブルを見る。
    pub has_user_data: &'a dyn Fn(u64) -> bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// パス一致。`content_updated` はFPが変わった（再パック等）。IDは同じ。
    Existing { id: u64, content_updated: bool },
    /// FP一致の移動として、消えているIDを新パスへ引き継いだ。
    Moved { id: u64, from: String },
    /// 内容更新で、空のIDが同FPの孤児IDへ統合された。`dropped` のレコードは消える。
    Merged { kept: u64, dropped: u64 },
    Created { id: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdentityError {
    Db,
    /// 読み取り後に前提が変わった（内部で再試行する）。
    Conflict,
}

fn db_err<E>(_: E) -> IdentityError {
    IdentityError::Db
}

#[derive(Default)]
struct Snapshot {
    path_rec: Option<FileRecord>,
    /// 観測FPと同じFPを持つ他のID。
    cands: Vec<FileRecord>,
}

enum Decision {
    Update { new: FileRecord, content_updated: bool, adopt: Option<FileRecord> },
    Move { orphan: FileRecord },
    Create,
}

fn open_ro<T>(r: Result<T, TableError>) -> Result<Option<T>, IdentityError> {
    match r {
        Ok(t) => Ok(Some(t)),
        Err(TableError::TableDoesNotExist(_)) => Ok(None),
        Err(_) => Err(IdentityError::Db),
    }
}

fn read_snapshot(
    db: &Arc<Mutex<Database>>,
    path_key: &str,
    fp: Option<Fp>,
) -> Result<Snapshot, IdentityError> {
    let db = db.lock().map_err(db_err)?;
    let tx = db.begin_read().map_err(db_err)?;
    let (Some(ids), Some(paths)) = (
        open_ro(tx.open_table(FILE_ID_TABLE))?,
        open_ro(tx.open_table(FILE_PATH_INDEX_TABLE))?,
    ) else {
        return Ok(Snapshot::default());
    };
    let get = |id: u64| -> Result<Option<FileRecord>, IdentityError> {
        Ok(ids.get(id).map_err(db_err)?.and_then(|g| FileRecord::decode(g.value())))
    };
    let path_rec = match paths.get(path_key).map_err(db_err)? {
        Some(g) => get(g.value())?,
        None => None,
    };
    let mut cands = Vec::new();
    if let (Some(fp), Some(fps)) = (fp, open_ro(tx.open_multimap_table(FILE_FP_INDEX_TABLE))?) {
        for v in fps.get(fp.as_slice()).map_err(db_err)? {
            let id = v.map_err(db_err)?.value();
            if path_rec.as_ref().is_some_and(|r| r.id == id) {
                continue;
            }
            if let Some(rec) = get(id)? {
                if rec.path_key != path_key {
                    cands.push(rec);
                }
            }
        }
    }
    Ok(Snapshot { path_rec, cands })
}

fn record_from_obs(path_key: &str, obs: &Observation, now: i64) -> FileRecord {
    FileRecord {
        id: 0,
        path_key: path_key.to_owned(),
        origin_key: path_key.to_owned(),
        size: obs.size,
        mtime: obs.mtime,
        fp: obs.fp,
        volume: obs.volume.clone(),
        status: FileStatus::Confirmed,
        last_seen: now,
        history: Vec::new(),
    }
}

/// 消えている候補から1つ選ぶ。mtime一致（移動はmtimeを保つ）> ファイル名一致 > 登録が古い順。
fn pick_orphan(
    cands: &[FileRecord],
    path_key: &str,
    obs: &Observation,
    probe: &dyn Fn(&FileRecord) -> Presence,
) -> Option<FileRecord> {
    let name = file_name_of(path_key);
    cands
        .iter()
        .filter(|c| probe(c) == Presence::Missing)
        .min_by_key(|c| (c.mtime != obs.mtime, c.file_name() != name, c.id))
        .cloned()
}

fn decide(snapshot: Snapshot, path_key: &str, obs: &Observation, env: &ResolveEnv) -> Decision {
    match snapshot.path_rec {
        Some(rec) => {
            // サイズがあるのにFPが無い観測は「取れなかった」（書き込み中）。既存のFPは残し、内容更新とも見なさない。
            let fp_unknown = obs.fp.is_none() && obs.size > 0;
            let new_fp = if fp_unknown { rec.fp } else { obs.fp };
            let content_updated = !fp_unknown && rec.fp.is_some() && rec.fp != obs.fp;
            let mut new = rec.clone();
            new.size = obs.size;
            new.mtime = obs.mtime;
            new.fp = new_fp;
            new.volume = obs.volume.clone();
            new.status = FileStatus::Confirmed;
            new.last_seen = env.now;
            // 内容が変わり、かつユーザーデータが空のIDは、同FPの孤児IDに統合する。
            let adopt = if content_updated && obs.fp.is_some() && !(env.has_user_data)(rec.id) {
                pick_orphan(&snapshot.cands, path_key, obs, env.probe)
            } else {
                None
            };
            Decision::Update { new, content_updated, adopt }
        }
        None => match pick_orphan(&snapshot.cands, path_key, obs, env.probe) {
            Some(orphan) if obs.fp.is_some() => Decision::Move { orphan },
            _ => Decision::Create,
        },
    }
}

fn get_rec(
    ids: &redb::Table<'_, u64, &'static [u8]>,
    id: u64,
) -> Result<Option<FileRecord>, IdentityError> {
    Ok(ids.get(id).map_err(db_err)?.and_then(|g| FileRecord::decode(g.value())))
}

struct WriteTables<'tx> {
    ids: redb::Table<'tx, u64, &'static [u8]>,
    paths: redb::Table<'tx, &'static str, u64>,
    fps: redb::MultimapTable<'tx, &'static [u8], u64>,
}

impl WriteTables<'_> {
    fn put(&mut self, rec: &FileRecord) -> Result<(), IdentityError> {
        self.ids.insert(rec.id, rec.encode().as_slice()).map_err(db_err)?;
        Ok(())
    }

    fn fp_index_replace(&mut self, id: u64, old: Option<Fp>, new: Option<Fp>) -> Result<(), IdentityError> {
        if old == new {
            return Ok(());
        }
        if let Some(o) = old {
            self.fps.remove(o.as_slice(), id).map_err(db_err)?;
        }
        if let Some(n) = new {
            self.fps.insert(n.as_slice(), id).map_err(db_err)?;
        }
        Ok(())
    }

    /// `rec`（移動元の記録）を `new_path` へ移し、観測値で更新する。旧パスは履歴へ。
    fn relocate(
        &mut self,
        mut rec: FileRecord,
        new_path: &str,
        obs: &Observation,
        now: i64,
    ) -> Result<FileRecord, IdentityError> {
        let old_path = std::mem::replace(&mut rec.path_key, new_path.to_owned());
        let old_fp = rec.fp;
        self.paths.remove(old_path.as_str()).map_err(db_err)?;
        rec.history.retain(|h| *h != old_path && h.as_str() != new_path);
        rec.history.insert(0, old_path);
        rec.history.truncate(HISTORY_MAX);
        rec.size = obs.size;
        rec.mtime = obs.mtime;
        rec.fp = obs.fp;
        rec.volume = obs.volume.clone();
        rec.status = FileStatus::Confirmed;
        rec.last_seen = now;
        self.paths.insert(new_path, rec.id).map_err(db_err)?;
        self.fp_index_replace(rec.id, old_fp, rec.fp)?;
        self.put(&rec)?;
        Ok(rec)
    }
}

fn apply(
    db: &Arc<Mutex<Database>>,
    decision: Decision,
    path_key: &str,
    obs: &Observation,
    now: i64,
) -> Result<Resolution, IdentityError> {
    let db = db.lock().map_err(db_err)?;
    let tx = db.begin_write().map_err(db_err)?;
    let resolution = {
        let mut t = WriteTables {
            ids: tx.open_table(FILE_ID_TABLE).map_err(db_err)?,
            paths: tx.open_table(FILE_PATH_INDEX_TABLE).map_err(db_err)?,
            fps: tx.open_multimap_table(FILE_FP_INDEX_TABLE).map_err(db_err)?,
        };
        // 読み取り時点の前提（このパスにはまだ誰も居ない／居る）を確認する。
        let path_owner = t.paths.get(path_key).map_err(db_err)?.map(|g| g.value());
        match decision {
            Decision::Update { new, content_updated, adopt } => {
                if path_owner != Some(new.id) {
                    return Err(IdentityError::Conflict);
                }
                let cur = get_rec(&t.ids, new.id)?.ok_or(IdentityError::Conflict)?;
                match adopt {
                    None => {
                        t.fp_index_replace(new.id, cur.fp, new.fp)?;
                        t.put(&new)?;
                        Resolution::Existing { id: new.id, content_updated }
                    }
                    Some(orphan) => {
                        let orphan_cur = get_rec(&t.ids, orphan.id)?.ok_or(IdentityError::Conflict)?;
                        if orphan_cur.path_key != orphan.path_key {
                            return Err(IdentityError::Conflict);
                        }
                        // 空のIDを消して、孤児IDをこのパスへ移す。
                        if let Some(f) = cur.fp {
                            t.fps.remove(f.as_slice(), cur.id).map_err(db_err)?;
                        }
                        t.ids.remove(cur.id).map_err(db_err)?;
                        let kept = t.relocate(orphan_cur, path_key, obs, now)?;
                        Resolution::Merged { kept: kept.id, dropped: cur.id }
                    }
                }
            }
            Decision::Move { orphan } => {
                if path_owner.is_some() {
                    return Err(IdentityError::Conflict);
                }
                let cur = get_rec(&t.ids, orphan.id)?.ok_or(IdentityError::Conflict)?;
                if cur.path_key != orphan.path_key {
                    return Err(IdentityError::Conflict);
                }
                let from = cur.path_key.clone();
                let moved = t.relocate(cur, path_key, obs, now)?;
                Resolution::Moved { id: moved.id, from }
            }
            Decision::Create => {
                if path_owner.is_some() {
                    return Err(IdentityError::Conflict);
                }
                let mut counter = tx.open_table(IDENTITY_COUNTER_TABLE).map_err(db_err)?;
                let id = {
                    let next = counter.get(NEXT_ID_KEY).map_err(db_err)?.map(|g| g.value());
                    next.unwrap_or(1)
                };
                counter.insert(NEXT_ID_KEY, id + 1).map_err(db_err)?;
                let mut rec = record_from_obs(path_key, obs, now);
                rec.id = id;
                t.paths.insert(path_key, id).map_err(db_err)?;
                t.fp_index_replace(id, None, rec.fp)?;
                t.put(&rec)?;
                // 最初のID作成で、このDBをFP仕様（旧パス仕様へ戻す基準にならないもの）にする。
                let mut meta = tx.open_table(IDENTITY_META_TABLE).map_err(db_err)?;
                meta.insert(IDENTITY_ENABLED_KEY, 1).map_err(db_err)?;
                Resolution::Created { id }
            }
        }
    };
    tx.commit().map_err(db_err)?;
    Ok(resolution)
}

/// パスキーを観測値でIDへ解決する。読み取り→（ロック外で）存在確認→書き込みの3段階で、
/// 競合したら最初からやり直す。ファイル観測（`observe`）は呼び出し側で済ませておく。
pub fn resolve_path(
    db: &Arc<Mutex<Database>>,
    path_key: &str,
    obs: &Observation,
    env: &ResolveEnv,
) -> Result<Resolution, IdentityError> {
    for _ in 0..MAX_RESOLVE_ATTEMPTS {
        let snapshot = read_snapshot(db, path_key, obs.fp)?;
        let decision = decide(snapshot, path_key, obs, env);
        match apply(db, decision, path_key, obs, env.now) {
            Err(IdentityError::Conflict) => continue,
            other => return other,
        }
    }
    Err(IdentityError::Conflict)
}

/// 読み取りトランザクション上でパスキーからレコードを引く。ID層が未使用なら None。
pub fn lookup_tx(tx: &redb::ReadTransaction, path_key: &str) -> Option<FileRecord> {
    let ids = tx.open_table(FILE_ID_TABLE).ok()?;
    let paths = tx.open_table(FILE_PATH_INDEX_TABLE).ok()?;
    let id = paths.get(path_key).ok()??.value();
    FileRecord::decode(ids.get(id).ok()??.value())
}

/// IDからレコードを引く。無ければ（日数による削除などで消えた場合を含め）None。
pub fn record_by_id_tx(tx: &redb::ReadTransaction, id: u64) -> Option<FileRecord> {
    let ids = tx.open_table(FILE_ID_TABLE).ok()?;
    FileRecord::decode(ids.get(id).ok()??.value())
}

/// ロック取得済みの `Database` からパスキーを引く（`Mutex` は再入不可のため、ロック中はこちらを使う）。
pub fn lookup_in(db: &Database, path_key: &str) -> Option<FileRecord> {
    lookup_tx(&db.begin_read().ok()?, path_key)
}

/// パスキーからレコードを引く（高速経路）。ID層が未使用なら None。
pub fn lookup(db: &Arc<Mutex<Database>>, path_key: &str) -> Option<FileRecord> {
    let guard = db.lock().ok()?;
    lookup_in(&guard, path_key)
}

/// `prefix`（"dir\0"）で始まるパスキーのレコードを全て返す。ディレクトリ単位の一括ロード用。
pub fn dir_records_tx(tx: &redb::ReadTransaction, prefix: &str) -> Vec<FileRecord> {
    let (Ok(ids), Ok(paths)) = (tx.open_table(FILE_ID_TABLE), tx.open_table(FILE_PATH_INDEX_TABLE)) else {
        return Vec::new();
    };
    let Ok(range) = paths.range(prefix..) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in range {
        let Ok((k, v)) = entry else { continue };
        if !k.value().starts_with(prefix) {
            break;
        }
        if let Some(rec) = ids.get(v.value()).ok().flatten().and_then(|g| FileRecord::decode(g.value())) {
            out.push(rec);
        }
    }
    out
}

/// 全てのIDレコードを返す（お気に入りの横断一覧など、全件を見る用途。通常の表示経路では使わない）。
pub fn all_records_tx(tx: &redb::ReadTransaction) -> Vec<FileRecord> {
    let Ok(ids) = tx.open_table(FILE_ID_TABLE) else { return Vec::new() };
    let Ok(iter) = ids.iter() else { return Vec::new() };
    iter.flatten().filter_map(|(_, v)| FileRecord::decode(v.value())).collect()
}

impl FileRecord {
    /// 旧v1テーブル（パスキー）を引く候補のキー。現在のパス、ID作成時のパス、旧パス履歴の順（重複なし）。
    /// v1行は削除しない約束なので、移動後や遅延移行前でも、これで旧データへ届く。
    pub fn legacy_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = Vec::with_capacity(2 + self.history.len());
        for k in [self.path_key.as_str(), self.origin_key.as_str()]
            .into_iter()
            .chain(self.history.iter().map(String::as_str))
        {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys
    }
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// statがレコードと一致した（何も読まず、何も書かなかった）。
    Unchanged,
    Resolved(Resolution),
    /// 書き込み中の疑いで後回しにした（`record_unstable_without_fp` が false のとき）。
    Unstable,
    /// ファイルが存在しない（またはファイルでない）。
    Gone,
    /// 権限・DBエラー等。IDの状態は変えていない。
    Failed(String),
}

/// 1ファイルを解決する（stat一致なら何もしない。不一致なら観測→解決）。ワーカーと同期解決の共通部。
/// `key` は `spread_state::make_key` 形式。`record_unstable_without_fp` が true なら、書き込み中でも
/// FPなしで記録する（ユーザー操作の同期解決用）。false なら `Unstable` を返して後回しにする。
pub fn resolve_file(
    db: &Arc<Mutex<Database>>,
    key: &str,
    path: &Path,
    now: i64,
    record_unstable_without_fp: bool,
) -> ResolveOutcome {
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return ResolveOutcome::Gone,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ResolveOutcome::Gone,
        Err(e) => return ResolveOutcome::Failed(e.to_string()),
    };
    let (size, mtime) = (meta.len(), mtime_secs(&meta));
    if lookup(db, key).is_some_and(|r| quick_hit(&r, size, mtime)) {
        return ResolveOutcome::Unchanged;
    }
    let obs = match observe(path, now) {
        Observed::Ready(o) => o,
        Observed::Unstable if record_unstable_without_fp => {
            Observation { size, mtime, volume: mount_root(path), fp: None }
        }
        Observed::Unstable => return ResolveOutcome::Unstable,
        Observed::Gone => return ResolveOutcome::Gone,
        Observed::Unreadable(e) => return ResolveOutcome::Failed(e),
    };
    let has_user_data = |id: u64| crate::spread_state::id_has_user_data(db, id);
    let env = ResolveEnv { now, probe: &probe_record, has_user_data: &has_user_data };
    match resolve_path(db, key, &obs, &env) {
        Ok(r) => ResolveOutcome::Resolved(r),
        Err(e) => ResolveOutcome::Failed(format!("{e:?}")),
    }
}

/// 同期でIDを解決してレコードを返す。ユーザー操作（評価の書き込み・ビューアーを開く等）で
/// 未解決のファイルに対して使う。stat一致なら何も読まず、不一致なら観測→解決する。
/// ファイルが無い／読めない場合は、既存のレコードがあればそれ、無ければ None。
/// 書き込み中でFPが取れなければ、FPなしで記録する（後で埋める）。
pub fn ensure_record(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<FileRecord> {
    let key = crate::spread_state::make_key(dir, filename);
    resolve_file(db, &key, &dir.join(filename), now_unix(), true);
    lookup(db, &key)
}

/// stat（サイズ・mtime）だけで同一と見なせるか。FPを読まずに済ませる判定。
/// FP未取得（書き込み中に記録した等）のレコードは、FPを埋めるため一致扱いにしない（0バイトは除く）。
pub fn quick_hit(rec: &FileRecord, size: u64, mtime: i64) -> bool {
    rec.status == FileStatus::Confirmed
        && rec.size == size
        && rec.mtime == mtime
        && (rec.fp.is_some() || size == 0)
}

/// ファイルが見つからなかったIDを「未確認」にする（削除はしない）。成功したら true。
pub fn mark_unconfirmed(db: &Arc<Mutex<Database>>, path_key: &str) -> bool {
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    {
        let (Ok(mut ids), Ok(paths)) = (tx.open_table(FILE_ID_TABLE), tx.open_table(FILE_PATH_INDEX_TABLE))
        else {
            return false;
        };
        let Some(id) = paths.get(path_key).ok().flatten().map(|g| g.value()) else { return false };
        let Some(mut rec) = get_rec(&ids, id).ok().flatten() else { return false };
        rec.status = FileStatus::Unconfirmed;
        if ids.insert(id, rec.encode().as_slice()).is_err() {
            return false;
        }
    }
    tx.commit().is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use redb::TableHandle;

    use super::*;
    use crate::spread_state::{is_identity_spec, open_spread_db};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_file_identity_test_{}_{}_{}",
                std::process::id(),
                nonce,
                tag
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 存在確認とユーザーデータの有無を、テストから差し込む。
    #[derive(Default)]
    struct Fake {
        presence: HashMap<u64, Presence>,
        user_data: HashSet<u64>,
    }

    impl Fake {
        fn set(&mut self, id: u64, p: Presence) {
            self.presence.insert(id, p);
        }
    }

    const NOW: i64 = 1_000_000;

    fn obs(size: u64, mtime: i64, fp: Option<u8>) -> Observation {
        Observation { size, mtime, volume: "/".to_owned(), fp: fp.map(|b| [b; 16]) }
    }

    fn resolve(db: &Arc<Mutex<Database>>, key: &str, o: &Observation, fake: &Fake) -> Resolution {
        let probe = |r: &FileRecord| fake.presence.get(&r.id).copied().unwrap_or(Presence::Missing);
        let has = |id: u64| fake.user_data.contains(&id);
        let env = ResolveEnv { now: NOW, probe: &probe, has_user_data: &has };
        resolve_path(db, key, o, &env).unwrap()
    }

    fn rec_by_id(db: &Arc<Mutex<Database>>, id: u64) -> Option<FileRecord> {
        let db = db.lock().unwrap();
        let tx = db.begin_read().unwrap();
        let ids = tx.open_table(FILE_ID_TABLE).ok()?;
        FileRecord::decode(ids.get(id).unwrap()?.value())
    }

    fn fp_index_ids(db: &Arc<Mutex<Database>>, fp: u8) -> Vec<u64> {
        let db = db.lock().unwrap();
        let tx = db.begin_read().unwrap();
        let Ok(fps) = tx.open_multimap_table(FILE_FP_INDEX_TABLE) else { return Vec::new() };
        let mut v: Vec<u64> = fps
            .get([fp; 16].as_slice())
            .unwrap()
            .map(|r| r.unwrap().value())
            .collect();
        v.sort();
        v
    }

    fn new_db(t: &TempRoot) -> Arc<Mutex<Database>> {
        open_spread_db(&t.0).unwrap()
    }

    // ---- FP ----

    fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn fp_is_none_for_empty_and_stable_for_same_content() {
        let t = TempRoot::new("fp_basic");
        let a = write_file(&t.0, "a", b"");
        assert_eq!(compute_fp(&a).unwrap(), None);
        let b = write_file(&t.0, "b", &patterned(1000));
        let c = write_file(&t.0, "c", &patterned(1000));
        assert!(compute_fp(&b).unwrap().is_some());
        assert_eq!(compute_fp(&b).unwrap(), compute_fp(&c).unwrap());
    }

    #[test]
    fn small_file_is_fully_hashed() {
        let t = TempRoot::new("fp_small");
        let base = patterned(FP_WHOLE_MAX as usize);
        let mut changed = base.clone();
        changed[base.len() / 2] ^= 1;
        let a = write_file(&t.0, "a", &base);
        let b = write_file(&t.0, "b", &changed);
        assert_ne!(compute_fp(&a).unwrap(), compute_fp(&b).unwrap());
    }

    #[test]
    fn large_file_samples_windows_only() {
        let t = TempRoot::new("fp_large");
        let base = patterned(1_000_000);
        let fp_of = |name: &str, f: &dyn Fn(&mut Vec<u8>)| {
            let mut d = base.clone();
            f(&mut d);
            compute_fp(&write_file(&t.0, name, &d)).unwrap().unwrap()
        };
        let original = fp_of("orig", &|_| {});
        // 窓の外だけが違う同サイズのファイルは同じFPになる（既知の限界。サンプル方式の代償）。
        assert_eq!(original, fp_of("outside", &|d| d[100_000] ^= 1));
        // 先頭・25/50/75%・末尾の窓の中は区別できる。
        assert_ne!(original, fp_of("head", &|d| d[0] ^= 1));
        assert_ne!(original, fp_of("q1", &|d| d[233_616 + 10] ^= 1));
        assert_ne!(original, fp_of("mid", &|d| d[483_616 + 10] ^= 1));
        assert_ne!(original, fp_of("q3", &|d| d[733_616 + 10] ^= 1));
        assert_ne!(original, fp_of("tail", &|d| d[999_999] ^= 1));
        // サイズが違えばFPも違う。
        assert_ne!(original, fp_of("longer", &|d| d.push(0)));
    }

    #[test]
    fn fp_windows_cover_expected_ranges() {
        assert_eq!(fp_windows(100), vec![(0, 100)]);
        assert_eq!(fp_windows(FP_WHOLE_MAX), vec![(0, FP_WHOLE_MAX)]);
        let w = fp_windows(1_000_000);
        assert_eq!(w.len(), 5);
        assert_eq!(w[0], (0, FP_WINDOW));
        assert_eq!(w[4], (1_000_000 - FP_WINDOW, FP_WINDOW));
        // 256KB直後でも窓はファイル内に収まる。
        for (o, n) in fp_windows(FP_WHOLE_MAX + 1) {
            assert!(o + n <= FP_WHOLE_MAX + 1);
        }
    }

    // ---- observe ----

    fn set_mtime(path: &Path, secs_ago: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago))
            .unwrap();
    }

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn observe_defers_fresh_files_and_reports_settled_ones() {
        let t = TempRoot::new("observe");
        let p = write_file(&t.0, "a.zip", &patterned(5000));
        // 書いた直後はmtimeが新しい→後回し。
        assert_eq!(observe(&p, now_unix()), Observed::Unstable);
        set_mtime(&p, 100);
        match observe(&p, now_unix()) {
            Observed::Ready(o) => {
                assert_eq!(o.size, 5000);
                assert!(o.fp.is_some());
                assert!(!o.volume.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // 未来のmtime（時計ずれ）は待っても落ち着かないので後回しにしない。
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(matches!(observe(&p, now_unix()), Observed::Ready(_)));
    }

    #[test]
    fn observe_reports_gone_for_missing_path_and_directory() {
        let t = TempRoot::new("observe_gone");
        assert_eq!(observe(&t.0.join("none.zip"), now_unix()), Observed::Gone);
        assert_eq!(observe(&t.0, now_unix()), Observed::Gone);
    }

    #[test]
    fn observe_zero_byte_file_has_no_fp() {
        let t = TempRoot::new("observe_zero");
        let p = write_file(&t.0, "z", b"");
        set_mtime(&p, 100);
        match observe(&p, now_unix()) {
            Observed::Ready(o) => assert_eq!((o.size, o.fp), (0, None)),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn mount_root_is_an_online_volume() {
        let t = TempRoot::new("mount");
        let p = write_file(&t.0, "a", b"x");
        let root = mount_root(&p);
        assert!(Path::new(&root).is_dir());
        assert!(p.canonicalize().unwrap().starts_with(&root));
        assert!(volume_online(&root));
        assert!(volume_online("/"));
        // 存在しないマウントポイントはオフライン扱い。
        assert!(!volume_online("/nonexistent_nekoviewer_volume/sub"));
    }

    #[test]
    fn probe_record_distinguishes_present_missing_and_offline() {
        let t = TempRoot::new("probe");
        let p = write_file(&t.0, "a.zip", b"x");
        let mut rec = record_from_obs(&format!("{}\0a.zip", t.0.display()), &obs(1, 0, Some(1)), NOW);
        rec.volume = mount_root(&p);
        assert_eq!(probe_record(&rec), Presence::Present);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(probe_record(&rec), Presence::Missing);
        #[cfg(unix)]
        {
            rec.volume = "/nonexistent_nekoviewer_volume/sub".to_owned();
            assert_eq!(probe_record(&rec), Presence::Offline);
        }
    }

    // ---- レコード ----

    #[test]
    fn record_round_trips_and_rejects_corrupt_bytes() {
        let rec = FileRecord {
            id: 42,
            path_key: "/d\0日本語.zip".to_owned(),
            origin_key: "/old\0a.zip".to_owned(),
            size: 123,
            mtime: -5,
            fp: Some([9; 16]),
            volume: "/media/x".to_owned(),
            status: FileStatus::Unconfirmed,
            last_seen: 77,
            history: vec!["/h1\0a".to_owned(), "/h2\0a".to_owned()],
        };
        assert_eq!(FileRecord::decode(&rec.encode()), Some(rec.clone()));
        let none_fp = FileRecord { fp: None, history: vec![], ..rec };
        assert_eq!(FileRecord::decode(&none_fp.encode()), Some(none_fp.clone()));
        let bytes = none_fp.encode();
        assert_eq!(FileRecord::decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(FileRecord::decode(&[]), None);
        let mut wrong_version = bytes;
        wrong_version[0] = 99;
        assert_eq!(FileRecord::decode(&wrong_version), None);
    }

    // ---- 解決 ----

    #[test]
    fn open_spread_db_leaves_legacy_db_without_identity_tables() {
        let t = TempRoot::new("legacy_untouched");
        let db = new_db(&t);
        assert!(lookup(&db, "/d\0a.zip").is_none());
        assert!(!is_identity_spec(&db));
        let guard = db.lock().unwrap();
        let tx = guard.begin_read().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name().ends_with("_v1") && h.name().starts_with("file_")));
    }

    #[test]
    fn first_create_marks_db_as_identity_spec_and_path_hit_is_stable() {
        let t = TempRoot::new("create");
        let db = new_db(&t);
        let fake = Fake::default();
        assert!(!is_identity_spec(&db));
        let o = obs(100, 50, Some(1));
        assert_eq!(resolve(&db, "/d\0a.zip", &o, &fake), Resolution::Created { id: 1 });
        assert!(is_identity_spec(&db));
        let rec = lookup(&db, "/d\0a.zip").unwrap();
        assert_eq!((rec.id, rec.origin_key.as_str(), rec.status), (1, "/d\0a.zip", FileStatus::Confirmed));
        assert!(quick_hit(&rec, 100, 50));
        assert!(!quick_hit(&rec, 101, 50));
        assert!(!quick_hit(&rec, 100, 51));
        assert_eq!(
            resolve(&db, "/d\0a.zip", &o, &fake),
            Resolution::Existing { id: 1, content_updated: false }
        );
        // 別パスの新規はIDが増える。
        assert_eq!(resolve(&db, "/d\0b.zip", &obs(5, 5, Some(2)), &fake), Resolution::Created { id: 2 });
    }

    #[test]
    fn move_keeps_id_and_records_history() {
        let t = TempRoot::new("move");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/d1\0a.zip", &obs(100, 50, Some(1)), &fake);
        fake.set(1, Presence::Missing);
        let r = resolve(&db, "/d2\0b.zip", &obs(100, 50, Some(1)), &fake);
        assert_eq!(r, Resolution::Moved { id: 1, from: "/d1\0a.zip".to_owned() });
        assert!(lookup(&db, "/d1\0a.zip").is_none());
        let rec = lookup(&db, "/d2\0b.zip").unwrap();
        assert_eq!(rec.id, 1);
        assert_eq!(rec.origin_key, "/d1\0a.zip");
        assert_eq!(rec.history, vec!["/d1\0a.zip".to_owned()]);
        // 元のパスにコピーを戻すと、移動先が現存しているので別IDになる。
        fake.set(1, Presence::Present);
        assert_eq!(resolve(&db, "/d1\0a.zip", &obs(100, 50, Some(1)), &fake), Resolution::Created { id: 2 });
    }

    #[test]
    fn move_chain_keeps_only_recent_history_and_original_key() {
        let t = TempRoot::new("chain");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/p\01", &obs(10, 1, Some(1)), &fake);
        for n in 2..=6 {
            fake.set(1, Presence::Missing);
            let r = resolve(&db, &format!("/p\0{n}"), &obs(10, 1, Some(1)), &fake);
            assert!(matches!(r, Resolution::Moved { id: 1, .. }), "{r:?}");
        }
        let rec = lookup(&db, "/p\06").unwrap();
        assert_eq!(rec.origin_key, "/p\01");
        assert_eq!(rec.history, vec!["/p\05".to_owned(), "/p\04".to_owned(), "/p\03".to_owned()]);
    }

    #[test]
    fn copy_of_present_file_gets_its_own_id() {
        let t = TempRoot::new("copy");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/d\0a.zip", &obs(100, 50, Some(1)), &fake);
        fake.set(1, Presence::Present);
        assert_eq!(resolve(&db, "/e\0a.zip", &obs(100, 60, Some(1)), &fake), Resolution::Created { id: 2 });
        // 同一FPのIDが2つ並ぶ。どちらも元のパスで引ける。
        assert_eq!(fp_index_ids(&db, 1), vec![1, 2]);
        assert_eq!(lookup(&db, "/d\0a.zip").unwrap().id, 1);
        assert_eq!(lookup(&db, "/e\0a.zip").unwrap().id, 2);
    }

    #[test]
    fn offline_volume_ids_are_never_adopted() {
        let t = TempRoot::new("offline");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/hdd\0a.zip", &obs(100, 50, Some(1)), &fake);
        fake.set(1, Presence::Offline);
        assert_eq!(resolve(&db, "/local\0a.zip", &obs(100, 50, Some(1)), &fake), Resolution::Created { id: 2 });
        // HDDが戻れば元のパスはそのまま引ける。
        assert_eq!(lookup(&db, "/hdd\0a.zip").unwrap().id, 1);
    }

    fn two_missing_twins(db: &Arc<Mutex<Database>>, fake: &mut Fake, a: (&str, i64), b: (&str, i64)) {
        resolve(db, a.0, &obs(100, a.1, Some(7)), fake);
        fake.set(1, Presence::Present);
        resolve(db, b.0, &obs(100, b.1, Some(7)), fake);
        fake.set(1, Presence::Missing);
        fake.set(2, Presence::Missing);
    }

    #[test]
    fn ambiguous_candidates_prefer_mtime_then_name_then_oldest() {
        // mtime一致が、登録の古さより優先される。
        let t = TempRoot::new("rank_mtime");
        let db = new_db(&t);
        let mut fake = Fake::default();
        two_missing_twins(&db, &mut fake, ("/d\0a.zip", 100), ("/d\0b.zip", 200));
        assert!(matches!(
            resolve(&db, "/n\0c.zip", &obs(100, 200, Some(7)), &fake),
            Resolution::Moved { id: 2, .. }
        ));
        // 残りの1つは次の新パスが拾う。
        assert!(matches!(
            resolve(&db, "/n\0d.zip", &obs(100, 999, Some(7)), &fake),
            Resolution::Moved { id: 1, .. }
        ));

        // mtimeが同じならファイル名一致が優先される。
        let t = TempRoot::new("rank_name");
        let db = new_db(&t);
        let mut fake = Fake::default();
        two_missing_twins(&db, &mut fake, ("/d\0a.zip", 100), ("/d\0b.zip", 100));
        assert!(matches!(
            resolve(&db, "/n\0b.zip", &obs(100, 100, Some(7)), &fake),
            Resolution::Moved { id: 2, .. }
        ));

        // どちらも同じなら登録が古い順。
        let t = TempRoot::new("rank_oldest");
        let db = new_db(&t);
        let mut fake = Fake::default();
        two_missing_twins(&db, &mut fake, ("/d\0a.zip", 100), ("/d\0b.zip", 100));
        assert!(matches!(
            resolve(&db, "/n\0z.zip", &obs(100, 100, Some(7)), &fake),
            Resolution::Moved { id: 1, .. }
        ));
    }

    #[test]
    fn content_update_keeps_id_and_refreshes_fp_index() {
        let t = TempRoot::new("content");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/d\0a.zip", &obs(100, 50, Some(1)), &fake);
        let r = resolve(&db, "/d\0a.zip", &obs(120, 60, Some(2)), &fake);
        assert_eq!(r, Resolution::Existing { id: 1, content_updated: true });
        let rec = lookup(&db, "/d\0a.zip").unwrap();
        assert_eq!((rec.size, rec.mtime, rec.fp), (120, 60, Some([2; 16])));
        assert_eq!(fp_index_ids(&db, 1), Vec::<u64>::new());
        assert_eq!(fp_index_ids(&db, 2), vec![1]);
        // 旧FPのファイルが別の場所に現れても、更新済みのIDは奪われない。
        fake.set(1, Presence::Missing);
        assert_eq!(resolve(&db, "/e\0a.zip", &obs(100, 50, Some(1)), &fake), Resolution::Created { id: 2 });
    }

    #[test]
    fn fp_backfill_on_legacy_record_is_not_a_content_update() {
        let t = TempRoot::new("backfill");
        let db = new_db(&t);
        let fake = Fake::default();
        resolve(&db, "/d\0a.zip", &obs(100, 50, None), &fake); // FP未取得で作られたレコード
        let r = resolve(&db, "/d\0a.zip", &obs(100, 50, Some(3)), &fake);
        assert_eq!(r, Resolution::Existing { id: 1, content_updated: false });
        assert_eq!(fp_index_ids(&db, 3), vec![1]);
    }

    #[test]
    fn blank_id_is_merged_into_orphan_when_fp_changes() {
        // 別FS移動の途中（コピー中）を拾って作られた空のID(2)が、完了後のFPで孤児ID(1)に統合される。
        let t = TempRoot::new("merge");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/old\0a.zip", &obs(100, 50, Some(5)), &fake);
        fake.user_data.insert(1);
        fake.set(1, Presence::Present);
        assert_eq!(resolve(&db, "/new\0a.zip", &obs(60, 60, Some(9)), &fake), Resolution::Created { id: 2 });
        // 移動元の削除が済み、コピー先の内容が完成した。
        fake.set(1, Presence::Missing);
        let r = resolve(&db, "/new\0a.zip", &obs(100, 50, Some(5)), &fake);
        assert_eq!(r, Resolution::Merged { kept: 1, dropped: 2 });
        assert_eq!(lookup(&db, "/new\0a.zip").unwrap().id, 1);
        assert!(lookup(&db, "/old\0a.zip").is_none());
        assert!(rec_by_id(&db, 2).is_none());
        assert_eq!(fp_index_ids(&db, 9), Vec::<u64>::new());
        assert_eq!(fp_index_ids(&db, 5), vec![1]);
    }

    #[test]
    fn id_with_user_data_is_never_merged() {
        let t = TempRoot::new("no_merge");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/old\0a.zip", &obs(100, 50, Some(5)), &fake);
        fake.set(1, Presence::Present);
        resolve(&db, "/new\0a.zip", &obs(60, 60, Some(9)), &fake);
        fake.user_data.insert(2); // 新パス側のIDに評価が付いた
        fake.set(1, Presence::Missing);
        let r = resolve(&db, "/new\0a.zip", &obs(100, 50, Some(5)), &fake);
        assert_eq!(r, Resolution::Existing { id: 2, content_updated: true });
        assert_eq!(lookup(&db, "/old\0a.zip").unwrap().id, 1);
    }

    #[test]
    fn zero_byte_files_are_path_only_and_never_adopt() {
        let t = TempRoot::new("zero");
        let db = new_db(&t);
        let fake = Fake::default();
        assert_eq!(resolve(&db, "/d\0a", &obs(0, 1, None), &fake), Resolution::Created { id: 1 });
        assert_eq!(resolve(&db, "/d\0b", &obs(0, 1, None), &fake), Resolution::Created { id: 2 });
        assert_eq!(lookup(&db, "/d\0a").unwrap().fp, None);
        // 消えているIDがあっても、FPなしの新パスは引き継がない。
        let mut fake = Fake::default();
        resolve(&db, "/d\0big", &obs(100, 1, Some(1)), &fake);
        fake.set(3, Presence::Missing);
        assert_eq!(resolve(&db, "/d\0c", &obs(0, 1, None), &fake), Resolution::Created { id: 4 });
    }

    #[test]
    fn unconfirmed_record_returns_to_confirmed_when_file_reappears() {
        let t = TempRoot::new("unconfirmed");
        let db = new_db(&t);
        let fake = Fake::default();
        let o = obs(100, 50, Some(1));
        resolve(&db, "/d\0a.zip", &o, &fake);
        assert!(mark_unconfirmed(&db, "/d\0a.zip"));
        assert!(!mark_unconfirmed(&db, "/d\0none.zip"));
        let rec = lookup(&db, "/d\0a.zip").unwrap();
        assert_eq!(rec.status, FileStatus::Unconfirmed);
        assert!(!quick_hit(&rec, 100, 50));
        assert_eq!(
            resolve(&db, "/d\0a.zip", &o, &fake),
            Resolution::Existing { id: 1, content_updated: false }
        );
        assert!(quick_hit(&lookup(&db, "/d\0a.zip").unwrap(), 100, 50));
    }

    #[test]
    fn stale_decisions_are_rejected_as_conflicts() {
        let t = TempRoot::new("conflict");
        let db = new_db(&t);
        let mut fake = Fake::default();
        resolve(&db, "/d1\0a.zip", &obs(100, 50, Some(1)), &fake);
        let stale = lookup(&db, "/d1\0a.zip").unwrap();
        // 読み取り後に、別のワーカーがIDを別パスへ動かした。
        fake.set(1, Presence::Missing);
        resolve(&db, "/d2\0a.zip", &obs(100, 50, Some(1)), &fake);
        let o = obs(100, 50, Some(1));
        assert_eq!(
            apply(&db, Decision::Move { orphan: stale }, "/d3\0a.zip", &o, NOW),
            Err(IdentityError::Conflict)
        );
        // 既に誰かが居るパスへの新規作成も競合。
        assert_eq!(apply(&db, Decision::Create, "/d2\0a.zip", &o, NOW), Err(IdentityError::Conflict));
        // 競合で書き込みは起きていない。
        assert_eq!(lookup(&db, "/d2\0a.zip").unwrap().id, 1);
        assert!(lookup(&db, "/d3\0a.zip").is_none());
    }

    #[test]
    fn unknown_fp_observation_keeps_existing_fp_and_is_not_a_content_update() {
        let t = TempRoot::new("fp_unknown");
        let db = new_db(&t);
        let fake = Fake::default();
        resolve(&db, "/d\0a.zip", &obs(100, 50, Some(1)), &fake);
        // 書き込み中でFPが取れなかった観測（サイズはあるのにFPなし）。
        let r = resolve(&db, "/d\0a.zip", &obs(150, 60, None), &fake);
        assert_eq!(r, Resolution::Existing { id: 1, content_updated: false });
        let rec = lookup(&db, "/d\0a.zip").unwrap();
        assert_eq!((rec.size, rec.mtime, rec.fp), (150, 60, Some([1; 16])));
        assert_eq!(fp_index_ids(&db, 1), vec![1]);
    }

    #[test]
    fn legacy_keys_are_current_origin_then_history_without_duplicates() {
        let mut rec = record_from_obs("/a\0x", &obs(1, 1, Some(1)), NOW);
        assert_eq!(rec.legacy_keys(), vec!["/a\0x"]);
        rec.path_key = "/c\0x".to_owned();
        rec.history = vec!["/b\0x".to_owned(), "/a\0x".to_owned()];
        assert_eq!(rec.legacy_keys(), vec!["/c\0x", "/a\0x", "/b\0x"]);
    }

    #[test]
    fn ensure_record_creates_fp_less_record_for_fresh_file_and_fills_fp_later() {
        let t = TempRoot::new("ensure");
        let db = new_db(&t);
        let p = write_file(&t.0, "a.zip", &patterned(5000));
        // 書いた直後（mtimeが新しい）はFPを取らずに記録する。
        let rec = ensure_record(&db, &t.0, "a.zip").unwrap();
        assert_eq!(rec.fp, None);
        assert_eq!(rec.size, 5000);
        // 落ち着いた後に呼び直すとFPが埋まる（IDは同じ）。
        set_mtime(&p, 100);
        let rec2 = ensure_record(&db, &t.0, "a.zip").unwrap();
        assert_eq!(rec2.id, rec.id);
        assert!(rec2.fp.is_some());
        // 以後はstat一致で何も読まない。
        assert!(quick_hit(&rec2, 5000, rec2.mtime));
        // ファイルが無ければ、記録があればそれ・無ければ None。
        assert!(ensure_record(&db, &t.0, "none.zip").is_none());
    }
}
