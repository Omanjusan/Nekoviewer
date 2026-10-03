use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::file_settings::Slot;
use crate::types::{PageMode, ReaderSortKey};

/// キー = "{正規化済みディレクトリ}\0{ファイル名}"
/// 値 = (page_mode: u8, spread_offset: i32)
/// 復帰は常にファイル先頭固定。spread_offset は「先頭から見開きを組んだときの
/// ズレ状態」で、0=先頭仮想なし、-1=先頭仮想あり。絶対ページ位置は保存しない。
/// 旧データの +1 も読込時には -1 と同じ「先頭仮想あり」として扱う。
pub const SPREAD_TABLE: TableDefinition<&str, (u8, i32)> = TableDefinition::new("spread_state");

/// アーカイブ単位のソート条件保存テーブル（第1世代）。
///
/// キーは SPREAD_TABLE と同じ「正規化済みディレクトリ\0ファイル名」。
/// 値は (sort_key: u8, ascending: bool)。レコード不在が保存OFFを表す。
/// 値形式を将来変更する場合はこの定義を変更せず、`archive_sort_state_v2` のような
/// 新しいテーブルを追加して移行すること。
pub const ARCHIVE_SORT_TABLE_V1: TableDefinition<&str, (u8, bool)> =
    TableDefinition::new("archive_sort_state_v1");

/// アーカイブ単位の登録サムネイルページ。値は表示順に依存しないentry_name。
pub const THUMBNAIL_SELECTION_TABLE_V1: TableDefinition<&str, &str> =
    TableDefinition::new("thumbnail_selection_v1");

/// アーカイブ単位の登録サムネイル。v1のentry_nameに生成方法を追加した第2世代。
pub const THUMBNAIL_SELECTION_TABLE_V2: TableDefinition<&str, (&str, u8)> =
    TableDefinition::new("thumbnail_selection_v2");

/// アーカイブ単位のしおり保存テーブル（第1世代）。
///
/// キーは他テーブルと同じ「正規化済みディレクトリ\0ファイル名」。
/// 値は (bookmark_enabled, last_entry_name, updated_at, archive_mtime)。
/// last_entry_name はページ番号ではなくファイル内の実エントリ名（ソート順に非依存）。
/// レコード不在 or bookmark_enabled=false は「しおり保存OFF」を表す。
/// 値形式を将来変更する場合はこの定義を変更せず、`bookmark_state_v2` のような
/// 新しいテーブルを追加して移行すること（thumbnail_selection方式を踏襲）。
pub const BOOKMARK_TABLE_V1: TableDefinition<&str, (bool, &str, i64, i64)> =
    TableDefinition::new("bookmark_state_v1");

/// アーカイブ単位の評価・訪問記録テーブル（第1世代）。
///
/// キーは他テーブルと同じ「正規化済みディレクトリ\0ファイル名」。
/// 値は (rating_half, visit_count, last_visit_at)。
/// rating_half は半星単位（0=未評価 / 1..=10=★0.5〜★5.0）、last_visit_at は unix秒。
/// レコード不在は「一度も開いていない（NEW）」を表し、評価と訪問回数は独立した項目
/// （訪問だけがあって未評価のレコードが正常な状態）。
/// 値形式を将来変更する場合はこの定義を変更せず、`archive_rating_v2` のような
/// 新しいテーブルを追加して移行すること（thumbnail_selection方式を踏襲）。
pub const ARCHIVE_RATING_TABLE_V1: TableDefinition<&str, (u8, u32, i64)> =
    TableDefinition::new("archive_rating_v1");

/// アーカイブ単位のタグ紐付けテーブル（第1世代）。
///
/// キーは他テーブルと同じ「正規化済みディレクトリ\0ファイル名」。
/// 値はタグマネージャーのtier_id（u64、リネームで変わらない不変ID）の集合を、
/// 8バイトLEで連結したバイト列として持つ（`encode_tag_ids`/`decode_tag_ids`）。
/// メイン/属性の区別はDB側では持たず、読込側が現在のカテゴリ定義と突き合わせて
/// 振り分ける。tier削除で孤立したidの掃除は、ファイルを開いて読み込むたびに
/// 現存tier_idだけへフィルタし直して書き戻す自己修復方式（一括GCは行わない）。
/// 値形式を将来変更する場合はこの定義を変更せず、`archive_tags_v2` のような
/// 新しいテーブルを追加して移行すること（thumbnail_selection方式を踏襲）。
pub const ARCHIVE_TAGS_TABLE_V1: TableDefinition<&str, &[u8]> =
    TableDefinition::new("archive_tags_v1");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailSourceKind {
    Full,
    LeftHalf,
    RightHalf,
}

impl ThumbnailSourceKind {
    fn as_u8(self) -> u8 {
        match self {
            Self::Full => 0,
            Self::LeftHalf => 1,
            Self::RightHalf => 2,
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Full),
            1 => Some(Self::LeftHalf),
            2 => Some(Self::RightHalf),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailSelection {
    pub entry_name: String,
    pub source_kind: ThumbnailSourceKind,
}

/// アーカイブ単位のしおり保存状態。
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkState {
    pub enabled: bool,
    pub last_entry_name: String,
    pub updated_at: i64,
    pub archive_mtime: i64,
    /// 保存時のファイルのFP。内容が変わっていないかの判定に使う（旧v1から移した行・FP未取得は None で、
    /// その場合は `archive_mtime` で判定する）。
    pub archive_fp: Option<crate::file_identity::Fp>,
}

/// アーカイブ単位の評価・訪問記録。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ArchiveRating {
    /// 半星単位（0=未評価 / 1..=10）
    pub rating_half: u8,
    /// 開いた回数
    pub visit_count: u32,
    /// 最終訪問日時（unix秒。0=記録なし）
    pub last_visit_at: i64,
}

/// サムネイル上の保存設定表示に必要な、アーカイブ単位の状態。
#[derive(Clone, Copy, PartialEq, Default)]
pub struct SavedArchiveSettings {
    pub spread_mode: Option<PageMode>,
    pub has_saved_sort: bool,
    pub has_custom_thumbnail: bool,
    pub has_bookmark: bool,
}

/// root（config.rsが解決したconf置き場所）の nekoviewer_spread.redb を開く。
/// 失敗時は None（保存機能自体を無効化）。
pub fn open_spread_db(root: &Path) -> Option<Arc<Mutex<Database>>> {
    let _ = std::fs::create_dir_all(root);
    let db_path = root.join("nekoviewer_spread.redb");
    let db = Database::create(&db_path).ok()?;
    {
        let tx = db.begin_write().ok()?;
        tx.open_table(SPREAD_TABLE).ok()?;
        tx.open_table(ARCHIVE_SORT_TABLE_V1).ok()?;
        tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok()?;
        tx.open_table(THUMBNAIL_SELECTION_TABLE_V2).ok()?;
        tx.open_table(BOOKMARK_TABLE_V1).ok()?;
        tx.open_table(ARCHIVE_RATING_TABLE_V1).ok()?;
        tx.open_table(ARCHIVE_TAGS_TABLE_V1).ok()?;
        tx.commit().ok()?;
    }
    Some(Arc::new(Mutex::new(db)))
}

/// ID層（フィンガープリント仕様）の使用開始マーカーを置くメタテーブル。
///
/// `open_spread_db` では意図的に作らない（旧パス仕様DBの構造を起動だけで変えないため）。
/// テーブル不在・キー不在はどちらも「パス仕様」を表す。ID層が最初にIDを作る時と、
/// 開発用ツールのテスト用切替だけが `set_identity_spec(.., true)` で立てる。
pub(crate) const IDENTITY_META_TABLE: TableDefinition<&str, u32> = TableDefinition::new("identity_meta_v1");
pub(crate) const IDENTITY_ENABLED_KEY: &str = "identity_enabled";

/// DBがFP仕様（ID層を使い始めたもの）か。テーブル・キーが無ければ false。読み取りのみ。
pub fn is_identity_spec(db: &Arc<Mutex<Database>>) -> bool {
    let Ok(db) = db.lock() else { return false };
    is_identity_spec_in(&db)
}

/// ロック取得済みの `Database` に対する判定。呼び出し側がMutexを保持したまま使う用
/// （`Mutex` は再入不可のため、`is_identity_spec` をロック中に呼ぶとデッドロックする）。
pub fn is_identity_spec_in(db: &Database) -> bool {
    let Ok(tx) = db.begin_read() else { return false };
    let Ok(table) = tx.open_table(IDENTITY_META_TABLE) else { return false };
    matches!(table.get(IDENTITY_ENABLED_KEY), Ok(Some(v)) if v.value() != 0)
}

/// FP仕様マーカーの設定/解除。成功したら true。解除はキーを消すだけでテーブルは残す。
pub fn set_identity_spec(db: &Arc<Mutex<Database>>, enabled: bool) -> bool {
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    {
        let Ok(mut table) = tx.open_table(IDENTITY_META_TABLE) else { return false };
        let ok = if enabled {
            table.insert(IDENTITY_ENABLED_KEY, 1).is_ok()
        } else {
            table.remove(IDENTITY_ENABLED_KEY).is_ok()
        };
        if !ok {
            return false;
        }
    }
    tx.commit().is_ok()
}

/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn write_thumbnail_selection_v1(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    selection: &ThumbnailSelection,
) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2) {
        let _ = table.insert(
            key.as_str(),
            (selection.entry_name.as_str(), selection.source_kind.as_u8()),
        );
    }
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}


/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn remove_thumbnail_selection_v1(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        let _ = table.remove(key.as_str());
    }
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}

