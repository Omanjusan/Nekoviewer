//! ビューアーページデコード用ジョブ管理の基礎部分。
//!
//! フェーズ1では既存ワーカーへまだ接続せず、優先度、必要集合の差し替え、
//! active/preparing の2世代管理、待機・実行中ジョブの協調キャンセルを独立して検証する。
use std::cmp::Ordering;
use std::collections::HashSet;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DecodeJobKey {
    pub archive_path: PathBuf,
    pub page_index: usize,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagePriorityClass {
    Visible,
    Ahead,
    Behind,
}

impl PagePriorityClass {
    fn rank(self) -> u8 {
        match self {
            Self::Visible => 3,
            Self::Ahead => 2,
            Self::Behind => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptedGenerations {
    pub active: u64,
    pub preparing: Option<u64>,
}

impl AcceptedGenerations {
    fn accepts(self, generation: u64) -> bool {
        generation == self.active || self.preparing == Some(generation)
    }

    fn rank(self, generation: u64) -> Option<u8> {
        if self.preparing == Some(generation) {
            Some(2)
        } else if self.active == generation {
            Some(1)
        } else {
            None
        }
    }
}

pub struct DesiredDecodeJob<T> {
    pub key: DecodeJobKey,
    pub class: PagePriorityClass,
    /// 現在の可視ページからの距離。Visible は通常0。
    pub distance: usize,
    pub payload: T,
}

/// フェーズ2でワーカー結果を成功・失敗・キャンセルの全経路から返すための共通契約。
pub enum DecodeJobOutcome<T> {
    Ready(T),
    Failed,
    #[allow(dead_code)] // キュー内キャンセルは内部消費。将来の観測用契約として予約する。
    Cancelled,
}

pub struct ScheduledDecodeJob<T> {
    pub id: u64,
    #[allow(dead_code)] // フェーズ3の世代別結果配置で使用する。
    pub key: DecodeJobKey,
    pub payload: T,
    cancelled: Arc<AtomicBool>,
}

impl<T> ScheduledDecodeJob<T> {
    /// デコード開始直前と完了直後に確認する。デコード処理中は強制停止しない。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(AtomicOrdering::Acquire)
    }
}

struct QueuedJob<T> {
    id: u64,
    key: DecodeJobKey,
    class: PagePriorityClass,
    distance: usize,
    order: u64,
    payload: T,
    cancelled: Arc<AtomicBool>,
}

struct RunningJob {
    key: DecodeJobKey,
    class: PagePriorityClass,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HeapEntry {
    id: u64,
    key: DecodeJobKey,
    generation_rank: u8,
    class_rank: u8,
    distance: usize,
    order: u64,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.generation_rank
            .cmp(&other.generation_rank)
            .then_with(|| self.class_rank.cmp(&other.class_rank))
            // BinaryHeapは最大値を先に返すので、距離と投入順は小さい方を優先する。
            .then_with(|| other.distance.cmp(&self.distance))
            .then_with(|| other.order.cmp(&self.order))
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct QueueState<T> {
    generations: AcceptedGenerations,
    queued: HashMap<DecodeJobKey, QueuedJob<T>>,
    heap: BinaryHeap<HeapEntry>,
    running: HashMap<u64, RunningJob>,
    /// Visible以外（Ahead/Behind）を同時実行してよい最大数。
    max_speculative_running: usize,
    next_id: u64,
    next_order: u64,
    shutting_down: bool,
}

impl<T> QueueState<T> {
    fn rebuild_heap(&mut self) {
        let generations = self.generations;
        self.heap = self
            .queued
            .values()
            .filter_map(|job| {
                generations
                    .rank(job.key.generation)
                    .map(|generation_rank| HeapEntry {
                        id: job.id,
                        key: job.key.clone(),
                        generation_rank,
                        class_rank: job.class.rank(),
                        distance: job.distance,
                        order: job.order,
                    })
            })
            .collect();
    }

    fn has_live_running(&self, key: &DecodeJobKey) -> bool {
        self.running
            .values()
            .any(|job| &job.key == key && !job.cancelled.load(AtomicOrdering::Acquire))
    }

    fn pop_next(&mut self) -> Option<ScheduledDecodeJob<T>> {
        let visible_running = self.running.values().any(|job| job.class == PagePriorityClass::Visible);
        let speculative_running = self.running.values()
            .filter(|job| job.class != PagePriorityClass::Visible)
            .count();
        let mut blocked = Vec::new();
        while let Some(entry) = self.heap.pop() {
            let Some(job) = self.queued.get(&entry.key) else {
                continue;
            };
            if job.id != entry.id || job.cancelled.load(AtomicOrdering::Acquire) {
                self.queued.remove(&entry.key);
                continue;
            }
            if !self.generations.accepts(job.key.generation) {
                job.cancelled.store(true, AtomicOrdering::Release);
                self.queued.remove(&entry.key);
                continue;
            }
            let speculative = job.class != PagePriorityClass::Visible;
            if speculative
                && (visible_running || speculative_running >= self.max_speculative_running)
            {
                blocked.push(entry);
                continue;
            }
            let job = self.queued.remove(&entry.key).unwrap();
            let scheduled = ScheduledDecodeJob {
                id: job.id,
                key: job.key.clone(),
                payload: job.payload,
                cancelled: Arc::clone(&job.cancelled),
            };
            self.running.insert(
                job.id,
                RunningJob {
                    key: job.key,
                    class: job.class,
                    cancelled: job.cancelled,
                },
            );
            self.heap.extend(blocked);
            return Some(scheduled);
        }
        self.heap.extend(blocked);
        None
    }
}

struct QueueShared<T> {
    state: Mutex<QueueState<T>>,
    wake: Condvar,
}

pub struct DecodeJobQueue<T> {
    shared: Arc<QueueShared<T>>,
}

impl<T> Clone for DecodeJobQueue<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> DecodeJobQueue<T> {
    pub fn new(active_generation: u64) -> Self {
        Self {
            shared: Arc::new(QueueShared {
                state: Mutex::new(QueueState {
                    generations: AcceptedGenerations {
                        active: active_generation,
                        preparing: None,
                    },
                    queued: HashMap::new(),
                    heap: BinaryHeap::new(),
                    running: HashMap::new(),
                    max_speculative_running: 1,
                    next_id: 0,
                    next_order: 0,
                    shutting_down: false,
                }),
                wake: Condvar::new(),
            }),
        }
    }

    /// Ahead/Behindの最大同時実行数を変更する。実行中ジョブは止めず、以後の開始数を制限する。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_max_speculative_running(&self, max: usize) {
        let mut state = self.shared.state.lock().unwrap();
        state.max_speculative_running = max;
        self.shared.wake.notify_all();
    }

    /// active/preparing以外の待機ジョブを削除し、実行中ジョブには停止信号を立てる。
    /// preparingをactiveへ昇格した場合、その世代のジョブは停止せず継続する。
    pub fn set_generations(&self, active: u64, preparing: Option<u64>) {
        let mut state = self.shared.state.lock().unwrap();
        state.generations = AcceptedGenerations { active, preparing };
        let generations = state.generations;

        state.queued.retain(|key, job| {
            let keep = generations.accepts(key.generation);
            if !keep {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
            keep
        });
        for job in state.running.values() {
            if !generations.accepts(job.key.generation) {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
        }
        state.rebuild_heap();
        self.shared.wake.notify_all();
    }

    /// 既存の必要集合を維持したまま1件を追加・更新する。
    /// フェーズ2の既存prefetch経路と、FileCache待ち解除後の遅延投入に使用する。
    pub fn submit(&self, desired_job: DesiredDecodeJob<T>) -> bool {
        let mut state = self.shared.state.lock().unwrap();
        if state.shutting_down
            || !state.generations.accepts(desired_job.key.generation)
            || state.has_live_running(&desired_job.key)
        {
            return false;
        }

        let key = desired_job.key.clone();
        if let Some(queued) = state.queued.get_mut(&key) {
            queued.class = desired_job.class;
            queued.distance = desired_job.distance;
            queued.payload = desired_job.payload;
        } else {
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            let order = state.next_order;
            state.next_order = state.next_order.wrapping_add(1);
            state.queued.insert(
                key.clone(),
                QueuedJob {
                    id,
                    key,
                    class: desired_job.class,
                    distance: desired_job.distance,
                    order,
                    payload: desired_job.payload,
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        state.rebuild_heap();
        self.shared.wake.notify_one();
        true
    }

    pub fn contains(&self, key: &DecodeJobKey) -> bool {
        let state = self.shared.state.lock().unwrap();
        state.queued.contains_key(key)
            || state
                .running
                .values()
                .any(|job| &job.key == key && !job.cancelled.load(AtomicOrdering::Acquire))
    }

    /// 指定集合から外れた待機ジョブを削除し、実行中ジョブには停止信号を立てる。
    /// payloadを作り直さず、ページ移動や先読み幅縮小だけを反映するための軽量経路。
    pub fn retain_desired_keys(&self, desired: &HashSet<DecodeJobKey>) {
        let mut state = self.shared.state.lock().unwrap();
        state.queued.retain(|key, job| {
            let keep = desired.contains(key);
            if !keep {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
            keep
        });
        for job in state.running.values() {
            if !desired.contains(&job.key) {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
        }
        state.rebuild_heap();
    }

    /// 現在必要なジョブ集合で待機キューを置き換える。
    /// 対象外になった実行中ジョブは止めず、完了結果を不採用にする停止信号だけを立てる。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn replace_desired(&self, desired: Vec<DesiredDecodeJob<T>>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.shutting_down {
            return;
        }

        let mut desired_by_key: HashMap<DecodeJobKey, DesiredDecodeJob<T>> = desired
            .into_iter()
            .filter(|job| state.generations.accepts(job.key.generation))
            .map(|job| (job.key.clone(), job))
            .collect();

        state.queued.retain(|key, job| {
            let keep = desired_by_key.contains_key(key);
            if !keep {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
            keep
        });
        for job in state.running.values() {
            if !desired_by_key.contains_key(&job.key) {
                job.cancelled.store(true, AtomicOrdering::Release);
            }
        }

        for (key, desired_job) in desired_by_key.drain() {
            if let Some(queued) = state.queued.get_mut(&key) {
                queued.class = desired_job.class;
                queued.distance = desired_job.distance;
                queued.payload = desired_job.payload;
                continue;
            }
            if state.has_live_running(&key) {
                continue;
            }

            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            let order = state.next_order;
            state.next_order = state.next_order.wrapping_add(1);
            state.queued.insert(
                key.clone(),
                QueuedJob {
                    id,
                    key,
                    class: desired_job.class,
                    distance: desired_job.distance,
                    order,
                    payload: desired_job.payload,
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
            );
        }

        state.rebuild_heap();
        self.shared.wake.notify_all();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn try_take(&self) -> Option<ScheduledDecodeJob<T>> {
        self.shared.state.lock().unwrap().pop_next()
    }

    /// ワーカー用の待機取得。shutdown後、待機キューが空ならNoneを返す。
    pub fn wait_take(&self) -> Option<ScheduledDecodeJob<T>> {
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(job) = state.pop_next() {
                return Some(job);
            }
            if state.shutting_down {
                return None;
            }
            state = self.shared.wake.wait(state).unwrap();
        }
    }

    /// 成功・失敗を問わず実行中管理を解除し、結果を採用してよい場合だけtrueを返す。
    pub fn finish(&self, id: u64) -> bool {
        let mut state = self.shared.state.lock().unwrap();
        let Some(job) = state.running.remove(&id) else {
            return false;
        };
        let accepted = !job.cancelled.load(AtomicOrdering::Acquire)
            && state.generations.accepts(job.key.generation)
        ;
        self.shared.wake.notify_all();
        accepted
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending_count(&self) -> usize {
        let state = self.shared.state.lock().unwrap();
        let running = state
            .running
            .values()
            .filter(|job| !job.cancelled.load(AtomicOrdering::Acquire))
            .count();
        state.queued.len() + running
    }

    pub fn shutdown(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.shutting_down = true;
        for job in state.queued.values() {
            job.cancelled.store(true, AtomicOrdering::Release);
        }
        state.queued.clear();
        state.heap.clear();
        for job in state.running.values() {
            job.cancelled.store(true, AtomicOrdering::Release);
        }
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn job(
        generation: u64,
        page: usize,
        class: PagePriorityClass,
        distance: usize,
    ) -> DesiredDecodeJob<usize> {
        DesiredDecodeJob {
            key: DecodeJobKey {
                archive_path: PathBuf::from("book.zip"),
                page_index: page,
                generation,
            },
            class,
            distance,
            payload: page,
        }
    }

    fn take_pages(queue: &DecodeJobQueue<usize>) -> Vec<usize> {
        let mut pages = Vec::new();
        while let Some(job) = queue.try_take() {
            pages.push(job.key.page_index);
            assert!(queue.finish(job.id));
        }
        pages
    }

    #[test]
    fn orders_visible_then_ahead_near_to_far_then_behind_near_to_far() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 98, PagePriorityClass::Behind, 2),
            job(1, 102, PagePriorityClass::Ahead, 2),
            job(1, 99, PagePriorityClass::Behind, 1),
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
        ]);
        assert_eq!(take_pages(&queue), vec![100, 101, 102, 99, 98]);
    }

    #[test]
    fn preparing_generation_precedes_active_generation() {
        let queue = DecodeJobQueue::new(10);
        queue.set_generations(10, Some(11));
        queue.replace_desired(vec![
            job(10, 100, PagePriorityClass::Visible, 0),
            job(11, 101, PagePriorityClass::Behind, 10),
        ]);
        let first = queue.try_take().unwrap();
        assert_eq!(first.key.generation, 11);
    }

    #[test]
    fn duplicate_key_is_queued_once() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Ahead, 1),
            job(1, 100, PagePriorityClass::Visible, 0),
        ]);
        assert_eq!(queue.pending_count(), 1);
        assert_eq!(take_pages(&queue), vec![100]);
    }

    #[test]
    fn individual_submit_keeps_existing_jobs_and_updates_priority() {
        let queue = DecodeJobQueue::new(1);
        assert!(queue.submit(job(1, 99, PagePriorityClass::Behind, 1)));
        assert!(queue.submit(job(1, 101, PagePriorityClass::Ahead, 1)));
        assert!(queue.submit(job(1, 100, PagePriorityClass::Visible, 0)));
        assert_eq!(take_pages(&queue), vec![100, 101, 99]);
    }

    #[test]
    fn widening_and_shrinking_replace_only_the_desired_waiting_set() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
        ]);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
            job(1, 102, PagePriorityClass::Ahead, 2),
        ]);
        assert_eq!(queue.pending_count(), 3);

        queue.replace_desired(vec![job(1, 100, PagePriorityClass::Visible, 0)]);
        assert_eq!(take_pages(&queue), vec![100]);
    }

    #[test]
    fn retaining_desired_keys_cancels_removed_waiting_and_running_jobs() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
            job(1, 99, PagePriorityClass::Behind, 1),
        ]);
        let running = queue.try_take().unwrap();
        assert_eq!(running.key.page_index, 100);

        let keep = HashSet::from([DecodeJobKey {
            archive_path: PathBuf::from("book.zip"),
            page_index: 101,
            generation: 1,
        }]);
        queue.retain_desired_keys(&keep);

        assert!(running.is_cancelled());
        // キャンセル済みでも実処理はfinishまでCPUを使い続けるため、先読み枠を空けない。
        assert!(queue.try_take().is_none());
        assert!(!queue.finish(running.id));
        assert_eq!(take_pages(&queue), vec![101]);
    }

    #[test]
    fn changing_position_reprioritizes_existing_waiting_jobs() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
        ]);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Behind, 1),
            job(1, 101, PagePriorityClass::Visible, 0),
        ]);
        assert_eq!(take_pages(&queue), vec![101, 100]);
    }

    #[test]
    fn speculative_job_waits_until_visible_job_finishes() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 100, PagePriorityClass::Visible, 0),
            job(1, 101, PagePriorityClass::Ahead, 1),
        ]);

        let visible = queue.try_take().unwrap();
        assert_eq!(visible.key.page_index, 100);
        assert!(queue.try_take().is_none());

        assert!(queue.finish(visible.id));
        let ahead = queue.try_take().unwrap();
        assert_eq!(ahead.key.page_index, 101);
        assert!(queue.finish(ahead.id));
    }

    #[test]
    fn speculative_parallelism_is_runtime_configurable() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 101, PagePriorityClass::Ahead, 1),
            job(1, 102, PagePriorityClass::Ahead, 2),
        ]);

        let first = queue.try_take().unwrap();
        assert!(queue.try_take().is_none());

        queue.set_max_speculative_running(2);
        let second = queue.try_take().unwrap();
        assert_ne!(first.key.page_index, second.key.page_index);
        assert!(queue.finish(first.id));
        assert!(queue.finish(second.id));
    }

    #[test]
    fn queued_visible_job_bypasses_a_saturated_speculative_lane() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![
            job(1, 101, PagePriorityClass::Ahead, 1),
            job(1, 102, PagePriorityClass::Ahead, 2),
        ]);
        let ahead = queue.try_take().unwrap();
        assert!(queue.submit(job(1, 100, PagePriorityClass::Visible, 0)));

        let visible = queue.try_take().unwrap();
        assert_eq!(visible.key.page_index, 100);
        assert!(queue.finish(visible.id));
        assert!(queue.finish(ahead.id));
    }

    #[test]
    fn invalidated_waiting_generation_is_removed() {
        let queue = DecodeJobQueue::new(10);
        queue.set_generations(10, Some(11));
        queue.replace_desired(vec![
            job(10, 100, PagePriorityClass::Visible, 0),
            job(11, 100, PagePriorityClass::Visible, 0),
        ]);
        queue.set_generations(11, Some(12));
        assert_eq!(queue.pending_count(), 1);
        let remaining = queue.try_take().unwrap();
        assert_eq!(remaining.key.generation, 11);
    }

    #[test]
    fn promoted_preparing_generation_keeps_running() {
        let queue = DecodeJobQueue::new(10);
        queue.set_generations(10, Some(11));
        queue.replace_desired(vec![job(11, 100, PagePriorityClass::Visible, 0)]);
        let running = queue.try_take().unwrap();
        queue.set_generations(11, Some(12));
        assert!(!running.is_cancelled());
        assert!(queue.finish(running.id));
    }

    #[test]
    fn invalidated_running_job_finishes_as_discarded() {
        let queue = DecodeJobQueue::new(10);
        queue.replace_desired(vec![job(10, 100, PagePriorityClass::Visible, 0)]);
        let running = queue.try_take().unwrap();
        queue.set_generations(11, None);
        assert!(running.is_cancelled());
        assert!(!queue.finish(running.id));
    }

    #[test]
    fn no_longer_desired_running_job_is_cancelled_and_may_be_requeued() {
        let queue = DecodeJobQueue::new(1);
        queue.replace_desired(vec![job(1, 100, PagePriorityClass::Visible, 0)]);
        let old = queue.try_take().unwrap();
        queue.replace_desired(Vec::new());
        assert!(old.is_cancelled());

        queue.replace_desired(vec![job(1, 100, PagePriorityClass::Visible, 0)]);
        let replacement = queue.try_take().unwrap();
        assert_ne!(old.id, replacement.id);
        assert!(!queue.finish(old.id));
        assert!(queue.finish(replacement.id));
    }

    #[test]
    fn shutdown_wakes_a_waiting_worker() {
        let queue = DecodeJobQueue::<usize>::new(1);
        let worker_queue = queue.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(worker_queue.wait_take().is_none()).unwrap();
        });

        queue.shutdown();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }
}
