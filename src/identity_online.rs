//! ボリュームがオンラインだった日の記録（未確認IDの保持日数を「オンラインだった日数」で数えるため）。
//!
//! 外付けHDDやUSBを長期間つながないと、中身が無事でも「見つからないまま日数が過ぎた」ことになり、
//! 評価やお気に入りが消えてしまう。それを避けるため、アプリが動いていてボリュームが見えていた日
//! （UTC日）だけを数える。アプリを起動していない日は数えない（消えにくい側に倒れる）。
//!
//! - 記録: `volume_online_days_v1`。キー = (ボリューム, 日番号)、値なし。同じ日は何度書いても1行。
//!   最初に必要になるまで作らない。400日より古い行は書き込みのついでに消す
//! - 経過日数: ID の `last_seen`（未確認になった時刻）の翌日から今日までに、オンラインだった日の数
//!   （当日は数えない。保持日数 N なら、N 日分のオンラインを重ねるまで消えない）

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, TableError};

use crate::file_identity::all_records_tx;

const ONLINE_DAYS_TABLE: TableDefinition<(&str, i64), ()> = TableDefinition::new("volume_online_days_v1");
const DAY_SECS: i64 = 86_400;
/// この日数より古い記録は消す（保持日数の上限120日に、十分な余裕を足した値）。
const KEEP_DAYS: i64 = 400;

/// unix秒 → 日番号（UTC）。
pub fn day_of(unix: i64) -> i64 {
    unix.div_euclid(DAY_SECS)
}

/// 今日、オンラインだったボリュームを記録する。記録したボリュームの数を返す。
/// 対象のボリュームは、IDレコードが持つボリューム（マウントルート）。空のボリュームは対象外。
pub fn mark_online_volumes_with(db: &Arc<Mutex<Database>>, now: i64, online: &dyn Fn(&str) -> bool) -> usize {
    let volumes: Vec<String> = {
        let Ok(guard) = db.lock() else { return 0 };
        let Ok(tx) = guard.begin_read() else { return 0 };
        let mut v: Vec<String> =
            all_records_tx(&tx).into_iter().map(|r| r.volume).filter(|v| !v.is_empty()).collect();
        v.sort();
        v.dedup();
        v
    };
    // ボリュームの確認（ネットワークドライブでは遅いことがある）はロックの外で行う。
    let online_volumes: Vec<&String> = volumes.iter().filter(|v| online(v)).collect();
    let today = day_of(now);
    let Ok(guard) = db.lock() else { return 0 };
    let Ok(tx) = guard.begin_write() else { return 0 };
    let marked = {
        let Ok(mut table) = tx.open_table(ONLINE_DAYS_TABLE) else { return 0 };
        for v in &online_volumes {
            if table.insert((v.as_str(), today), ()).is_err() {
                return 0;
            }
        }
        let old: Vec<(String, i64)> = table
            .iter()
            .map(|it| {
                it.flatten()
                    .map(|(k, _)| {
                        let (v, d) = k.value();
                        (v.to_owned(), d)
                    })
                    .filter(|(_, d)| *d < today - KEEP_DAYS)
                    .collect()
            })
            .unwrap_or_default();
        for (v, d) in &old {
            let _ = table.remove((v.as_str(), *d));
        }
        online_volumes.len()
    };
    if tx.commit().is_err() {
        return 0;
    }
    marked
}

/// ボリューム別の「オンラインだった日」の一覧（読み取り用のスナップショット）。
#[derive(Debug, Default, Clone)]
pub struct OnlineDays {
    days: HashMap<String, Vec<i64>>,
}

impl OnlineDays {
    pub fn load(db: &Arc<Mutex<Database>>) -> Self {
        let mut days: HashMap<String, Vec<i64>> = HashMap::new();
        let Ok(guard) = db.lock() else { return Self::default() };
        let Ok(tx) = guard.begin_read() else { return Self::default() };
        let table = match tx.open_table(ONLINE_DAYS_TABLE) {
            Ok(t) => t,
            Err(TableError::TableDoesNotExist(_)) => return Self::default(),
            Err(_) => return Self::default(),
        };
        let Ok(iter) = table.iter() else { return Self::default() };
        for (k, _) in iter.flatten() {
            let (v, d) = k.value();
            days.entry(v.to_owned()).or_default().push(d);
        }
        for list in days.values_mut() {
            list.sort_unstable();
        }
        Self { days }
    }