// ---- ファイル単位の設定（見開き・ソート・登録サムネ）。IDが解決済みなら file_settings_v2 ----

fn spread_from_raw(v: (u8, i32)) -> Option<(PageMode, i32)> {
    Some((page_mode_from_u8(v.0)?, v.1))
}

fn thumbnail_from_raw(v: (String, u8)) -> Option<ThumbnailSelection> {
    Some(ThumbnailSelection { entry_name: v.0, source_kind: ThumbnailSourceKind::from_u8(v.1)? })
}

fn dir_prefix(dir: &Path) -> String {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("{}\0", key.to_string_lossy())
}

pub fn write_thumbnail_selection(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    selection: &ThumbnailSelection,
) {
    let raw = (selection.entry_name.clone(), selection.source_kind.as_u8());
    if crate::file_settings::modify(db, dir, filename, |s| s.thumb = Slot::Set(raw)).is_none() {
        write_thumbnail_selection_v1(db, dir, filename, selection);
    }
}

pub fn read_thumbnail_selection(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<ThumbnailSelection> {
    crate::file_settings::read_effective(db, dir, filename)?.thumb.and_then(thumbnail_from_raw)
}

pub fn remove_thumbnail_selection(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    if crate::file_settings::modify(db, dir, filename, |s| s.thumb = Slot::Cleared).is_none() {
        remove_thumbnail_selection_v1(db, dir, filename);
    }
}

/// アーカイブのソート条件を保存する（上書き）。戻り値は書き込み成否（一括変更のトースト集計用）。
pub fn write_archive_sort(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    key: ReaderSortKey,
    ascending: bool,
) -> bool {
    let raw = (reader_sort_key_to_u8(key), ascending);
    crate::file_settings::modify(db, dir, filename, |s| s.sort = Slot::Set(raw))
        .unwrap_or_else(|| write_archive_sort_v1(db, dir, filename, key, ascending))
}

/// アーカイブの保存済みソート条件を返す。レコード不在・未知値は None。
pub fn read_archive_sort(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<(ReaderSortKey, bool)> {
    let (key_raw, ascending) = crate::file_settings::read_effective(db, dir, filename)?.sort?;
    Some((reader_sort_key_from_u8(key_raw)?, ascending))
}

/// アーカイブのソート条件保存を解除する。
pub fn remove_archive_sort(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    if crate::file_settings::modify(db, dir, filename, |s| s.sort = Slot::Cleared).is_none() {
        remove_archive_sort_v1(db, dir, filename);
    }
}

/// dir 配下の有効なソート保存値を列挙する。
pub fn list_dir_archive_sorts(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
) -> Vec<(String, ReaderSortKey, bool)> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    crate::file_settings::dir_effective_tx(&tx, &dir_prefix(dir))
        .into_iter()
        .filter_map(|(name, e)| {
            let (key_raw, ascending) = e.sort?;
            Some((name, reader_sort_key_from_u8(key_raw)?, ascending))
        })
        .collect()
}

/// 見開き状態を保存する（上書き）。戻り値は書き込み成否（一括変更のトースト集計用）。
pub fn write_spread(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, mode: PageMode, offset: i32) -> bool {
    let raw = (page_mode_to_u8(mode), offset);
    crate::file_settings::modify(db, dir, filename, |s| s.spread = Slot::Set(raw))
        .unwrap_or_else(|| write_spread_v1(db, dir, filename, mode, offset))
}

/// 保存済みの見開き状態を返す。レコード不在・未知値は None。
pub fn read_spread(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<(PageMode, i32)> {
    crate::file_settings::read_effective(db, dir, filename)?.spread.and_then(spread_from_raw)
}

/// 見開き状態を削除する（保存解除）。
pub fn remove_spread(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    if crate::file_settings::modify(db, dir, filename, |s| s.spread = Slot::Cleared).is_none() {
        remove_spread_v1(db, dir, filename);
    }
}

/// dir 配下で保存済みのファイル名一覧を返す（入場時ロード用）。
/// 戻り値: (filename, page_mode, spread_offset)
pub fn list_dir_entries(db: &Arc<Mutex<Database>>, dir: &Path) -> Vec<(String, PageMode, i32)> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    crate::file_settings::dir_effective_tx(&tx, &dir_prefix(dir))
        .into_iter()
        .filter_map(|(name, e)| {
            let (mode, offset) = spread_from_raw(e.spread?)?;
            Some((name, mode, offset))
        })
        .collect()
}

pub(crate) fn make_key(dir: &Path, filename: &str) -> String {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("{}\0{}", key.to_string_lossy(), filename)
}

/// 複数ディレクトリを横断する一覧向けに、3種類の保存設定を一括取得する。
/// いずれの設定もないパスは戻り値へ含めない。
pub fn saved_settings_for_paths(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
) -> HashMap<PathBuf, SavedArchiveSettings> {
    let Ok(db) = db.lock() else {
        return HashMap::new();
    };
    let Ok(tx) = db.begin_read() else {
        return HashMap::new();
    };
    let mut out = HashMap::new();

    for path in paths {
        let Some(dir) = path.parent() else { continue };
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let owner = owner_tx(&tx, dir, filename);
        let effective = crate::file_settings::effective_tx(&tx, &owner);
        let spread_mode = effective.spread.and_then(|v| page_mode_from_u8(v.0));
        let has_saved_sort = effective.sort.is_some_and(|v| reader_sort_key_from_u8(v.0).is_some());
        let has_custom_thumbnail = effective.thumb.is_some();
        let has_bookmark = bookmark_get_tx(&tx, &owner).is_some_and(|b| b.enabled);
        let settings = SavedArchiveSettings {
            spread_mode,
            has_saved_sort,
            has_custom_thumbnail,
            has_bookmark,
        };
        if settings != SavedArchiveSettings::default() {
            out.insert(path.clone(), settings);
        }
    }
    out
}

pub fn page_mode_to_u8(mode: PageMode) -> u8 {
    match mode {
        PageMode::Single => 0,
        PageMode::SpreadLeft => 1,
        PageMode::SpreadRight => 2,
    }
}

pub fn page_mode_from_u8(v: u8) -> Option<PageMode> {
    match v {
        1 => Some(PageMode::SpreadLeft),
        2 => Some(PageMode::SpreadRight),
        _ => None,
    }
}

/// archive_sort_state_v1 に保存する値。割り当てはリリース後に変更しないこと。
pub fn reader_sort_key_to_u8(key: ReaderSortKey) -> u8 {
    match key {
        ReaderSortKey::Name => 0,
        ReaderSortKey::Natural => 1,
        ReaderSortKey::Date => 2,
    }
}

/// 未知の値は、将来世代や破損データを既定値と誤認しないよう未保存扱いにする。
pub fn reader_sort_key_from_u8(v: u8) -> Option<ReaderSortKey> {
    match v {
        0 => Some(ReaderSortKey::Name),
        1 => Some(ReaderSortKey::Natural),
        2 => Some(ReaderSortKey::Date),
        _ => None,
    }
}

/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn write_archive_sort_v1(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    key: ReaderSortKey,
    ascending: bool,
) -> bool {
    let db_key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    let inserted = match tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        Ok(mut table) => table.insert(db_key.as_str(), (reader_sort_key_to_u8(key), ascending)).is_ok(),
        Err(_) => false,
    };
    tx.commit().is_ok() && inserted
}


/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn remove_archive_sort_v1(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let db_key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        let _ = table.remove(db_key.as_str());
    }
    let _ = tx.commit();
}



/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn write_spread_v1(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, mode: PageMode, offset: i32) -> bool {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    let inserted = match tx.open_table(SPREAD_TABLE) {
        Ok(mut table) => table.insert(key.as_str(), (page_mode_to_u8(mode), offset)).is_ok(),
        Err(_) => false,
    };
    tx.commit().is_ok() && inserted
}


/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn remove_spread_v1(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(SPREAD_TABLE) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}



fn unix_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// アーカイブ単位のしおり保存テーブル（第2世代、キー=ファイルID）。
/// 値は (bookmark_enabled, last_entry_name, updated_at, archive_mtime, archive_fp)。
/// archive_fp は16バイト（未取得は空）。旧v1（パスキー）からは、書き込み時に遅延移行する。
pub const BOOKMARK_TABLE_V2: TableDefinition<u64, (bool, &str, i64, i64, &[u8])> =
    TableDefinition::new("bookmark_state_v2");

/// アーカイブ単位の評価・訪問記録テーブル（第2世代、キー=ファイルID）。値形式はv1と同じ。
pub const ARCHIVE_RATING_TABLE_V2: TableDefinition<u64, (u8, u32, i64)> =
    TableDefinition::new("archive_rating_v2");

/// データの持ち主。IDが解決済みならID（v2テーブル）、未解決ならパスキー（旧v1テーブル）。
/// v1行は削除しないので、IDが解決済みでもv2に無ければ旧パスキー（`legacy_keys`）で引き直せる。
pub(crate) enum Owner {
    Id(crate::file_identity::FileRecord),
    Path(String),
}

impl Owner {
    fn legacy_keys(&self) -> Vec<&str> {
        match self {
            Self::Id(rec) => rec.legacy_keys(),
            Self::Path(key) => vec![key.as_str()],
        }
    }
}

pub(crate) fn owner_tx(tx: &redb::ReadTransaction, dir: &Path, filename: &str) -> Owner {
    let key = make_key(dir, filename);
    match crate::file_identity::lookup_tx(tx, &key) {
        Some(rec) => Owner::Id(rec),
        None => Owner::Path(key),
    }
}

