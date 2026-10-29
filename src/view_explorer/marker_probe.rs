//! 重複マーカーの重複先の存在確認。専用の小さなスレッドで、ファイルIDの現在のパスを確認する。
//!
//! - 確認はフォルダを読み込むたびに1回（UI側が、まだ確認していないIDだけを投入する）
//! - ネットワークドライブで `metadata` が止まっても画面が固まらないよう、UIスレッドでは確認しない
//! - 結果は「現存／消失／オフライン」。台帳には書かない（表示だけに反映する）。消失は、重複先が移動しただけで
//!   まだ新しい場所を開いていない場合も含むので、オフラインと同じく断定せず、あとで更新されれば元に戻る
//! - 世代: フォルダを読み込み直すたびに進める。古い世代のジョブは次のIDで打ち切り、古い結果は捨てる

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{self, FileRecord, Presence};

/// 結果を返す単位（これだけ確認するたびに1回送る）。
const CHUNK: usize = 32;

pub(super) enum ProbeEvent {
    Results { generation: u64, presence: Vec<(u64, Presence)> },
    /// 遡及検出（フォルダ内の同内容ファイルへのマーカー付け）が済んだ。`created` は新しくできたマーカーの数。
    Detected { generation: u64, created: usize },
    /// 類似ファイル一覧の収集が済んだ。`token` は一覧を開くたびに変わる番号（閉じた後の古い結果を捨てる）。
    Group { generation: u64, token: u64, group: SimilarGroup },
}

/// 類似ファイル一覧に並べるファイル1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SimilarFile {
    pub id: u64,
    pub path: std::path::PathBuf,
    /// 集めた時点のサイズ・更新日時（完全比較の結果が今も有効かの判定に使う）。
    pub stat: Option<super::marker_compare::Stat>,
}

/// 同じ内容（FP一致）のファイルの集まり。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SimilarGroup {
    /// 現存するファイル（ID昇順）。
    pub files: Vec<SimilarFile>,
    /// 一覧に出せないファイルの数（消失／オフライン）。
    pub missing: usize,
    pub offline: usize,
}

enum Job {
    /// 重複先の存在確認。
    Probe { generation: u64, db: Arc<Mutex<Database>>, ids: Vec<u64> },
    /// フォルダ内のファイルについて、同じ内容（FP一致）の現存ファイルを探してマーカーを付ける。
    Detect { generation: u64, db: Arc<Mutex<Database>>, dir: std::path::PathBuf },
    /// 類似ファイル一覧のために、`entry_id` と同じ内容の現存ファイルを全て集める。
    Group { generation: u64, token: u64, db: Arc<Mutex<Database>>, entry_id: u64 },
}

pub(super) struct MarkerProbeWorker {
    job_tx: Option<mpsc::Sender<Job>>,
    pub(super) event_rx: mpsc::Receiver<ProbeEvent>,
    current: Arc<AtomicU64>,
}