    /// `last_seen`（翌日以降）から `now`（当日まで）の間に、ボリュームがオンラインだった日の数。
    pub fn since(&self, volume: &str, last_seen: i64, now: i64) -> i64 {
        let Some(list) = self.days.get(volume) else { return 0 };
        let (from, to) = (day_of(last_seen) + 1, day_of(now));
        if to < from {
            return 0;
        }
        let lo = list.partition_point(|d| *d < from);
        let hi = list.partition_point(|d| *d <= to);
        (hi - lo) as i64
    }

    #[cfg(test)]
    pub fn from_days(volume: &str, days: &[i64]) -> Self {
        let mut map = HashMap::new();
        map.insert(volume.to_owned(), days.to_vec());
        Self { days: map }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::file_identity::ensure_record;
    use crate::spread_state::open_spread_db;

    struct Env {
        root: PathBuf,
        db: Arc<Mutex<Database>>,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_identity_online_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            let db = open_spread_db(&root).unwrap();
            Self { root, db }
        }

        /// 実ファイルのIDを作る（ボリュームは実際のマウントルート）。そのボリュームの文字列を返す。
        fn volume(&self) -> String {
            let p = self.root.join("a.zip");
            std::fs::write(&p, vec![1u8; 3000]).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
            ensure_record(&self.db, &self.root, "a.zip").unwrap().volume
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const D: i64 = DAY_SECS;

    #[test]
    fn since_counts_online_days_after_last_seen_up_to_today() {
        let od = OnlineDays::from_days("v", &[10, 11, 13, 20]);
        // last_seen が10日目 → 11日目から数える（当日は数えない）。
        assert_eq!(od.since("v", 10 * D + 5, 20 * D), 3);
        assert_eq!(od.since("v", 10 * D, 13 * D + 100), 2);
        assert_eq!(od.since("v", 9 * D, 20 * D), 4);
        // 今日が last_seen と同じ日、または過去なら0。
        assert_eq!(od.since("v", 20 * D, 20 * D), 0);
        assert_eq!(od.since("v", 25 * D, 20 * D), 0);
        // 知らないボリュームは0（消えない側）。
        assert_eq!(od.since("other", 0, 100 * D), 0);
    }

    #[test]
    fn marking_records_only_online_volumes_once_per_day() {
        let env = Env::new("mark");
        let vol = env.volume();
        let now = 100 * D + 123;
        assert_eq!(mark_online_volumes_with(&env.db, now, &|_| true), 1);
        assert_eq!(mark_online_volumes_with(&env.db, now + 3600, &|_| true), 1, "同じ日は1行のまま");
        let od = OnlineDays::load(&env.db);
        assert_eq!(od.since(&vol, 99 * D, 100 * D), 1);
        // 翌日はオフライン、翌々日はオンライン。
        assert_eq!(mark_online_volumes_with(&env.db, now + D, &|_| false), 0);
        assert_eq!(mark_online_volumes_with(&env.db, now + 2 * D, &|_| true), 1);
        let od = OnlineDays::load(&env.db);
        assert_eq!(od.since(&vol, 99 * D, 102 * D), 2, "オフラインの日は数えない");
    }

    #[test]
    fn nothing_is_created_before_any_volume_is_online() {
        let env = Env::new("lazy");
        env.volume();
        assert_eq!(mark_online_volumes_with(&env.db, 100 * D, &|_| false), 0);
        assert_eq!(OnlineDays::load(&env.db).since("anything", 0, 1000 * D), 0);
    }

    #[test]
    fn rows_older_than_the_keep_window_are_pruned() {
        let env = Env::new("prune");
        let vol = env.volume();
        mark_online_volumes_with(&env.db, 100 * D, &|_| true);
        mark_online_volumes_with(&env.db, (100 + KEEP_DAYS + 5) * D, &|_| true);
        let od = OnlineDays::load(&env.db);
        assert_eq!(od.since(&vol, 0, (100 + KEEP_DAYS + 5) * D), 1, "古い日は消え、新しい日だけ残る");
    }
}