/// そのIDにユーザーデータ（評価・訪問・しおり）があるか。ID層の「空のID」判定に使う。
pub fn id_has_user_data(db: &Arc<Mutex<Database>>, id: u64) -> bool {
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_read() else { return false };
    let has_rating = tx
        .open_table(ARCHIVE_RATING_TABLE_V2)
        .ok()
        .is_some_and(|t| t.get(id).ok().flatten().is_some());
    has_rating
        || tx
            .open_table(BOOKMARK_TABLE_V2)
            .ok()
            .is_some_and(|t| t.get(id).ok().flatten().is_some())
        || crate::file_settings::id_has_settings(&db, id)
}

fn decode_bookmark(value: (bool, &str, i64, i64)) -> BookmarkState {
    BookmarkState {
        enabled: value.0,
        last_entry_name: value.1.to_string(),
        updated_at: value.2,
        archive_mtime: value.3,
        archive_fp: None,
    }
}

fn decode_bookmark_v2(value: (bool, &str, i64, i64, &[u8])) -> BookmarkState {
    BookmarkState {
        enabled: value.0,
        last_entry_name: value.1.to_string(),
        updated_at: value.2,
        archive_mtime: value.3,
        archive_fp: value.4.try_into().ok(),
    }
}

fn bookmark_get_tx(tx: &redb::ReadTransaction, owner: &Owner) -> Option<BookmarkState> {
    if let Owner::Id(rec) = owner {
        if let Ok(t) = tx.open_table(BOOKMARK_TABLE_V2) {
            if let Some(v) = t.get(rec.id).ok().flatten() {
                return Some(decode_bookmark_v2(v.value()));
            }
        }
    }
    let t = tx.open_table(BOOKMARK_TABLE_V1).ok()?;
    owner
        .legacy_keys()
        .into_iter()
        .find_map(|k| t.get(k).ok().flatten().map(|v| decode_bookmark(v.value())))
}

enum BookmarkChange {
    /// 何も書かない（これ自体は失敗ではない）。
    Keep,
    Set(BookmarkState),
    Remove,
}

/// しおり行を読み→変更→書く。IDを同期で解決でき、v2に行が無ければ旧v1の行を読んで引き継ぐ。
/// IDを解決できない（ファイルが無い等）場合は、従来どおりパスキーのv1行を直接読み書きする。
/// `f` には現在のしおり状態と、ファイルの現在のFP（不明なら None）を渡す。
fn bookmark_modify(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    f: impl FnOnce(Option<BookmarkState>, Option<crate::file_identity::Fp>) -> BookmarkChange,
) -> bool {
    let rec = crate::file_identity::ensure_record(db, dir, filename);
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let ok = match &rec {
        Some(rec) => {
            let Ok(mut v2) = tx.open_table(BOOKMARK_TABLE_V2) else { return false };
            let Ok(mut v1) = tx.open_table(BOOKMARK_TABLE_V1) else { return false };
            let current = match v2.get(rec.id).ok().flatten().map(|v| decode_bookmark_v2(v.value())) {
                Some(s) => Some(s),
                None => rec
                    .legacy_keys()
                    .into_iter()
                    .find_map(|k| v1.get(k).ok().flatten().map(|v| decode_bookmark(v.value()))),
            };
            match f(current, rec.fp) {
                BookmarkChange::Keep => true,
                BookmarkChange::Set(s) => v2
                    .insert(
                        rec.id,
                        (
                            s.enabled,
                            s.last_entry_name.as_str(),
                            s.updated_at,
                            s.archive_mtime,
                            s.archive_fp.as_ref().map_or(&[][..], |fp| fp.as_slice()),
                        ),
                    )
                    .is_ok(),
                BookmarkChange::Remove => {
                    // v1の行も消す（残すと、v2に無いことを理由に旧しおりが復活してしまう）。
                    let removed = v2.remove(rec.id).is_ok();
                    for k in rec.legacy_keys() {
                        let _ = v1.remove(k);
                    }
                    removed
                }
            }
        }
        None => {
            let key = make_key(dir, filename);
            let Ok(mut v1) = tx.open_table(BOOKMARK_TABLE_V1) else { return false };
            let current = v1.get(key.as_str()).ok().flatten().map(|v| decode_bookmark(v.value()));
            match f(current, None) {
                BookmarkChange::Keep => true,
                BookmarkChange::Set(s) => v1
                    .insert(
                        key.as_str(),
                        (s.enabled, s.last_entry_name.as_str(), s.updated_at, s.archive_mtime),
                    )
                    .is_ok(),
                BookmarkChange::Remove => v1.remove(key.as_str()).is_ok(),
            }
        }
    };
    tx.commit().is_ok() && ok
}

/// しおり保存の有効/無効を切り替える（右クリックメニューのトグル用）。
/// 既存の位置情報（last_entry_name等）は変更しない。レコード不在時、
/// enabled=trueなら空の位置情報でレコードを新規作成する（次の離脱時保存を待つ状態）。
/// enabled=falseでレコード不在なら何もしない（この場合も戻り値はtrue。何もしないこと自体は
/// 失敗ではないため。一括変更のトースト集計用）。
pub fn write_bookmark_enabled(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, enabled: bool) -> bool {
    bookmark_modify(db, dir, filename, |current, _| match current {
        Some(state) => BookmarkChange::Set(BookmarkState { enabled, ..state }),
        None if enabled => BookmarkChange::Set(BookmarkState {
            enabled: true,
            last_entry_name: String::new(),
            updated_at: 0,
            archive_mtime: 0,
            archive_fp: None,
        }),
        None => BookmarkChange::Keep,
    })
}

/// 離脱時に現在の閲覧位置を保存する。bookmark_enabled=trueのレコードが既に
/// 存在する場合のみ書き込む（呼び出し元がenabled状態を見て呼ぶ前提の保険）。
/// 保存時のファイルのFPも一緒に記録し、復帰時に内容が変わっていないかの判定に使う。
pub fn write_bookmark_position(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    last_entry_name: &str,
    archive_mtime: i64,
) {
    bookmark_modify(db, dir, filename, |current, fp| match current.filter(|state| state.enabled) {
        Some(state) => BookmarkChange::Set(BookmarkState {
            last_entry_name: last_entry_name.to_owned(),
            updated_at: unix_timestamp_secs(),
            archive_mtime,
            archive_fp: fp,
            ..state
        }),
        None => BookmarkChange::Keep,
    });
}

/// 保存済みのしおり状態を返す。レコード不在は None。
pub fn read_bookmark(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<BookmarkState> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let owner = owner_tx(&tx, dir, filename);
    bookmark_get_tx(&tx, &owner)
}

/// 復帰失敗時、位置情報だけ初期化する（enabledは維持し、次回離脱時に再記録させる）。
pub fn clear_bookmark_position(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    bookmark_modify(db, dir, filename, |current, _| match current {
        Some(state) => BookmarkChange::Set(BookmarkState {
            last_entry_name: String::new(),
            updated_at: 0,
            archive_mtime: 0,
            archive_fp: None,
            ..state
        }),
        None => BookmarkChange::Keep,
    });
}

/// しおりレコードを完全に削除する（しおり保存OFF）。IDが解決済みなら旧v1の行も消す。
pub fn remove_bookmark(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    bookmark_modify(db, dir, filename, |_, _| BookmarkChange::Remove);
}

/// 保存済みのしおりが、いまのファイルに対して有効か。FPが両方分かれば内容の一致で、
/// どちらかが不明（旧v1から移した行・FP未取得）なら更新日時の一致で判定する。
pub fn bookmark_matches_file(
    bookmark: &BookmarkState,
    file_fp: Option<crate::file_identity::Fp>,
    file_mtime: i64,
) -> bool {
    match (bookmark.archive_fp, file_fp) {
        (Some(saved), Some(now)) => saved == now,
        _ => bookmark.archive_mtime == file_mtime,
    }
}

/// 評価の半星値の上限（★5.0）。これを超える値は上限へ丸める。
pub const RATING_HALF_MAX: u8 = 10;

fn decode_rating(value: (u8, u32, i64)) -> ArchiveRating {
    ArchiveRating {
        rating_half: value.0.min(RATING_HALF_MAX),
        visit_count: value.1,
        last_visit_at: value.2,
    }
}

/// パスにID記録も旧v1データ（評価・しおり）も無いものを返す。ID解決の完了までサムネに
/// 「ファイル検証中」を出す対象（新規または移動・リネーム直後のファイル）。1回の読み取りで調べる。
pub fn paths_without_identity_or_legacy(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    let Ok(db) = db.lock() else { return out };
    let Ok(tx) = db.begin_read() else { return out };
    let rating_v1 = tx.open_table(ARCHIVE_RATING_TABLE_V1).ok();
    let bookmark_v1 = tx.open_table(BOOKMARK_TABLE_V1).ok();
    let spread_v1 = tx.open_table(SPREAD_TABLE).ok();
    let sort_v1 = tx.open_table(ARCHIVE_SORT_TABLE_V1).ok();
    let thumb_v1 = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok();
    let thumb_v2 = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2).ok();
    let tags_v1 = tx.open_table(ARCHIVE_TAGS_TABLE_V1).ok();
    let favorite_v1 = tx.open_table(crate::favorites::FAVORITE_MEMBERSHIP_TABLE).ok();
    let mut prefixes: HashMap<PathBuf, String> = HashMap::new();
    for path in paths {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            continue;
        };
        let prefix = prefixes.entry(dir.to_path_buf()).or_insert_with(|| {
            let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            format!("{}\0", canon.to_string_lossy())
        });
        let key = format!("{prefix}{name}");
        let has_legacy = rating_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || bookmark_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || spread_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || sort_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || thumb_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || thumb_v2.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || tags_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some())
            || favorite_v1.as_ref().is_some_and(|t| t.get(key.as_str()).ok().flatten().is_some());
        if !has_legacy && crate::file_identity::lookup_tx(&tx, &key).is_none() {
            out.insert(path.clone());
        }
    }
    out
}

