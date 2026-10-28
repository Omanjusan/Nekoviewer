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
}

struct Job {
    generation: u64,
    db: Arc<Mutex<Database>>,
    ids: Vec<u64>,
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
                    let keep_going = || worker_current.load(Ordering::Acquire) == job.generation;
                    probe_ids(&job.db, &job.ids, &keep_going, &file_identity::probe_record, &mut |presence| {
                        let _ = event_tx.send(ProbeEvent::Results { generation: job.generation, presence });
                        ctx.request_repaint();
                    });
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
            let _ = tx.send(Job { generation, db, ids });
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

    /// 確認結果を受け取る。表示（アイコン・絞り込み・並び）が変わりうるので、必要なら並びを作り直す。
    pub(super) fn poll_marker_probe(&mut self) {
        let mut changed = false;
        while let Ok(ProbeEvent::Results { generation, presence }) = self.marker_probe.event_rx.try_recv() {
            if !self.marker_probe.is_current(generation) {
                continue;
            }
            for (id, p) in presence {
                changed |= self.marker_presence.insert(id, p) != Some(p);
            }
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
}