impl MarkerProbeWorker {
    pub(super) fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (event_tx, event_rx) = mpsc::channel::<ProbeEvent>();
        let current = Arc::new(AtomicU64::new(0));
        let worker_current = Arc::clone(&current);
        std::thread::Builder::new()
            .name("marker-probe".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Probe { generation, db, ids } => {
                            let keep_going = || worker_current.load(Ordering::Acquire) == generation;
                            probe_ids(&db, &ids, &keep_going, &file_identity::probe_record, &mut |presence| {
                                let _ = event_tx.send(ProbeEvent::Results { generation, presence });
                                ctx.request_repaint();
                            });
                        }
                        Job::Detect { generation, db, dir } => {
                            let created = crate::duplicate_marker::detect_in_dir(
                                &db,
                                &dir,
                                &file_identity::probe_record,
                                file_identity::now_unix(),
                            );
                            let _ = event_tx.send(ProbeEvent::Detected { generation, created });
                            ctx.request_repaint();
                        }
                        Job::Group { generation, token, db, entry_id } => {
                            let keep_going = || worker_current.load(Ordering::Acquire) == generation;
                            if let Some(group) = collect_group(&db, entry_id, &keep_going, &file_identity::probe_record) {
                                let _ = event_tx.send(ProbeEvent::Group { generation, token, group });
                                ctx.request_repaint();
                            }
                        }
                    }
                }
            })
            .expect("spawn marker-probe");
        Self { job_tx: Some(job_tx), event_rx, current }
    }

    /// 世代を進める（フォルダを読み込み直す時）。処理中の古いジョブは次のIDで打ち切られる。
    pub(super) fn invalidate(&self) {
        self.current.fetch_add(1, Ordering::AcqRel);
    }

    /// 今の世代で確認を依頼する（世代は進めない。同じ世代の先の依頼は打ち切らない）。
    pub(super) fn submit(&self, db: Arc<Mutex<Database>>, ids: Vec<u64>) {
        let generation = self.current.load(Ordering::Acquire);
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job::Probe { generation, db, ids });
        }
    }

    /// 遡及検出を依頼する（フォルダ1つぶん）。結果は `Detected` で返る。
    pub(super) fn submit_detect(&self, db: Arc<Mutex<Database>>, dir: std::path::PathBuf) {
        let generation = self.current.load(Ordering::Acquire);
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job::Detect { generation, db, dir });
        }
    }

    /// 類似ファイル一覧の収集を依頼する。結果は `Group` で返る。
    pub(super) fn submit_group(&self, db: Arc<Mutex<Database>>, entry_id: u64, token: u64) {
        let generation = self.current.load(Ordering::Acquire);
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job::Group { generation, token, db, entry_id });
        }
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.current.load(Ordering::Acquire) == generation
    }

    /// 終了時に呼ぶ。処理中のジョブは打ち切られ、以後の依頼は受け付けない。
    pub(super) fn shutdown(&mut self) {
        self.invalidate();
        self.job_tx = None;
    }
}

/// `ids` の現在のパスを確認して、`CHUNK` 件ごとに `emit` へ渡す。IDのレコードが無いものは結果に含めない。
/// レコードはDBロックの中でまとめて読み、ファイルの確認はロックの外で行う。
fn probe_ids(
    db: &Arc<Mutex<Database>>,
    ids: &[u64],
    keep_going: &dyn Fn() -> bool,
    probe: &dyn Fn(&FileRecord) -> Presence,
    emit: &mut dyn FnMut(Vec<(u64, Presence)>),
) {
    let records: Vec<FileRecord> = {
        let Ok(guard) = db.lock() else { return };
        let Ok(tx) = guard.begin_read() else { return };
        ids.iter().filter_map(|id| file_identity::record_by_id_tx(&tx, *id)).collect()
    };
    for chunk in records.chunks(CHUNK) {
        if !keep_going() {
            return;
        }
        let presence = chunk.iter().map(|rec| (rec.id, probe(rec))).collect();
        emit(presence);
    }
}

/// `entry_id` と同じFPのファイルを全て集め、現存するものを返す（件数の上限なし）。
/// レコードはDBロックの中でまとめて読み、ファイルの確認はロックの外で行う。途中で世代が変われば None。
/// 入口のIDにFPが無い（0バイト等）ときは、入口だけの組になる。
fn collect_group(
    db: &Arc<Mutex<Database>>,
    entry_id: u64,
    keep_going: &dyn Fn() -> bool,
    probe: &dyn Fn(&FileRecord) -> Presence,
) -> Option<SimilarGroup> {
    let mut members: Vec<FileRecord> = {
        let guard = db.lock().ok()?;
        let tx = guard.begin_read().ok()?;
        let entry = file_identity::record_by_id_tx(&tx, entry_id)?;
        match &entry.fp {
            Some(fp) => file_identity::records_with_fp_tx(&tx, fp),
            None => vec![entry],
        }
    };
    // 同じパスを指す古いレコードは、別のファイルとは数えない。
    members.sort_by_key(|m| m.id);
    members.dedup_by(|a, b| a.path_key == b.path_key);
    let mut group = SimilarGroup::default();
    for m in &members {
        if !keep_going() {
            return None;
        }
        match probe(m) {
            Presence::Present => {
                let path = file_identity::path_of_key(&m.path_key);
                let stat = super::marker_compare::stat(&path);
                group.files.push(SimilarFile { id: m.id, path, stat });
            }
            Presence::Missing => group.missing += 1,
            Presence::Offline => group.offline += 1,
        }
    }
    Some(group)
}

