//! タグ検索の走査エンジン。spread DB のID台帳を全件走査し、有効タグを全て持つ（AND）
//! ファイルをバックグラウンドで拾って、一定件数ごとのチャンクで送る。
//!
//! - 条件が空（有効タグなし）なら何も走査せず、すぐ完了を返す（空表示の仕様）。
//! - メインタグ「未設定」（`TAG_MAIN_UNSET_ID`）は疑似タグ。「メインカテゴリのどのtierも
//!   持たない」ファイルに一致する。DBに未設定は書かない仕様なので、タグ記録が無いファイルも
//!   対象になり、走査の基準はタグ記録ではなくID台帳になる。
//! - negative tier も普通のタグと同じ扱い。
//! - ファイルの実在確認（stat）はここではしない。パスはID台帳の現在のパスキーのまま返し、
//!   実在・オフラインの判定と表示用の解決は呼び出し側が表示領域の分だけ行う（LAZY）。
//! - ID未解決で旧v1のパスキーにだけタグが残る行は対象外（実体が無いファイルの残骸のため）。
//! - 打ち切りは `TagScan` を捨てる（Drop）か `cancel` で行う。タグを切り替えたら古い走査を
//!   捨てて新しく始める。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{FileRecord, FileStatus};
use crate::tag_manager::TAG_MAIN_UNSET_ID;

/// 1チャンクに詰める最大件数。
pub const CHUNK_SIZE: usize = 200;

/// 検索条件。有効タグから、実タグの必須集合と「メイン未設定を要求するか」に分けて持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagQuery {
    required: HashSet<u64>,
    want_main_unset: bool,
    main_ids: HashSet<u64>,
}

impl TagQuery {
    /// `enabled`: 有効にしたタグ（未設定の疑似タグ `TAG_MAIN_UNSET_ID` を含みうる）。
    /// `main_ids`: メインカテゴリに属するtier_idの全体（未設定の判定に使う）。
    pub fn new(enabled: &HashSet<u64>, main_ids: HashSet<u64>) -> Self {
        Self {
            required: enabled.iter().copied().filter(|&id| id != TAG_MAIN_UNSET_ID).collect(),
            want_main_unset: enabled.contains(&TAG_MAIN_UNSET_ID),
            main_ids,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.required.is_empty() && !self.want_main_unset
    }

    /// ファイルのタグ集合が条件を満たすか（全部AND）。
    pub fn matches(&self, tags: &[u64]) -> bool {
        if !self.required.iter().all(|id| tags.contains(id)) {
            return false;
        }
        !self.want_main_unset || !tags.iter().any(|id| self.main_ids.contains(id))
    }
}

/// 走査で見つかった1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagHit {
    pub id: u64,
    /// ID台帳の現在のパス（実在は未確認）。
    pub path: PathBuf,
    /// 前回の確認でファイルが見つからなかったレコードなら false。
    pub confirmed: bool,
}

#[derive(Debug)]
pub enum TagScanMsg {
    Hits(Vec<TagHit>),
    /// 走査完了（打ち切りでは送られない）。`scanned` は調べたレコード数。
    Done { scanned: usize },
}

/// 実行中の走査。捨てると（Drop）走査スレッドは次のレコードで打ち切る。
pub struct TagScan {
    pub rx: mpsc::Receiver<TagScanMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for TagScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// バックグラウンドで走査を始める。チャンクや完了を送るたびに `wake` を呼ぶ（再描画要求用）。
pub fn spawn(db: Arc<Mutex<Database>>, query: TagQuery, wake: impl Fn() + Send + 'static) -> TagScan {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    std::thread::spawn(move || {
        let scanned = run_scan(&db, &query, CHUNK_SIZE, &flag, |hits| {
            if tx.send(TagScanMsg::Hits(hits)).is_ok() {
                wake();
            }
        });
        if !flag.load(Ordering::Relaxed) {
            let _ = tx.send(TagScanMsg::Done { scanned });
            wake();
        }
    });
    TagScan { rx, cancel }
}