/// 旧v1にユーザーデータ（評価・有効なしおり・見開き・ソート・登録サムネ・タグ・お気に入り）があるのに、まだID記録が無いパスキーの一覧。
/// アイドル時のバックフィル（FP化）の対象。未訪問フォルダのファイルも移動追従の対象にするため。
pub fn legacy_data_keys_without_id(db: &Arc<Mutex<Database>>) -> Vec<String> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(t) = tx.open_table(ARCHIVE_RATING_TABLE_V1) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(BOOKMARK_TABLE_V1) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().filter(|(_, v)| v.value().0).map(|(k, _)| k.value().to_string()));
        }
    }
    // 見開き・ソート・登録サムネの旧v1行も、ユーザーが明示的に保存したデータなので対象にする。
    if let Ok(t) = tx.open_table(SPREAD_TABLE) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V2) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(ARCHIVE_TAGS_TABLE_V1) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    if let Ok(t) = tx.open_table(crate::favorites::FAVORITE_MEMBERSHIP_TABLE) {
        if let Ok(iter) = t.iter() {
            keys.extend(iter.flatten().map(|(k, _)| k.value().to_string()));
        }
    }
    keys.retain(|k| crate::file_identity::lookup_tx(&tx, k).is_none());
    keys.into_iter().collect()
}

fn rating_get_tx(tx: &redb::ReadTransaction, owner: &Owner) -> Option<ArchiveRating> {
    if let Owner::Id(rec) = owner {
        if let Ok(t) = tx.open_table(ARCHIVE_RATING_TABLE_V2) {
            if let Some(v) = t.get(rec.id).ok().flatten() {
                return Some(decode_rating(v.value()));
            }
        }
    }
    let t = tx.open_table(ARCHIVE_RATING_TABLE_V1).ok()?;
    owner
        .legacy_keys()
        .into_iter()
        .find_map(|k| t.get(k).ok().flatten().map(|v| decode_rating(v.value())))
}

/// 評価行を読み→変更→書く。IDを同期で解決でき、v2に行が無ければ旧v1の行を読んで引き継ぐ。
/// IDを解決できない（ファイルが無い等）場合は、従来どおりパスキーのv1行を直接読み書きする。
fn rating_modify(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    f: impl FnOnce(Option<ArchiveRating>) -> ArchiveRating,
) -> bool {
    let rec = crate::file_identity::ensure_record(db, dir, filename);
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let ok = match &rec {
        Some(rec) => {
            let Ok(mut v2) = tx.open_table(ARCHIVE_RATING_TABLE_V2) else { return false };
            let Ok(v1) = tx.open_table(ARCHIVE_RATING_TABLE_V1) else { return false };
            let current = match v2.get(rec.id).ok().flatten().map(|v| decode_rating(v.value())) {
                Some(r) => Some(r),
                None => rec
                    .legacy_keys()
                    .into_iter()
                    .find_map(|k| v1.get(k).ok().flatten().map(|v| decode_rating(v.value()))),
            };
            let next = f(current);
            v2.insert(rec.id, (next.rating_half, next.visit_count, next.last_visit_at)).is_ok()
        }
        None => {
            let key = make_key(dir, filename);
            let Ok(mut v1) = tx.open_table(ARCHIVE_RATING_TABLE_V1) else { return false };
            let current = v1.get(key.as_str()).ok().flatten().map(|v| decode_rating(v.value()));
            let next = f(current);
            v1.insert(key.as_str(), (next.rating_half, next.visit_count, next.last_visit_at)).is_ok()
        }
    };
    tx.commit().is_ok() && ok
}