impl super::NekoviewApp {
    /// 表示中のマーカーの重複先（再取得）と参照元（旧ファイル側）のうち、まだ確認を依頼していないIDを確認に出す。
    pub(super) fn request_marker_probe(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let mut ids: Vec<u64> = Vec::new();
        for marker in self.dir_markers.values() {
            if let Some(own) = marker.own.as_ref().filter(|o| o.kind == crate::duplicate_marker::MarkerKind::Reacquired) {
                ids.extend(&own.targets);
            }
            ids.extend(&marker.referenced_by);
        }
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|id| self.marker_probe_requested.insert(*id));
        if !ids.is_empty() {
            self.marker_probe.submit(db, ids);
        }
    }

    /// 表示中のフォルダで、同じ内容の別ファイルを探してマーカーを付ける（遡及検出）。
    /// ID解決が済んだ後に呼ぶ（新しく現れたファイルにもFPが付いているので、同時に拾える）。
    pub(super) fn request_marker_detect(&mut self) {
        if !self.marker_view_active() {
            return;
        }
        let Some(db) = self.spread_db.clone() else { return };
        self.marker_probe.submit_detect(db, self.current_dir.clone());
    }

    /// 確認結果を受け取る。表示（アイコン・絞り込み・並び）が変わりうるので、必要なら並びを作り直す。
    pub(super) fn poll_marker_probe(&mut self) {
        let mut changed = false;
        let mut detected = 0;
        while let Ok(event) = self.marker_probe.event_rx.try_recv() {
            match event {
                ProbeEvent::Results { generation, presence } => {
                    if !self.marker_probe.is_current(generation) {
                        continue;
                    }
                    for (id, p) in presence {
                        changed |= self.marker_presence.insert(id, p) != Some(p);
                    }
                }
                ProbeEvent::Detected { generation, created } => {
                    if self.marker_probe.is_current(generation) {
                        detected += created;
                    }
                }
                ProbeEvent::Group { generation, token, group } => {
                    if self.marker_probe.is_current(generation) {
                        self.apply_similar_group(token, group);
                    }
                }
            }
        }
        if detected > 0 && self.marker_view_active() {
            // 新しく付いたマーカーを表示に反映する（台帳を読み直し、重複先の確認も依頼する）。
            self.reload_dir_state_maps();
            self.set_toast(crate::i18n::t().dup_marker_toast(detected));
        }
        if changed {
            if self.dup_filter || self.dup_sort {
                self.resort_keeping_selection_always();
            }
            self.egui_ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::*;
    use crate::spread_state::open_spread_db;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root =
                std::env::temp_dir().join(format!("nekoviewer_marker_probe_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn register(db: &Arc<Mutex<Database>>, dir: &std::path::Path, name: &str, fill: u8) -> u64 {
        std::fs::write(dir.join(name), vec![fill; 64]).unwrap();
        // 書いたばかりのファイルは書き込み中を疑ってFPを取らないので、古い更新日時にする。
        let f = std::fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        file_identity::ensure_record(db, dir, name).unwrap().id
    }

    #[test]
    fn real_probe_tells_present_from_missing() {
        let t = TempRoot::new("real");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        std::fs::remove_file(t.0.join("b.zip")).unwrap();
        let out = RefCell::new(Vec::new());
        probe_ids(&db, &[a, b], &|| true, &file_identity::probe_record, &mut |p| out.borrow_mut().extend(p));
        let out = out.into_inner();
        assert!(out.contains(&(a, Presence::Present)), "{out:?}");
        assert!(out.contains(&(b, Presence::Missing)), "{out:?}");
    }

    #[test]
    fn unknown_ids_are_skipped_and_results_are_chunked() {
        let t = TempRoot::new("chunk");
        let db = open_spread_db(&t.0).unwrap();
        let ids: Vec<u64> = (0..(CHUNK as u8 + 3)).map(|i| register(&db, &t.0, &format!("f{i}.zip"), i + 1)).collect();
        let mut query = ids.clone();
        query.push(999_999);
        let chunks = RefCell::new(Vec::new());
        probe_ids(&db, &query, &|| true, &|_| Presence::Present, &mut |p| chunks.borrow_mut().push(p.len()));
        assert_eq!(chunks.into_inner(), vec![CHUNK, 3], "レコードの無いIDは含めず、CHUNK件ごとに送る");
    }

    #[test]
    fn stopping_midway_discards_the_rest() {
        let t = TempRoot::new("stop");
        let db = open_spread_db(&t.0).unwrap();
        let ids: Vec<u64> = (0..(CHUNK as u8 * 2)).map(|i| register(&db, &t.0, &format!("g{i}.zip"), i + 1)).collect();
        let sent = RefCell::new(0usize);
        let calls = RefCell::new(0usize);
        let keep_going = || {
            *calls.borrow_mut() += 1;
            *calls.borrow() <= 1
        };
        probe_ids(&db, &ids, &keep_going, &|_| Presence::Present, &mut |p| *sent.borrow_mut() += p.len());
        assert_eq!(sent.into_inner(), CHUNK, "世代が変わったら次の塊からは確認しない");
    }

    #[test]
    fn group_collects_every_present_file_with_the_same_content() {
        let t = TempRoot::new("group");
        let db = open_spread_db(&t.0).unwrap();
        let sub = t.0.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let a = register(&db, &t.0, "a.zip", 7);
        let b = register(&db, &sub, "b.zip", 7);
        let c = register(&db, &t.0, "c.zip", 7);
        let other = register(&db, &t.0, "other.zip", 9);
        std::fs::remove_file(t.0.join("c.zip")).unwrap();
        let group = collect_group(&db, b, &|| true, &file_identity::probe_record).unwrap();
        let ids: Vec<u64> = group.files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![a, b], "現存する同内容ファイルが、ID昇順で全て並ぶ");
        assert!(!ids.contains(&other));
        assert_eq!((group.missing, group.offline), (1, 0), "消えた {c} は数だけ数える");
    }

    #[test]
    fn group_has_no_size_cap() {
        let t = TempRoot::new("nocap");
        let db = open_spread_db(&t.0).unwrap();
        let ids: Vec<u64> = (0..40).map(|i| register(&db, &t.0, &format!("n{i}.zip"), 5)).collect();
        let group = collect_group(&db, ids[0], &|| true, &|_| Presence::Present).unwrap();
        assert_eq!(group.files.len(), 40, "マーカーの重複先の上限(16)は一覧に関係しない");
    }

    #[test]
    fn group_is_abandoned_when_the_generation_changes() {
        let t = TempRoot::new("abandon");
        let db = open_spread_db(&t.0).unwrap();
        let a = register(&db, &t.0, "p.zip", 3);
        register(&db, &t.0, "q.zip", 3);
        assert!(collect_group(&db, a, &|| false, &|_| Presence::Present).is_none());
    }

    #[test]
    fn group_of_an_unknown_id_is_none() {
        let t = TempRoot::new("unknown");
        let db = open_spread_db(&t.0).unwrap();
        assert!(collect_group(&db, 12345, &|| true, &|_| Presence::Present).is_none());
    }
}
