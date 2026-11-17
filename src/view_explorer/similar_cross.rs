//! 検索結果（新モード）の「？」の遅延照合。[similar_search.rs](crate::similar_search) の照合を、専用スレッドで、
//! **画面に出ている行の分だけ**行い、結果を読み取り専用で表示する（クリックしても何も起きない）。
//!
//! - 描画した行のファイルIDを集め（`sim_cross_note_wanted`）、フレームの終わりにまとめて依頼する（`submit_sim_cross_requests`）。
//!   スクロールして新しく出た行の分だけ、増分で計算する。全結果は計算しない。
//! - 専用スレッドは世代番号つき。結果の切り替え・表示を離れる・しきい値の変更で世代を進め、古い依頼は次の一歩で捨てる
//!   （中断ボタンは要らない）。ハッシュの表は、世代の最初の依頼で DB から読み直す（その後は持ち回る）。
//! - 保存済みのハッシュだけを使う。ハッシュが無いファイルは判定できないので、何も出さない。
//! - 画面のしきい値（サムネ一致閾値）に従う。変えると、計算し直す。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::Database;

use crate::similar_search::{find_similar, SimilarTable};
use crate::thumb_similar::SimilarMatch;

use super::dup_marker_ui::SimSummary;
use super::NekoviewApp;

/// 1回の依頼で渡す対象の最大数（1フレームに描かれる行の数より十分大きい）。
const MAX_IDS_PER_REQUEST: usize = 256;
/// ワーカー内で、世代を確かめながら進める単位。
const WORKER_CHUNK: usize = 16;

struct Job {
    generation: u64,
    ids: Vec<u64>,
    max_distance: u32,
    /// ハッシュの表を DB から読み直す（世代の最初の依頼）。
    reload_table: bool,
}

/// ワーカーからの結果。
pub(super) struct Output {
    pub generation: u64,
    /// 似た相手が見つかった対象（ID, 相手）。
    pub results: Vec<(u64, Vec<SimilarMatch>)>,
    /// 判定が済んだ対象（相手が無かったものも含む）。
    pub done_ids: Vec<u64>,
}

pub(super) struct SimilarCrossWorker {
    job_tx: Option<mpsc::Sender<Job>>,
    output_rx: mpsc::Receiver<Output>,
    current: Arc<AtomicU64>,
}

impl SimilarCrossWorker {
    pub(super) fn spawn(ctx: egui::Context, db: Arc<Mutex<Database>>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (output_tx, output_rx) = mpsc::channel::<Output>();
        let current = Arc::new(AtomicU64::new(0));
        let worker_current = Arc::clone(&current);
        std::thread::Builder::new()
            .name("similar-cross".into())
            .spawn(move || {
                let mut table: Option<SimilarTable> = None;
                while let Ok(job) = job_rx.recv() {
                    let generation = job.generation;
                    let alive = || worker_current.load(Ordering::Acquire) == generation;
                    if !alive() {
                        continue;
                    }
                    if job.reload_table || table.is_none() {
                        table = Some(SimilarTable::load(&db));
                        if !alive() {
                            continue;
                        }
                    }
                    let Some(t) = table.as_ref() else { continue };
                    for chunk in job.ids.chunks(WORKER_CHUNK) {
                        if !alive() {
                            break;
                        }
                        let results = find_similar(&db, t, chunk, job.max_distance, &alive);
                        if !alive() {
                            break;
                        }
                        let _ = output_tx.send(Output { generation, results, done_ids: chunk.to_vec() });
                        ctx.request_repaint();
                    }
                }
            })
            .expect("spawn similar-cross");
        Self { job_tx: Some(job_tx), output_rx, current }
    }

    /// 世代を進める。処理中の古い依頼は、次の一歩で打ち切られる。新しい世代の番号を返す。
    pub(super) fn reset(&self) -> u64 {
        self.current.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(super) fn generation(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    fn submit(&self, ids: Vec<u64>, max_distance: u32, reload_table: bool) {
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job { generation: self.generation(), ids, max_distance, reload_table });
        }
    }
}

/// 画面の状態（結果・依頼中・要求）。
#[derive(Default)]
pub(super) struct SimilarCrossState {
    /// 似た相手が見つかった対象の結果（ID → 近い順の相手）。
    pub matches: HashMap<u64, Vec<SimilarMatch>>,
    /// 判定が済んだ対象。
    pub done: HashSet<u64>,
    /// 依頼済みで、結果待ちの対象。
    pub inflight: HashSet<u64>,
    /// このフレームで描かれた、未判定の対象。フレームの終わりに依頼する。
    pub wanted: HashSet<u64>,
    /// 次の依頼で、ハッシュの表を読み直すか。
    pub reload_table: bool,
    /// 結果を計算したときのしきい値(%)。
    pub threshold: u8,
}