/// 走査本体。`emit` に `chunk` 件ごと（と最後の端数）を渡し、調べたレコード数を返す。
/// `cancel` が立ったら、その時点までの端数は送らずに打ち切る。
fn run_scan(
    db: &Arc<Mutex<Database>>,
    query: &TagQuery,
    chunk: usize,
    cancel: &AtomicBool,
    mut emit: impl FnMut(Vec<TagHit>),
) -> usize {
    if query.is_empty() {
        return 0;
    }
    // 読み取りトランザクションを取ったらロックは放す（MVCCなので書き込みを止めない）。
    let tx = {
        let Ok(guard) = db.lock() else { return 0 };
        let Ok(tx) = guard.begin_read() else { return 0 };
        tx
    };
    let mut scanned = 0usize;
    let mut buf: Vec<TagHit> = Vec::new();
    crate::file_identity::for_each_record_tx(&tx, |rec: FileRecord| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        scanned += 1;
        let tags = crate::file_settings::effective_tags_tx(&tx, &rec).unwrap_or_default();
        if query.matches(&tags) {
            buf.push(TagHit {
                id: rec.id,
                path: crate::file_identity::path_of_key(&rec.path_key),
                confirmed: rec.status == FileStatus::Confirmed,
            });
            if buf.len() >= chunk {
                emit(std::mem::take(&mut buf));
            }
        }
        true
    });
    if !buf.is_empty() && !cancel.load(Ordering::Relaxed) {
        emit(buf);
    }
    scanned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spread_state::{open_spread_db, write_archive_tags};
    use std::path::Path;

    fn set(ids: &[u64]) -> HashSet<u64> {
        ids.iter().copied().collect()
    }

    // main tier = 1,2 / 属性 tier = 10,11,12
    fn query(enabled: &[u64]) -> TagQuery {
        TagQuery::new(&set(enabled), set(&[1, 2]))
    }

    #[test]
    fn empty_query_matches_nothing_by_design() {
        assert!(query(&[]).is_empty());
        assert!(!query(&[10]).is_empty());
        assert!(!query(&[TAG_MAIN_UNSET_ID]).is_empty());
    }

    #[test]
    fn all_enabled_tags_must_be_present() {
        let q = query(&[10, 11]);
        assert!(q.matches(&[10, 11, 12]));
        assert!(q.matches(&[1, 11, 10]));
        assert!(!q.matches(&[10]));
        assert!(!q.matches(&[]));
    }

    #[test]
    fn main_unset_matches_files_without_any_main_tier() {
        let q = query(&[TAG_MAIN_UNSET_ID]);
        assert!(q.matches(&[]), "タグ記録が無いファイルも未設定");
        assert!(q.matches(&[10, 11]), "属性タグだけならメインは未設定");
        assert!(!q.matches(&[1]));
        assert!(!q.matches(&[2, 10]));
        let q = query(&[TAG_MAIN_UNSET_ID, 10]);
        assert!(q.matches(&[10]));
        assert!(!q.matches(&[]), "属性タグ10が無い");
        assert!(!q.matches(&[1, 10]));
        // 未設定と実メインタグの同時指定は矛盾するので何にも一致しない
        assert!(!query(&[TAG_MAIN_UNSET_ID, 1]).matches(&[1]));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_tag_search_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_file(dir: &Path, name: &str, len: usize) {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| ((i + name.len()) % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
    }

    /// name -> tags の台帳を作る（タグ空のファイルもIDだけ作る）。
    fn seed(tag: &str, files: &[(&str, &[u64])]) -> (Arc<Mutex<Database>>, PathBuf) {
        let root = temp_dir(tag);
        let db = open_spread_db(&root).unwrap();
        for (i, (name, tags)) in files.iter().enumerate() {
            make_file(&root, name, 3000 + i * 7);
            crate::file_identity::ensure_record(&db, &root, name).unwrap();
            if !tags.is_empty() {
                assert!(write_archive_tags(&db, &root, name, tags));
            }
        }
        (db, root)
    }

    fn collect(db: &Arc<Mutex<Database>>, q: &TagQuery, chunk: usize) -> (Vec<Vec<TagHit>>, usize) {
        let mut chunks = Vec::new();
        let scanned = run_scan(db, q, chunk, &AtomicBool::new(false), |h| chunks.push(h));
        (chunks, scanned)
    }

    fn names(chunks: &[Vec<TagHit>]) -> Vec<String> {
        let mut v: Vec<String> = chunks
            .iter()
            .flatten()
            .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn scan_finds_and_matches_across_the_ledger() {
        let (db, _root) = seed(
            "find",
            &[("a.zip", &[1, 10, 11]), ("b.zip", &[2, 10]), ("c.zip", &[10, 11]), ("d.zip", &[]), ("e.zip", &[1])],
        );
        let (chunks, scanned) = collect(&db, &query(&[10, 11]), 100);
        assert_eq!(scanned, 5);
        assert_eq!(names(&chunks), vec!["a.zip", "c.zip"]);
        // 未設定: メインtierを持たない c(属性のみ) と d(記録なし)
        let (chunks, _) = collect(&db, &query(&[TAG_MAIN_UNSET_ID]), 100);
        assert_eq!(names(&chunks), vec!["c.zip", "d.zip"]);
        // 未設定 AND 属性10
        let (chunks, _) = collect(&db, &query(&[TAG_MAIN_UNSET_ID, 10]), 100);
        assert_eq!(names(&chunks), vec!["c.zip"]);
        assert!(chunks.iter().flatten().all(|h| h.confirmed));
    }

    #[test]
    fn empty_query_scans_nothing() {
        let (db, _root) = seed("empty", &[("a.zip", &[10])]);
        let (chunks, scanned) = collect(&db, &query(&[]), 100);
        assert!(chunks.is_empty());
        assert_eq!(scanned, 0);
    }

    #[test]
    fn hits_are_sent_in_chunks_with_a_remainder() {
        let files: Vec<(String, Vec<u64>)> = (0..5).map(|i| (format!("f{i}.zip"), vec![10])).collect();
        let refs: Vec<(&str, &[u64])> = files.iter().map(|(n, t)| (n.as_str(), t.as_slice())).collect();
        let (db, _root) = seed("chunks", &refs);
        let (chunks, _) = collect(&db, &query(&[10]), 2);
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 1]);
    }

    #[test]
    fn cancel_stops_the_scan_without_sending_the_remainder() {
        let (db, _root) = seed("cancel", &[("a.zip", &[10]), ("b.zip", &[10])]);
        let cancel = AtomicBool::new(true);
        let mut sent = 0;
        let scanned = run_scan(&db, &query(&[10]), 100, &cancel, |_| sent += 1);
        assert_eq!((sent, scanned), (0, 0));
    }

    #[test]
    fn spawned_scan_delivers_hits_then_done() {
        let (db, _root) = seed("spawn", &[("a.zip", &[10]), ("b.zip", &[11])]);
        let scan = spawn(db, query(&[10]), || {});
        let mut hit_names = Vec::new();
        loop {
            match scan.rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                TagScanMsg::Hits(h) => hit_names.extend(h.into_iter().map(|h| h.path)),
                TagScanMsg::Done { scanned } => {
                    assert_eq!(scanned, 2);
                    break;
                }
            }
        }
        assert_eq!(hit_names.len(), 1);
        assert!(hit_names[0].ends_with("a.zip"));
    }
}