/// アーカイブを開いたことを記録する（訪問回数+1・最終訪問日時を更新）。
/// レコード不在なら未評価・1回目で新規作成し、既存の評価は変更しない。
pub fn record_archive_visit(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> bool {
    rating_modify(db, dir, filename, |current| match current {
        Some(r) => ArchiveRating {
            visit_count: r.visit_count.saturating_add(1),
            last_visit_at: unix_timestamp_secs(),
            ..r
        },
        None => ArchiveRating { rating_half: 0, visit_count: 1, last_visit_at: unix_timestamp_secs() },
    })
}

/// 評価を絶対値で書き込む（0=未評価に戻す）。何度呼んでも同じ結果になる（冪等）。
/// 上限超過は★5.0へ丸める。訪問回数・最終訪問日時は変更せず、レコード不在なら
/// 訪問0回で新規作成する。
pub fn write_archive_rating(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, rating_half: u8) -> bool {
    let rating_half = rating_half.min(RATING_HALF_MAX);
    rating_modify(db, dir, filename, |current| ArchiveRating {
        rating_half,
        ..current.unwrap_or_default()
    })
}

/// 評価・訪問記録を返す。レコード不在（一度も開いていない）は None。
pub fn read_archive_rating(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<ArchiveRating> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let owner = owner_tx(&tx, dir, filename);
    rating_get_tx(&tx, &owner)
}

/// dir 配下の評価・訪問記録を一括で返す（サムネ帯・フィルタ用）。戻り値: (filename, ArchiveRating)
/// IDが解決済みのファイルはID経由（v2、無ければ旧v1）、未解決のファイルは旧v1のパスキーで引く。
pub fn list_dir_archive_ratings(db: &Arc<Mutex<Database>>, dir: &Path) -> Vec<(String, ArchiveRating)> {
    let prefix = {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        format!("{}\0", key.to_string_lossy())
    };
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let mut out: HashMap<String, ArchiveRating> = HashMap::new();
    if let Ok(table) = tx.open_table(ARCHIVE_RATING_TABLE_V1) {
        if let Ok(range) = table.range(prefix.as_str()..) {
            for entry in range {
                let Ok((k, v)) = entry else { continue };
                let full_key = k.value();
                if !full_key.starts_with(&prefix) {
                    break;
                }
                out.insert(full_key[prefix.len()..].to_string(), decode_rating(v.value()));
            }
        }
    }
    for rec in crate::file_identity::dir_records_tx(&tx, &prefix) {
        let name = rec.path_key[prefix.len()..].to_string();
        match rating_get_tx(&tx, &Owner::Id(rec)) {
            Some(r) => {
                out.insert(name, r);
            }
            None => {
                out.remove(&name);
            }
        }
    }
    out.into_iter().collect()
}

/// tier_idの集合を8バイトLEで連結したバイト列へ符号化する。
fn encode_tag_ids(ids: &[u64]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(ids.len() * 8);
    for id in ids {
        buf.extend_from_slice(&id.to_le_bytes());
    }
    buf
}

/// `encode_tag_ids`の逆変換。バイト長が8の倍数でない壊れたレコードは空扱いにする。
pub(crate) fn decode_tag_ids(bytes: &[u8]) -> Vec<u64> {
    bytes
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// ファイルのタグ紐付け（tier_idの集合）を丸ごと置き換える。空集合なら
/// レコード自体を削除する（未タグ付けと「空集合を明示保存」を区別しない）。
/// IDを解決できない（ファイルが無い等）場合は、従来どおり旧v1のパスキーへ書く。
pub fn write_archive_tags(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, tier_ids: &[u64]) -> bool {
    let slot = if tier_ids.is_empty() { Slot::Cleared } else { Slot::Set(tier_ids.to_vec()) };
    crate::file_settings::modify(db, dir, filename, |s| s.tags = slot)
        .unwrap_or_else(|| write_archive_tags_v1(db, dir, filename, tier_ids))
}

/// 旧v1（パスキー）への直接書き込み。IDを解決できないファイル（実体が無い等）用。
fn write_archive_tags_v1(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, tier_ids: &[u64]) -> bool {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    let ok = {
        let Ok(mut table) = tx.open_table(ARCHIVE_TAGS_TABLE_V1) else { return false };
        if tier_ids.is_empty() {
            table.remove(key.as_str()).is_ok()
        } else {
            let encoded = encode_tag_ids(tier_ids);
            table.insert(key.as_str(), encoded.as_slice()).is_ok()
        }
    };
    tx.commit().is_ok() && ok
}

/// ファイルのタグ紐付け（tier_idの集合）を返す。レコード不在は空Vec。
pub fn read_archive_tags(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Vec<u64> {
    crate::file_settings::read_effective(db, dir, filename)
        .and_then(|e| e.tags)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_temp_path(suffix: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "nekoviewer_sort_state_test_{}_{}_{}",
            std::process::id(),
            nonce,
            suffix
        ))
    }

    fn temp_db() -> Arc<Mutex<Database>> {
        let path = unique_temp_path("db.redb");
        let db = Database::create(path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            tx.open_table(SPREAD_TABLE).unwrap();
            tx.open_table(ARCHIVE_SORT_TABLE_V1).unwrap();
            tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).unwrap();
            tx.open_table(BOOKMARK_TABLE_V1).unwrap();
            tx.open_table(ARCHIVE_RATING_TABLE_V1).unwrap();
            tx.open_table(ARCHIVE_TAGS_TABLE_V1).unwrap();
            tx.commit().unwrap();
        }
        Arc::new(Mutex::new(db))
    }

    fn dummy_dir() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn archive_sort_key_encoding_is_stable() {
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Name), 0);
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Natural), 1);
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Date), 2);
        assert!(matches!(
            reader_sort_key_from_u8(0),
            Some(ReaderSortKey::Name)
        ));
        assert!(matches!(
            reader_sort_key_from_u8(1),
            Some(ReaderSortKey::Natural)
        ));
        assert!(matches!(
            reader_sort_key_from_u8(2),
            Some(ReaderSortKey::Date)
        ));
        assert!(reader_sort_key_from_u8(3).is_none());
        assert!(reader_sort_key_from_u8(u8::MAX).is_none());
    }

    #[test]
    fn spread_records_are_read_by_actual_parent_directory() {
        let db = temp_db();
        let dir = unique_temp_path("spread_dir");
        let other_dir = unique_temp_path("other_spread_dir");

        write_spread(&db, &dir, "book.zip", PageMode::SpreadLeft, 1);
        write_spread(&db, &other_dir, "book.zip", PageMode::SpreadRight, -1);

        let first = read_spread(&db, &dir, "book.zip").unwrap();
        assert!(matches!(first.0, PageMode::SpreadLeft));
        assert_eq!(first.1, 1);

        let other = read_spread(&db, &other_dir, "book.zip").unwrap();
        assert!(matches!(other.0, PageMode::SpreadRight));
        assert_eq!(other.1, -1);

        remove_spread(&db, &dir, "book.zip");
        assert!(read_spread(&db, &dir, "book.zip").is_none());
        assert!(read_spread(&db, &other_dir, "book.zip").is_some());
    }

    #[test]
    fn saved_settings_for_paths_combines_flags_and_keeps_full_paths() {
        let db = temp_db();
        let dir_a = unique_temp_path("settings_dir_a");
        let dir_b = unique_temp_path("settings_dir_b");
        let path_a = dir_a.join("same.zip");
        let path_b = dir_b.join("same.zip");
        let plain = dir_a.join("plain.zip");

        write_spread(&db, &dir_a, "same.zip", PageMode::SpreadLeft, 1);
        write_archive_sort(&db, &dir_a, "same.zip", ReaderSortKey::Natural, false);
        write_thumbnail_selection(&db, &dir_a, "same.zip", &ThumbnailSelection {
            entry_name: "003.jpg".to_string(),
            source_kind: ThumbnailSourceKind::Full,
        });
        write_spread(&db, &dir_b, "same.zip", PageMode::SpreadRight, -1);

        let settings =
            saved_settings_for_paths(&db, &[path_a.clone(), path_b.clone(), plain.clone()]);
        let settings_a = settings.get(&path_a).unwrap();
        assert!(matches!(settings_a.spread_mode, Some(PageMode::SpreadLeft)));
        assert!(settings_a.has_saved_sort);
        assert!(settings_a.has_custom_thumbnail);

        let settings_b = settings.get(&path_b).unwrap();
        assert!(matches!(settings_b.spread_mode, Some(PageMode::SpreadRight)));
        assert!(!settings_b.has_saved_sort);
        assert!(!settings_b.has_custom_thumbnail);
        assert!(!settings.contains_key(&plain));
    }

    #[test]
    fn archive_sort_roundtrip_overwrite_and_remove() {
        let db = temp_db();
        let dir = dummy_dir();
        assert!(read_archive_sort(&db, &dir, "a.zip").is_none());

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Natural, false);
        let value = read_archive_sort(&db, &dir, "a.zip").unwrap();
        assert!(matches!(value.0, ReaderSortKey::Natural));
        assert!(!value.1);

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Date, true);
        let value = read_archive_sort(&db, &dir, "a.zip").unwrap();
        assert!(matches!(value.0, ReaderSortKey::Date));
        assert!(value.1);

        remove_archive_sort(&db, &dir, "a.zip");
        assert!(read_archive_sort(&db, &dir, "a.zip").is_none());
    }

    #[test]
    fn thumbnail_selection_roundtrip_replace_and_remove() {
        let db = temp_db();
        let dir = dummy_dir();
        assert!(read_thumbnail_selection(&db, &dir, "book.zip").is_none());

        write_thumbnail_selection(&db, &dir, "book.zip", &ThumbnailSelection {
            entry_name: "pages/001.jpg".to_string(),
            source_kind: ThumbnailSourceKind::LeftHalf,
        });
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "book.zip"),
            Some(ThumbnailSelection {
                entry_name: "pages/001.jpg".to_string(),
                source_kind: ThumbnailSourceKind::LeftHalf,
            })
        );

        write_thumbnail_selection(&db, &dir, "book.zip", &ThumbnailSelection {
            entry_name: "pages/cover.png".to_string(),
            source_kind: ThumbnailSourceKind::RightHalf,
        });
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "book.zip"),
            Some(ThumbnailSelection {
                entry_name: "pages/cover.png".to_string(),
                source_kind: ThumbnailSourceKind::RightHalf,
            })
        );

        remove_thumbnail_selection(&db, &dir, "book.zip");
        assert!(read_thumbnail_selection(&db, &dir, "book.zip").is_none());
    }

    #[test]
    fn thumbnail_selection_v1_is_read_as_full() {
        let db = temp_db();
        let dir = dummy_dir();
        let key = make_key(&dir, "legacy.zip");
        {
            let db = db.lock().unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).unwrap();
                table.insert(key.as_str(), "pages/legacy.jpg").unwrap();
            }
            tx.commit().unwrap();
        }

        assert_eq!(
            read_thumbnail_selection(&db, &dir, "legacy.zip"),
            Some(ThumbnailSelection {
                entry_name: "pages/legacy.jpg".to_string(),
                source_kind: ThumbnailSourceKind::Full,
            })
        );
    }

    #[test]
    fn archive_sort_records_are_independent_and_listed_by_directory() {
        let db = temp_db();
        let dir = dummy_dir();
        let other_dir = unique_temp_path("other_dir");

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Name, true);
        write_archive_sort(&db, &dir, "b.zip", ReaderSortKey::Date, false);
        write_archive_sort(&db, &other_dir, "a.zip", ReaderSortKey::Natural, false);

        let mut values = list_dir_archive_sorts(&db, &dir);
        values.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].0, "a.zip");
        assert!(matches!(values[0].1, ReaderSortKey::Name));
        assert!(values[0].2);
        assert_eq!(values[1].0, "b.zip");
        assert!(matches!(values[1].1, ReaderSortKey::Date));
        assert!(!values[1].2);

        let other = read_archive_sort(&db, &other_dir, "a.zip").unwrap();
        assert!(matches!(other.0, ReaderSortKey::Natural));
        assert!(!other.1);
    }

    #[test]
    fn unknown_archive_sort_key_is_treated_as_unsaved() {
        let db = temp_db();
        let dir = dummy_dir();
        let db_key = make_key(&dir, "unknown.zip");
        {
            let db_guard = db.lock().unwrap();
            let tx = db_guard.begin_write().unwrap();
            {
                let mut table = tx.open_table(ARCHIVE_SORT_TABLE_V1).unwrap();
                table.insert(db_key.as_str(), (99, false)).unwrap();
            }
            tx.commit().unwrap();
        }

        assert!(read_archive_sort(&db, &dir, "unknown.zip").is_none());
        assert!(list_dir_archive_sorts(&db, &dir).is_empty());
    }

    #[test]
    fn opening_legacy_spread_only_db_adds_v1_sort_table_without_data_loss() {
        let root = unique_temp_path("legacy_root");
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("nekoviewer_spread.redb");
        {
            let db = Database::create(&db_path).unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(SPREAD_TABLE).unwrap();
                let key = make_key(&root, "legacy.zip");
                table
                    .insert(key.as_str(), (page_mode_to_u8(PageMode::SpreadLeft), 1))
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        let db = open_spread_db(&root).unwrap();
        let spreads = list_dir_entries(&db, &root);
        assert_eq!(spreads.len(), 1);
        assert_eq!(spreads[0].0, "legacy.zip");
        assert!(matches!(spreads[0].1, PageMode::SpreadLeft));
        assert_eq!(spreads[0].2, 1);
        assert!(list_dir_archive_sorts(&db, &root).is_empty());
    }

    #[test]
    fn bookmark_disabled_by_default_and_enable_creates_empty_position() {
        let db = temp_db();
        let dir = dummy_dir();
        assert!(read_bookmark(&db, &dir, "book.zip").is_none());

        write_bookmark_enabled(&db, &dir, "book.zip", true);
        let state = read_bookmark(&db, &dir, "book.zip").unwrap();
        assert!(state.enabled);
        assert_eq!(state.last_entry_name, "");
        assert_eq!(state.updated_at, 0);
        assert_eq!(state.archive_mtime, 0);
    }

    #[test]
    fn bookmark_disable_without_prior_record_is_noop() {
        let db = temp_db();
        let dir = dummy_dir();
        write_bookmark_enabled(&db, &dir, "book.zip", false);
        assert!(read_bookmark(&db, &dir, "book.zip").is_none());
    }

    #[test]
    fn bookmark_position_is_ignored_while_disabled() {
        let db = temp_db();
        let dir = dummy_dir();
        // レコード自体が無い状態
        write_bookmark_position(&db, &dir, "book.zip", "pages/010.jpg", 100);
        assert!(read_bookmark(&db, &dir, "book.zip").is_none());

        // 有効化した後に無効化した状態
        write_bookmark_enabled(&db, &dir, "book.zip", true);
        write_bookmark_enabled(&db, &dir, "book.zip", false);
        write_bookmark_position(&db, &dir, "book.zip", "pages/010.jpg", 100);
        let state = read_bookmark(&db, &dir, "book.zip").unwrap();
        assert!(!state.enabled);
        assert_eq!(state.last_entry_name, "", "無効中は位置を書き込まない");
    }

    #[test]
    fn bookmark_position_roundtrip_and_clear_keeps_enabled_flag() {
        let db = temp_db();
        let dir = dummy_dir();
        write_bookmark_enabled(&db, &dir, "book.zip", true);
        write_bookmark_position(&db, &dir, "book.zip", "pages/010.jpg", 12345);

        let state = read_bookmark(&db, &dir, "book.zip").unwrap();
        assert!(state.enabled);
        assert_eq!(state.last_entry_name, "pages/010.jpg");
        assert_eq!(state.archive_mtime, 12345);
        assert!(state.updated_at > 0);

        clear_bookmark_position(&db, &dir, "book.zip");
        let cleared = read_bookmark(&db, &dir, "book.zip").unwrap();
        assert!(cleared.enabled, "clearはenabledを維持する");
        assert_eq!(cleared.last_entry_name, "");
        assert_eq!(cleared.updated_at, 0);
        assert_eq!(cleared.archive_mtime, 0);
    }

    #[test]
    fn bookmark_records_are_independent_by_actual_parent_directory() {
        let db = temp_db();
        let dir = unique_temp_path("bookmark_dir");
        let other_dir = unique_temp_path("other_bookmark_dir");

        write_bookmark_enabled(&db, &dir, "book.zip", true);
        write_bookmark_position(&db, &dir, "book.zip", "pages/001.jpg", 1);
        write_bookmark_enabled(&db, &other_dir, "book.zip", true);
        write_bookmark_position(&db, &other_dir, "book.zip", "pages/099.jpg", 2);

        assert_eq!(read_bookmark(&db, &dir, "book.zip").unwrap().last_entry_name, "pages/001.jpg");
        assert_eq!(read_bookmark(&db, &other_dir, "book.zip").unwrap().last_entry_name, "pages/099.jpg");

        remove_bookmark(&db, &dir, "book.zip");
        assert!(read_bookmark(&db, &dir, "book.zip").is_none());
        assert!(read_bookmark(&db, &other_dir, "book.zip").is_some());
    }

    #[test]
    fn rating_record_is_absent_until_the_first_visit_or_rating() {
        let db = temp_db();
        let dir = unique_temp_path("rating_absent");
        assert_eq!(read_archive_rating(&db, &dir, "book.zip"), None, "NEW（未訪問）はレコード不在");
    }

    #[test]
    fn first_visit_creates_an_unrated_record_and_later_visits_count_up() {
        let db = temp_db();
        let dir = unique_temp_path("rating_visit");
        assert!(record_archive_visit(&db, &dir, "book.zip"));
        let first = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!(first.rating_half, 0, "訪問だけでは未評価のまま");
        assert_eq!(first.visit_count, 1);
        assert!(first.last_visit_at > 0);

        record_archive_visit(&db, &dir, "book.zip");
        record_archive_visit(&db, &dir, "book.zip");
        assert_eq!(read_archive_rating(&db, &dir, "book.zip").unwrap().visit_count, 3);
    }

    #[test]
    fn visit_keeps_the_rating_and_rating_keeps_the_visit_count() {
        let db = temp_db();
        let dir = unique_temp_path("rating_independent");
        record_archive_visit(&db, &dir, "book.zip");
        record_archive_visit(&db, &dir, "book.zip");
        assert!(write_archive_rating(&db, &dir, "book.zip", 7));

        let r = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (7, 2), "評価書込みは訪問回数に触れない");

        record_archive_visit(&db, &dir, "book.zip");
        let r = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (7, 3), "訪問記録は評価に触れない");
    }

    #[test]
    fn rating_write_is_idempotent_and_overwrites_by_absolute_value() {
        let db = temp_db();
        let dir = unique_temp_path("rating_idempotent");
        record_archive_visit(&db, &dir, "book.zip");
        for _ in 0..5 {
            assert!(write_archive_rating(&db, &dir, "book.zip", 5));
        }
        let r = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (5, 1), "連打しても値・訪問回数は変わらない");

        write_archive_rating(&db, &dir, "book.zip", 9);
        assert_eq!(read_archive_rating(&db, &dir, "book.zip").unwrap().rating_half, 9, "位置を変えて押し直せば上書き");
    }

    #[test]
    fn writing_zero_resets_to_unrated_but_keeps_the_record() {
        let db = temp_db();
        let dir = unique_temp_path("rating_unset");
        record_archive_visit(&db, &dir, "book.zip");
        write_archive_rating(&db, &dir, "book.zip", 8);
        write_archive_rating(&db, &dir, "book.zip", 0);
        write_archive_rating(&db, &dir, "book.zip", 0); // 既に未評価でも成功する
        let r = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (0, 1));
    }

    #[test]
    fn rating_write_without_prior_record_creates_it_with_zero_visits() {
        let db = temp_db();
        let dir = unique_temp_path("rating_no_visit");
        assert!(write_archive_rating(&db, &dir, "book.zip", 4));
        let r = read_archive_rating(&db, &dir, "book.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count, r.last_visit_at), (4, 0, 0));
    }

    #[test]
    fn rating_above_max_is_clamped_to_five_stars() {
        let db = temp_db();
        let dir = unique_temp_path("rating_clamp");
        write_archive_rating(&db, &dir, "book.zip", 200);
        assert_eq!(read_archive_rating(&db, &dir, "book.zip").unwrap().rating_half, RATING_HALF_MAX);
    }

    #[test]
    fn rating_records_are_independent_by_actual_parent_directory() {
        let db = temp_db();
        let dir = unique_temp_path("rating_dir");
        let other_dir = unique_temp_path("rating_other_dir");
        write_archive_rating(&db, &dir, "book.zip", 3);
        write_archive_rating(&db, &other_dir, "book.zip", 9);
        assert_eq!(read_archive_rating(&db, &dir, "book.zip").unwrap().rating_half, 3);
        assert_eq!(read_archive_rating(&db, &other_dir, "book.zip").unwrap().rating_half, 9);

        // 片方を更新しても、もう片方は影響を受けない。
        write_archive_rating(&db, &dir, "book.zip", 4);
        assert_eq!(read_archive_rating(&db, &dir, "book.zip").unwrap().rating_half, 4);
        assert_eq!(read_archive_rating(&db, &other_dir, "book.zip").unwrap().rating_half, 9);
    }

    #[test]
    fn list_dir_archive_ratings_returns_only_that_directory() {
        let db = temp_db();
        let dir = unique_temp_path("rating_list");
        let other_dir = unique_temp_path("rating_list_other");
        record_archive_visit(&db, &dir, "a.zip");
        write_archive_rating(&db, &dir, "b.zip", 10);
        write_archive_rating(&db, &other_dir, "c.zip", 2);

        let mut names: Vec<String> = list_dir_archive_ratings(&db, &dir).into_iter().map(|(n, _)| n).collect();
        names.sort();
        assert_eq!(names, vec!["a.zip".to_string(), "b.zip".to_string()]);
    }

    #[test]
    fn archive_tags_round_trip() {
        let db = temp_db();
        let dir = unique_temp_path("tags_round_trip");
        assert!(read_archive_tags(&db, &dir, "book.zip").is_empty());
        assert!(write_archive_tags(&db, &dir, "book.zip", &[3, 1, 2]));
        assert_eq!(read_archive_tags(&db, &dir, "book.zip"), vec![3, 1, 2]);
    }

    #[test]
    fn archive_tags_write_replaces_previous_set() {
        let db = temp_db();
        let dir = unique_temp_path("tags_replace");
        write_archive_tags(&db, &dir, "book.zip", &[1, 2, 3]);
        write_archive_tags(&db, &dir, "book.zip", &[9]);
        assert_eq!(read_archive_tags(&db, &dir, "book.zip"), vec![9]);
    }

    #[test]
    fn archive_tags_write_empty_removes_the_record() {
        let db = temp_db();
        let dir = unique_temp_path("tags_empty");
        write_archive_tags(&db, &dir, "book.zip", &[1]);
        write_archive_tags(&db, &dir, "book.zip", &[]);
        assert!(read_archive_tags(&db, &dir, "book.zip").is_empty());
    }

    #[test]
    fn archive_tags_are_independent_by_directory() {
        let db = temp_db();
        let dir = unique_temp_path("tags_dir");
        let other_dir = unique_temp_path("tags_other_dir");
        write_archive_tags(&db, &dir, "book.zip", &[1]);
        write_archive_tags(&db, &other_dir, "book.zip", &[2]);
        assert_eq!(read_archive_tags(&db, &dir, "book.zip"), vec![1]);
        assert_eq!(read_archive_tags(&db, &other_dir, "book.zip"), vec![2]);

        write_archive_tags(&db, &dir, "book.zip", &[]);
        assert!(read_archive_tags(&db, &dir, "book.zip").is_empty());
        assert_eq!(read_archive_tags(&db, &other_dir, "book.zip"), vec![2]);
    }

    // ---- ID化（評価・訪問・しおり）----

    fn real_dir(tag: &str) -> PathBuf {
        let dir = unique_temp_path(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 書き込み中と見なされないよう、mtimeを十分古くしたファイルを作る。
    fn real_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        p
    }

    fn seed_v1_rating(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, v: (u8, u32, i64)) {
        let key = make_key(dir, name);
        let guard = db.lock().unwrap();
        let tx = guard.begin_write().unwrap();
        {
            let mut t = tx.open_table(ARCHIVE_RATING_TABLE_V1).unwrap();
            t.insert(key.as_str(), v).unwrap();
        }
        tx.commit().unwrap();
    }

    fn seed_v1_bookmark(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, entry: &str, mtime: i64) {
        let key = make_key(dir, name);
        let guard = db.lock().unwrap();
        let tx = guard.begin_write().unwrap();
        {
            let mut t = tx.open_table(BOOKMARK_TABLE_V1).unwrap();
            t.insert(key.as_str(), (true, entry, 1i64, mtime)).unwrap();
        }
        tx.commit().unwrap();
    }

    fn v2_rating_rows(db: &Arc<Mutex<Database>>) -> usize {
        let guard = db.lock().unwrap();
        let tx = guard.begin_read().unwrap();
        tx.open_table(ARCHIVE_RATING_TABLE_V2).map_or(0, |t| t.iter().unwrap().count())
    }

    fn content(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn rating_of_real_file_is_stored_by_id_and_marks_identity_spec() {
        let db = temp_db();
        let dir = real_dir("id_rating");
        real_file(&dir, "a.zip", &content(3000));
        assert!(!is_identity_spec(&db));
        assert!(write_archive_rating(&db, &dir, "a.zip", 7));
        assert!(is_identity_spec(&db));
        assert_eq!(read_archive_rating(&db, &dir, "a.zip").unwrap().rating_half, 7);
        assert_eq!(v2_rating_rows(&db), 1);
        // 旧v1の行は作られない（IDが解決できる限り、新しい書き込みはv2へ）。
        let key = make_key(&dir, "a.zip");
        let guard = db.lock().unwrap();
        let tx = guard.begin_read().unwrap();
        assert!(tx.open_table(ARCHIVE_RATING_TABLE_V1).unwrap().get(key.as_str()).unwrap().is_none());
    }

    #[test]
    fn legacy_v1_rating_is_read_before_and_migrated_on_first_write() {
        let db = temp_db();
        let dir = real_dir("id_migrate");
        real_file(&dir, "a.zip", &content(3000));
        seed_v1_rating(&db, &dir, "a.zip", (6, 4, 99));
        // IDが無いうちは、旧v1をそのまま読む（旧値の表示）。
        assert_eq!(
            read_archive_rating(&db, &dir, "a.zip"),
            Some(ArchiveRating { rating_half: 6, visit_count: 4, last_visit_at: 99 })
        );
        // 最初の書き込みで旧値（評価6・訪問4）を引き継いでv2へ移り、訪問だけが+1される。
        assert!(record_archive_visit(&db, &dir, "a.zip"));
        let r = read_archive_rating(&db, &dir, "a.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (6, 5));
        assert_eq!(v2_rating_rows(&db), 1);
    }

    #[test]
    fn rating_follows_a_moved_file_and_copy_gets_its_own_record() {
        let db = temp_db();
        let dir1 = real_dir("id_move_a");
        let dir2 = real_dir("id_move_b");
        let p1 = real_file(&dir1, "a.zip", &content(5000));
        assert!(write_archive_rating(&db, &dir1, "a.zip", 9));

        // 移動（mtime・内容はそのまま）。新しい場所は未解決なので、解決すると評価が追従する。
        std::fs::rename(&p1, dir2.join("b.zip")).unwrap();
        assert!(crate::file_identity::ensure_record(&db, &dir2, "b.zip").is_some());
        assert_eq!(read_archive_rating(&db, &dir2, "b.zip").unwrap().rating_half, 9);
        assert_eq!(read_archive_rating(&db, &dir1, "a.zip"), None);

        // コピーは別ファイル扱い。評価は引き継がない。
        std::fs::copy(dir2.join("b.zip"), dir1.join("copy.zip")).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(dir1.join("copy.zip")).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(500)).unwrap();
        assert!(crate::file_identity::ensure_record(&db, &dir1, "copy.zip").is_some());
        assert_eq!(read_archive_rating(&db, &dir1, "copy.zip"), None);
        assert_eq!(read_archive_rating(&db, &dir2, "b.zip").unwrap().rating_half, 9);
    }

    #[test]
    fn unmigrated_v1_rating_is_reachable_after_a_move_via_origin_key() {
        let db = temp_db();
        let dir1 = real_dir("id_bridge_a");
        let dir2 = real_dir("id_bridge_b");
        let p1 = real_file(&dir1, "a.zip", &content(5000));
        seed_v1_rating(&db, &dir1, "a.zip", (8, 2, 50));
        // IDだけを作った（評価はまだv1のまま）状態で移動する。
        let rec = crate::file_identity::ensure_record(&db, &dir1, "a.zip").unwrap();
        assert_eq!(v2_rating_rows(&db), 0);
        std::fs::rename(&p1, dir2.join("b.zip")).unwrap();
        let moved = crate::file_identity::ensure_record(&db, &dir2, "b.zip").unwrap();
        assert_eq!(moved.id, rec.id);
        // 旧パスキー（origin_key）経由で旧v1の行に届く。
        assert_eq!(
            read_archive_rating(&db, &dir2, "b.zip"),
            Some(ArchiveRating { rating_half: 8, visit_count: 2, last_visit_at: 50 })
        );
    }

    #[test]
    fn list_dir_ratings_mixes_resolved_ids_and_unresolved_legacy_rows() {
        let db = temp_db();
        let dir = real_dir("id_list");
        real_file(&dir, "id.zip", &content(3000));
        real_file(&dir, "legacy.zip", &content(4000));
        assert!(write_archive_rating(&db, &dir, "id.zip", 3));
        seed_v1_rating(&db, &dir, "legacy.zip", (5, 1, 7));
        let other = real_dir("id_list_other");
        real_file(&other, "x.zip", &content(1000));
        assert!(write_archive_rating(&db, &other, "x.zip", 9));

        let mut got = list_dir_archive_ratings(&db, &dir);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<(&str, u8)> = got.iter().map(|(n, r)| (n.as_str(), r.rating_half)).collect();
        assert_eq!(names, vec![("id.zip", 3), ("legacy.zip", 5)]);
    }

    #[test]
    fn missing_file_falls_back_to_legacy_path_keyed_rows() {
        // 実体が無くてIDを作れない場合は、従来どおりパスキーのv1へ書き読みする。
        let db = temp_db();
        let dir = real_dir("id_fallback");
        assert!(write_archive_rating(&db, &dir, "ghost.zip", 2));
        assert_eq!(read_archive_rating(&db, &dir, "ghost.zip").unwrap().rating_half, 2);
        assert_eq!(v2_rating_rows(&db), 0);
        assert!(!is_identity_spec(&db));
    }

    #[test]
    fn id_has_user_data_turns_true_after_the_first_visit() {
        let db = temp_db();
        let dir = real_dir("id_has_data");
        real_file(&dir, "a.zip", &content(3000));
        let rec = crate::file_identity::ensure_record(&db, &dir, "a.zip").unwrap();
        assert!(!id_has_user_data(&db, rec.id));
        assert!(record_archive_visit(&db, &dir, "a.zip"));
        assert!(id_has_user_data(&db, rec.id));
    }

    #[test]
    fn bookmark_follows_a_moved_file_and_validity_uses_fp() {
        let db = temp_db();
        let dir1 = real_dir("id_bm_a");
        let dir2 = real_dir("id_bm_b");
        let p1 = real_file(&dir1, "a.zip", &content(5000));
        assert!(write_bookmark_enabled(&db, &dir1, "a.zip", true));
        write_bookmark_position(&db, &dir1, "a.zip", "p/010.jpg", 111);
        let saved = read_bookmark(&db, &dir1, "a.zip").unwrap();
        assert!(saved.archive_fp.is_some(), "保存時のFPを記録する");

        std::fs::rename(&p1, dir2.join("b.zip")).unwrap();
        let rec = crate::file_identity::ensure_record(&db, &dir2, "b.zip").unwrap();
        let moved = read_bookmark(&db, &dir2, "b.zip").unwrap();
        assert_eq!(moved.last_entry_name, "p/010.jpg");
        // mtimeが違っていても、内容（FP）が同じならしおりは有効。
        assert!(bookmark_matches_file(&moved, rec.fp, 99999));
    }

    #[test]
    fn bookmark_matches_file_prefers_fp_and_falls_back_to_mtime() {
        let state = |fp: Option<[u8; 16]>| BookmarkState {
            enabled: true,
            last_entry_name: "x".into(),
            updated_at: 0,
            archive_mtime: 100,
            archive_fp: fp,
        };
        assert!(bookmark_matches_file(&state(Some([1; 16])), Some([1; 16]), 999));
        assert!(!bookmark_matches_file(&state(Some([1; 16])), Some([2; 16]), 100));
        // 片方が不明（旧v1から移した行など）なら、更新日時で判定する。
        assert!(bookmark_matches_file(&state(None), Some([2; 16]), 100));
        assert!(!bookmark_matches_file(&state(None), Some([2; 16]), 101));
        assert!(bookmark_matches_file(&state(Some([1; 16])), None, 100));
    }

    #[test]
    fn remove_bookmark_does_not_resurrect_legacy_row() {
        let db = temp_db();
        let dir = real_dir("id_bm_remove");
        real_file(&dir, "a.zip", &content(3000));
        seed_v1_bookmark(&db, &dir, "a.zip", "old/005.jpg", 7);
        assert_eq!(read_bookmark(&db, &dir, "a.zip").unwrap().last_entry_name, "old/005.jpg");
        remove_bookmark(&db, &dir, "a.zip");
        assert!(read_bookmark(&db, &dir, "a.zip").is_none(), "旧v1の行が復活しない");
    }

    #[test]
    fn legacy_bookmark_is_migrated_with_unknown_fp() {
        let db = temp_db();
        let dir = real_dir("id_bm_migrate");
        real_file(&dir, "a.zip", &content(3000));
        seed_v1_bookmark(&db, &dir, "a.zip", "old/005.jpg", 7);
        // 位置を保存し直すと、旧しおりの有効/無効を引き継いだままFP付きのv2へ移る。
        write_bookmark_position(&db, &dir, "a.zip", "new/009.jpg", 8);
        let b = read_bookmark(&db, &dir, "a.zip").unwrap();
        assert_eq!((b.enabled, b.last_entry_name.as_str()), (true, "new/009.jpg"));
        assert!(b.archive_fp.is_some());
    }

    // ---- ID化（見開き・ソート・登録サムネ）----

    fn selection(entry: &str, kind: ThumbnailSourceKind) -> ThumbnailSelection {
        ThumbnailSelection { entry_name: entry.to_owned(), source_kind: kind }
    }

    fn settings_record_exists(db: &Arc<Mutex<Database>>, id: u64) -> bool {
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        tx.open_table(crate::file_settings::FILE_SETTINGS_TABLE_V2)
            .map(|t| t.get(id).unwrap().is_some())
            .unwrap_or(false)
    }

    #[test]
    fn file_settings_of_real_file_are_stored_by_id_not_by_path() {
        let db = temp_db();
        let dir = real_dir("fs_basic");
        real_file(&dir, "a.zip", &content(3000));
        assert!(write_spread(&db, &dir, "a.zip", PageMode::SpreadLeft, -1));
        assert!(write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Date, true));
        write_thumbnail_selection(&db, &dir, "a.zip", &selection("p/003.jpg", ThumbnailSourceKind::LeftHalf));
        assert_eq!(read_spread(&db, &dir, "a.zip"), Some((PageMode::SpreadLeft, -1)));
        assert_eq!(read_archive_sort(&db, &dir, "a.zip"), Some((ReaderSortKey::Date, true)));
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "a.zip"),
            Some(selection("p/003.jpg", ThumbnailSourceKind::LeftHalf))
        );
        let rec = crate::file_identity::ensure_record(&db, &dir, "a.zip").unwrap();
        assert!(settings_record_exists(&db, rec.id));
        // 旧v1（パスキー）には書かれない。
        let key = make_key(&dir, "a.zip");
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        assert!(tx.open_table(SPREAD_TABLE).unwrap().get(key.as_str()).unwrap().is_none());
        assert!(tx.open_table(ARCHIVE_SORT_TABLE_V1).unwrap().get(key.as_str()).unwrap().is_none());
    }

    #[test]
    fn first_write_migrates_all_legacy_settings_of_the_file_at_once() {
        let db = temp_db();
        let dir = real_dir("fs_migrate");
        let p = real_file(&dir, "a.zip", &content(4000));
        write_spread_v1(&db, &dir, "a.zip", PageMode::SpreadRight, 1);
        write_archive_sort_v1(&db, &dir, "a.zip", ReaderSortKey::Natural, false);
        write_thumbnail_selection_v1(&db, &dir, "a.zip", &selection("c.jpg", ThumbnailSourceKind::Full));
        // IDが無いうちは旧v1をそのまま読む。
        assert_eq!(read_spread(&db, &dir, "a.zip"), Some((PageMode::SpreadRight, 1)));
        // 見開きだけを書き換えると、ソート・登録サムネも同時にv2へ引き継がれる。
        assert!(write_spread(&db, &dir, "a.zip", PageMode::SpreadLeft, 0));
        // 移動しても、3つとも追従する（旧v1の行へ頼らない）。
        let dir2 = real_dir("fs_migrate_to");
        std::fs::rename(&p, dir2.join("b.zip")).unwrap();
        assert!(crate::file_identity::ensure_record(&db, &dir2, "b.zip").is_some());
        assert_eq!(read_spread(&db, &dir2, "b.zip"), Some((PageMode::SpreadLeft, 0)));
        assert_eq!(read_archive_sort(&db, &dir2, "b.zip"), Some((ReaderSortKey::Natural, false)));
        assert_eq!(
            read_thumbnail_selection(&db, &dir2, "b.zip"),
            Some(selection("c.jpg", ThumbnailSourceKind::Full))
        );
    }

    #[test]
    fn unmigrated_legacy_settings_follow_a_move_via_origin_key() {
        let db = temp_db();
        let dir1 = real_dir("fs_bridge_a");
        let dir2 = real_dir("fs_bridge_b");
        let p = real_file(&dir1, "a.zip", &content(4000));
        write_archive_sort_v1(&db, &dir1, "a.zip", ReaderSortKey::Name, true);
        // IDだけ作られた（設定はまだv1のまま）状態で移動する。書き込みは一度も起きていない。
        crate::file_identity::ensure_record(&db, &dir1, "a.zip").unwrap();
        std::fs::rename(&p, dir2.join("b.zip")).unwrap();
        crate::file_identity::ensure_record(&db, &dir2, "b.zip").unwrap();
        assert_eq!(read_archive_sort(&db, &dir2, "b.zip"), Some((ReaderSortKey::Name, true)));
    }

    #[test]
    fn cleared_setting_is_not_resurrected_from_legacy_row() {
        let db = temp_db();
        let dir = real_dir("fs_cleared");
        real_file(&dir, "a.zip", &content(3000));
        write_archive_sort_v1(&db, &dir, "a.zip", ReaderSortKey::Date, false);
        write_spread_v1(&db, &dir, "a.zip", PageMode::SpreadLeft, 0);
        remove_archive_sort(&db, &dir, "a.zip");
        assert_eq!(read_archive_sort(&db, &dir, "a.zip"), None, "旧v1が残っていても復活しない");
        assert!(list_dir_archive_sorts(&db, &dir).is_empty());
        // 解除していない見開きは旧v1から引き継がれて残る。
        assert_eq!(read_spread(&db, &dir, "a.zip"), Some((PageMode::SpreadLeft, 0)));
    }

    #[test]
    fn dir_lists_mix_resolved_ids_and_unresolved_legacy_rows() {
        let db = temp_db();
        let dir = real_dir("fs_list");
        real_file(&dir, "id.zip", &content(3000));
        real_file(&dir, "legacy.zip", &content(4000));
        assert!(write_spread(&db, &dir, "id.zip", PageMode::SpreadLeft, 0));
        write_spread_v1(&db, &dir, "legacy.zip", PageMode::SpreadRight, 1);
        write_archive_sort_v1(&db, &dir, "legacy.zip", ReaderSortKey::Name, true);

        let mut entries = list_dir_entries(&db, &dir);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            entries,
            vec![
                ("id.zip".to_owned(), PageMode::SpreadLeft, 0),
                ("legacy.zip".to_owned(), PageMode::SpreadRight, 1),
            ]
        );
        assert_eq!(
            list_dir_archive_sorts(&db, &dir),
            vec![("legacy.zip".to_owned(), ReaderSortKey::Name, true)]
        );
    }

    #[test]
    fn missing_file_falls_back_to_legacy_rows_for_settings() {
        let db = temp_db();
        let dir = real_dir("fs_ghost");
        assert!(write_spread(&db, &dir, "ghost.zip", PageMode::SpreadLeft, 1));
        assert!(write_archive_sort(&db, &dir, "ghost.zip", ReaderSortKey::Date, true));
        assert_eq!(read_spread(&db, &dir, "ghost.zip"), Some((PageMode::SpreadLeft, 1)));
        remove_spread(&db, &dir, "ghost.zip");
        assert_eq!(read_spread(&db, &dir, "ghost.zip"), None);
        assert!(!is_identity_spec(&db));
    }

    #[test]
    fn saved_settings_reflect_effective_values_of_ids_and_legacy() {
        let db = temp_db();
        let dir = real_dir("fs_saved");
        let a = real_file(&dir, "a.zip", &content(3000));
        let b = real_file(&dir, "b.zip", &content(4000));
        assert!(write_spread(&db, &dir, "a.zip", PageMode::SpreadRight, 0));
        write_thumbnail_selection_v1(&db, &dir, "b.zip", &selection("x.jpg", ThumbnailSourceKind::Full));
        let got = saved_settings_for_paths(&db, &[a.clone(), b.clone()]);
        assert_eq!(got[&a].spread_mode, Some(PageMode::SpreadRight));
        assert!(!got[&a].has_custom_thumbnail);
        assert!(got[&b].has_custom_thumbnail);
        assert_eq!(got[&b].spread_mode, None);
    }

    #[test]
    fn settings_count_as_user_data_only_while_a_value_is_set() {
        let db = temp_db();
        let dir = real_dir("fs_userdata");
        real_file(&dir, "a.zip", &content(3000));
        let rec = crate::file_identity::ensure_record(&db, &dir, "a.zip").unwrap();
        assert!(!id_has_user_data(&db, rec.id));
        assert!(write_spread(&db, &dir, "a.zip", PageMode::SpreadLeft, 0));
        assert!(id_has_user_data(&db, rec.id));
        remove_spread(&db, &dir, "a.zip");
        assert!(!id_has_user_data(&db, rec.id), "解除済みだけなら空のIDとして扱う");
    }

    #[test]
    fn legacy_v1_thumbnail_selection_is_read_as_full_source() {
        let db = temp_db();
        let dir = real_dir("fs_thumb_v1");
        real_file(&dir, "a.zip", &content(3000));
        // 第1世代（entry_nameのみ）の行。
        let key = make_key(&dir, "a.zip");
        {
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            {
                let mut t = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).unwrap();
                t.insert(key.as_str(), "old/cover.jpg").unwrap();
            }
            tx.commit().unwrap();
        }
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "a.zip"),
            Some(selection("old/cover.jpg", ThumbnailSourceKind::Full))
        );
        // 移行後（ID化済みの書き込み後）も同じ。
        assert!(write_spread(&db, &dir, "a.zip", PageMode::SpreadLeft, 0));
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "a.zip"),
            Some(selection("old/cover.jpg", ThumbnailSourceKind::Full))
        );
    }
}