impl NekoviewApp {
    /// 結果をすべて捨てて、世代を進める（結果の切り替え・表示を離れる・しきい値の変更）。
    pub(super) fn reset_sim_cross(&mut self) {
        if let Some(w) = &self.sim_cross_worker {
            w.reset();
        }
        let threshold = self.sim_threshold_pct;
        self.sim_cross = SimilarCrossState { reload_table: true, threshold, ..Default::default() };
    }

    /// 描かれた行のファイルIDを控える（未判定・未依頼のものだけ）。
    pub(super) fn sim_cross_note_wanted(&mut self, id: u64) {
        if !self.sim_cross.done.contains(&id) && !self.sim_cross.inflight.contains(&id) {
            self.sim_cross.wanted.insert(id);
        }
    }

    /// 行のファイルIDに出す「？」の中身。消していない相手がしきい値を満たせば紫、無く「消去済」が入っていて、
    /// 消した相手が満たせば灰色。フォルダ表示の `sim_summary` と同じ規則。
    pub(super) fn sim_cross_summary(&self, id: u64) -> Option<SimSummary> {
        let matches = self.sim_cross.matches.get(&id)?;
        if let Some((pct, count)) = crate::thumb_similar::summarize(matches, self.sim_threshold_pct) {
            return Some(SimSummary { pct, count, dismissed: false });
        }
        if self.dup_show_dismissed
            && let Some((pct, count)) = crate::thumb_similar::summarize_dismissed(matches, self.sim_threshold_pct)
        {
            return Some(SimSummary { pct, count, dismissed: true });
        }
        None
    }

    /// フレームの終わり: 描かれた未判定の行を、まとめて依頼する。しきい値が変わっていれば、先に結果を捨てる。
    pub(super) fn submit_sim_cross_requests(&mut self) {
        if !self.cross_marker_view_active() {
            self.sim_cross.wanted.clear();
            return;
        }
        if self.sim_cross.threshold != self.sim_threshold_pct {
            self.reset_sim_cross();
            // 捨てたので、このフレームの要求は次のフレームで取り直す（描画がもう一度走る）。
            self.egui_ctx.request_repaint();
            return;
        }
        if self.sim_cross.wanted.is_empty() {
            return;
        }
        let Some(db) = self.spread_db.clone() else {
            self.sim_cross.wanted.clear();
            return;
        };
        let worker = self
            .sim_cross_worker
            .get_or_insert_with(|| SimilarCrossWorker::spawn(self.egui_ctx.clone(), db));
        let ids: Vec<u64> = self.sim_cross.wanted.drain().take(MAX_IDS_PER_REQUEST).collect();
        self.sim_cross.wanted.clear();
        self.sim_cross.inflight.extend(ids.iter().copied());
        let reload = std::mem::take(&mut self.sim_cross.reload_table);
        let max_distance = crate::thumb_hash::max_distance_for_pct(self.sim_threshold_pct);
        worker.submit(ids, max_distance, reload);
    }

    /// 毎フレーム: ワーカーの結果を取り込む。古い世代の結果は捨てる。
    pub(super) fn poll_sim_cross(&mut self) {
        let Some(worker) = &self.sim_cross_worker else { return };
        let current = worker.generation();
        let mut changed = false;
        while let Ok(out) = worker.output_rx.try_recv() {
            if out.generation != current {
                continue;
            }
            for (id, matches) in out.results {
                self.sim_cross.matches.insert(id, matches);
            }
            for id in out.done_ids {
                self.sim_cross.inflight.remove(&id);
                self.sim_cross.done.insert(id);
            }
            changed = true;
        }
        if changed {
            self.egui_ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_generation_advances_and_stale_jobs_are_dropped() {
        // DB を使わないので、世代の管理だけを確かめる。
        let current = Arc::new(AtomicU64::new(0));
        let g1 = current.fetch_add(1, Ordering::AcqRel) + 1;
        let alive = |g: u64| current.load(Ordering::Acquire) == g;
        assert!(alive(g1));
        let g2 = current.fetch_add(1, Ordering::AcqRel) + 1;
        assert!(!alive(g1) && alive(g2), "世代を進めると、古い依頼は生きていない");
    }

    #[test]
    fn default_state_is_empty_and_marks_a_reload_only_after_a_reset() {
        let s = SimilarCrossState::default();
        assert!(s.matches.is_empty() && s.done.is_empty() && s.inflight.is_empty() && s.wanted.is_empty());
        assert!(!s.reload_table, "既定では読み直しを要求しない（reset_sim_cross が立てる）");
    }
}
