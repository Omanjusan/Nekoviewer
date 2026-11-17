//! ページ送りの待ち時間の計測（perf ログ専用）。
//!
//! ビューアーで「新しく表示されたページ」が、表示された時点で先読み済みだったか、
//! 未準備で待たされたか（待ち時間）を記録する。先読みの並列数を見直す価値があるかを、
//! 実機の連続ページ送りで判断するための材料。表示・デコードの挙動は変えない。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlipEvent {
    /// 表示された時点で、すでに表示できる状態だった（先読み済み）。
    Ready { index: usize },
    /// 未準備で表示され、あとから届いた。`waited` は表示された時点から届くまでの時間。
    Arrived { index: usize, waited: Duration },
    /// 未準備のまま、届く前に別のページへ移った。
    Abandoned { index: usize, waited: Duration },
}

impl FlipEvent {
    /// perf ログ用の1行（先頭の `[perf/open]` は呼び出し側が付ける）。
    pub fn describe(&self) -> String {
        match *self {
            FlipEvent::Ready { index } => format!("ページ送り index={index} 先読み済み"),
            FlipEvent::Arrived { index, waited } => format!("ページ送り待ち index={index} 待ち={:.0}ms", waited.as_secs_f64() * 1000.0),
            FlipEvent::Abandoned { index, waited } => format!("ページ送り待ち index={index} 到着前に移動 待ち={:.0}ms", waited.as_secs_f64() * 1000.0),
        }
    }
}

/// 新しく見えたページを追跡する。ページは元の通し番号（original_index）で数える。
#[derive(Default)]
pub struct FlipTracker {
    /// 直前のフレームで表示されていたページ。
    seen: HashSet<usize>,
    /// 未準備で表示され、まだ届いていないページと、その表示開始時刻。
    waiting: HashMap<usize, Instant>,
}

impl FlipTracker {
    /// フレームの最初に呼ぶ。`visible` はいま表示中のページ。`is_ready` は「いま表示できるか」
    /// （テクスチャがあるか、ページキャッシュに結果があってこのフレームで上げられるか）。
    pub fn begin_frame(&mut self, visible: &[usize], is_ready: &dyn Fn(usize) -> bool, now: Instant) -> Vec<FlipEvent> {
        let mut events = Vec::new();
        let visible_set: HashSet<usize> = visible.iter().copied().collect();

        // 待っていたのに、届く前に見えなくなった。
        let gone: Vec<usize> = self.waiting.keys().copied().filter(|i| !visible_set.contains(i)).collect();
        for index in gone {
            if let Some(since) = self.waiting.remove(&index) {
                events.push(FlipEvent::Abandoned { index, waited: now.saturating_duration_since(since) });
            }
        }
        // 新しく見えたページ。
        for &index in visible {
            if self.seen.contains(&index) {
                continue;
            }
            if is_ready(index) {
                events.push(FlipEvent::Ready { index });
            } else {
                self.waiting.insert(index, now);
            }
        }
        self.seen = visible_set;
        events
    }

    /// フレームの最後に呼ぶ（テクスチャのアップロード後）。待っていたページが表示できるようになっていれば届いた扱い。
    pub fn end_frame(&mut self, is_ready: &dyn Fn(usize) -> bool, now: Instant) -> Vec<FlipEvent> {
        let arrived: Vec<usize> = self.waiting.keys().copied().filter(|&i| is_ready(i)).collect();
        arrived
            .into_iter()
            .filter_map(|index| {
                self.waiting.remove(&index).map(|since| FlipEvent::Arrived { index, waited: now.saturating_duration_since(since) })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn prefetched_page_is_reported_ready_once_then_not_again() {
        let base = Instant::now();
        let mut t = FlipTracker::default();
        let ready = |_: usize| true;
        assert_eq!(t.begin_frame(&[3], &ready, at(base, 0)), vec![FlipEvent::Ready { index: 3 }]);
        // 同じページが見え続けている間は、何も出さない
        assert!(t.begin_frame(&[3], &ready, at(base, 16)).is_empty());
        // 次のページへ（見開き: 3 と 4 のうち 4 が新しい）
        assert_eq!(t.begin_frame(&[3, 4], &ready, at(base, 32)), vec![FlipEvent::Ready { index: 4 }]);
    }

    #[test]
    fn unready_page_reports_wait_when_it_arrives() {
        let base = Instant::now();
        let mut t = FlipTracker::default();
        assert!(t.begin_frame(&[5], &|_| false, at(base, 0)).is_empty());
        assert!(t.end_frame(&|_| false, at(base, 50)).is_empty(), "まだ届いていない");
        let ev = t.end_frame(&|_| true, at(base, 120));
        assert_eq!(ev, vec![FlipEvent::Arrived { index: 5, waited: Duration::from_millis(120) }]);
        // 届いたあとは二度と報告しない
        assert!(t.end_frame(&|_| true, at(base, 200)).is_empty());
        assert!(t.begin_frame(&[5], &|_| true, at(base, 216)).is_empty());
    }

    #[test]
    fn page_left_before_arrival_is_reported_as_abandoned() {
        let base = Instant::now();
        let mut t = FlipTracker::default();
        t.begin_frame(&[7], &|_| false, at(base, 0));
        let ev = t.begin_frame(&[8], &|i| i == 8, at(base, 80));
        assert_eq!(
            ev,
            vec![FlipEvent::Abandoned { index: 7, waited: Duration::from_millis(80) }, FlipEvent::Ready { index: 8 }]
        );
        // 放棄したページは、もう待たない
        assert!(t.end_frame(&|_| true, at(base, 100)).is_empty());
    }

    #[test]
    fn returning_to_a_page_counts_as_a_new_flip() {
        let base = Instant::now();
        let mut t = FlipTracker::default();
        t.begin_frame(&[1], &|_| true, at(base, 0));
        t.begin_frame(&[2], &|_| true, at(base, 16));
        // 戻る: 1 は「新しく見えた」扱いで、また報告される
        assert_eq!(t.begin_frame(&[1], &|_| true, at(base, 32)), vec![FlipEvent::Ready { index: 1 }]);
    }

    #[test]
    fn describe_lines_are_readable() {
        assert_eq!(FlipEvent::Ready { index: 4 }.describe(), "ページ送り index=4 先読み済み");
        let w = FlipEvent::Arrived { index: 9, waited: Duration::from_millis(123) };
        assert_eq!(w.describe(), "ページ送り待ち index=9 待ち=123ms");
        let a = FlipEvent::Abandoned { index: 2, waited: Duration::from_millis(40) };
        assert_eq!(a.describe(), "ページ送り待ち index=2 到着前に移動 待ち=40ms");
    }
}
