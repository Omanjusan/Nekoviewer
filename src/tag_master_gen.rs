//! タグマスター（`nekoviewer_tags.json`）の世代スナップショット。
//!
//! - 現世代は本物のファイルそのもの。履歴は「変更の直前の状態」のコピーで、`tag_master_gen/` に置く
//! - スナップショットは保存の直前に取る。タグ管理画面を開いている間は、最初の保存で1回だけ（呼び出し側が制御）。画面を開いていない間は、
//!   短い間隔の連続編集を1回にまとめる（デバウンス）。最新の履歴と内容が同じなら取らない
//! - 破壊的な操作（カテゴリ削除・復帰・試用の確定）の直前は、デバウンスを無視して取る（`force`）
//! - 世代数を超えた分は古い順に消す。最新の1件は消さない
//! - 復帰したマスターの `next_tier_id` は、現在の値と大きい方を取る。復帰で消えた要素の番号を、後から作る要素が
//!   再利用して、DBに残った古いタグ付けと誤って結び付くのを防ぐため
//!
//! 対象はマスターのJSONだけ。ファイルのタグ付け（DB）は含まない。

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::view_explorer::TagManagerCategoryUi;

const DIR_NAME: &str = "tag_master_gen";
const STEM: &str = "tags";
/// `YYYYMMDD-HHMMSS` の文字数。
const TIMESTAMP_LEN: usize = 15;
/// 連続編集を1回にまとめる間隔（秒）。直前のスナップショットからこの間は新しく取らない。
pub(crate) const DEBOUNCE_SECS: u64 = 60;
pub(crate) const KEEP_MIN: usize = 1;
pub(crate) const KEEP_MAX: usize = 10;

/// 世代1件の情報（一覧表示用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MasterGen {
    /// `tag_master_gen/` 内のファイル名。
    pub name: String,
    /// 更新日時（unix秒）。
    pub modified_unix: i64,
    pub size: u64,
}

fn gen_dir(root: &Path) -> PathBuf {
    root.join(DIR_NAME)
}

/// 保存の直前に呼ぶ。本物のファイル（変更前の状態）を履歴へコピーし、世代数を超えた古い分を消す。
/// 取らなかったとき（本物が無い・デバウンス中・直前と同じ内容）は None。
pub(crate) fn snapshot_before_save(root: &Path, keep: usize, force: bool) -> Option<MasterGen> {
    snapshot_with(root, keep, if force { 0 } else { DEBOUNCE_SECS })
}

fn snapshot_with(root: &Path, keep: usize, debounce_secs: u64) -> Option<MasterGen> {
    let live = std::fs::read(crate::tag_manager::real_path(root)).ok()?;
    let dir = gen_dir(root);
    let names = sorted_names(&dir);
    if let Some(newest) = names.last() {
        if debounce_secs > 0 {
            let recent = std::fs::metadata(dir.join(newest))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < Duration::from_secs(debounce_secs));
            if recent {
                return None;
            }
        }
        if std::fs::read(dir.join(newest)).is_ok_and(|prev| prev == live) {
            return None;
        }
    }
    std::fs::create_dir_all(&dir).ok()?;
    let dest = next_dest(&dir, &names);
    write_atomic(&dest, &live).ok()?;
    // 古い世代の削除に失敗しても、取った世代は有効（次回に持ち越す）。
    let _ = rotate(&dir, keep.clamp(KEEP_MIN, KEEP_MAX));
    let name = dest.file_name()?.to_string_lossy().into_owned();
    info(&dir, &name)
}

/// 世代の一覧（新しい順）。
pub(crate) fn list(root: &Path) -> Vec<MasterGen> {
    let dir = gen_dir(root);
    sorted_names(&dir).iter().rev().filter_map(|n| info(&dir, n)).collect()
}

/// 世代を読み、復帰後のマスターを返す。`next_tier_id` は、現在の値と世代の値の大きい方。
/// 名前が世代の形でない・ファイルが無い・壊れているときは None。
pub(crate) fn read_for_restore(root: &Path, name: &str, current_next_tier_id: u64) -> Option<(Vec<TagManagerCategoryUi>, u64)> {
    if !is_gen_name(name) {
        return None;
    }
    let (cats, next) = crate::tag_manager::load_path(&gen_dir(root).join(name))?;
    Some((cats, next.max(current_next_tier_id)))
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)
}

/// 古い順の世代ファイル名。
fn sorted_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_gen_name(n))
        .collect();
    names.sort_by_key(|n| sort_key(n));
    names
}

fn rotate(dir: &Path, keep: usize) -> io::Result<()> {
    let names = sorted_names(dir);
    let excess = names.len().saturating_sub(keep.max(1));
    for n in names.iter().take(excess) {
        std::fs::remove_file(dir.join(n))?;
    }
    Ok(())
}

/// `tags-{日時}.json` または `tags-{日時}-{連番}.json` の形か。パス区切りを含む名前は拒否する。
fn is_gen_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(STEM).and_then(|r| r.strip_prefix('-')) else { return false };
    let Some(body) = rest.strip_suffix(".json") else { return false };
    let (ts, tail) = body.split_at_checked(TIMESTAMP_LEN).unwrap_or((body, "x"));
    let ts_ok = ts.len() == TIMESTAMP_LEN
        && ts.chars().enumerate().all(|(i, c)| if i == 8 { c == '-' } else { c.is_ascii_digit() });
    let tail_ok = tail.is_empty()
        || tail.strip_prefix('-').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    ts_ok && tail_ok
}

/// 古い順に並べるキー（日時、同一秒内の連番）。
fn sort_key(name: &str) -> (String, u32) {
    let body = name
        .strip_prefix(STEM)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(".json"))
        .unwrap_or("");
    let (ts, tail) = body.split_at_checked(TIMESTAMP_LEN).unwrap_or((body, ""));
    let seq = tail.strip_prefix('-').and_then(|n| n.parse().ok()).unwrap_or(1);
    (ts.to_owned(), seq)
}

/// 次のファイル名。既存の最新より必ず後ろに並ぶ名前にする（時計が戻っていても、同じ秒でも）。
fn next_dest(dir: &Path, existing_sorted: &[String]) -> PathBuf {
    let now = timestamp_now();
    let (ts, seq) = match existing_sorted.last().map(|n| sort_key(n)) {
        Some((ts, seq)) if ts >= now => (ts, seq + 1),
        _ => (now, 1),
    };
    let name = if seq <= 1 { format!("{STEM}-{ts}.json") } else { format!("{STEM}-{ts}-{seq}.json") };
    dir.join(name)
}

fn timestamp_now() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn info(dir: &Path, name: &str) -> Option<MasterGen> {
    let meta = std::fs::metadata(dir.join(name)).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    Some(MasterGen { name: name.to_owned(), modified_unix, size: meta.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_manager::{save_slot, Slot};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_tag_gen_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 本物のマスターを、次のID違いで保存する（内容を変えるため）。
    fn save_real(root: &Path, next: u64) {
        let (cats, _) = crate::tag_manager::default_state();
        save_slot(root, Slot::Real, &cats, next);
    }

    #[test]
    fn name_shape_is_checked() {
        assert!(is_gen_name("tags-20261112-103000.json"));
        assert!(is_gen_name("tags-20261112-103000-2.json"));
        assert!(!is_gen_name("tags-20261112-103000.redb"));
        assert!(!is_gen_name("../tags-20261112-103000.json"));
        assert!(!is_gen_name("tags-20261112-1030.json"));
        assert!(!is_gen_name("tags-20261112-103000-.json"));
    }

    #[test]
    fn no_live_file_takes_no_snapshot() {
        let t = TempRoot::new("nolive");
        assert!(snapshot_before_save(&t.0, 5, true).is_none());
        assert!(list(&t.0).is_empty());
    }

    #[test]
    fn snapshot_holds_the_state_before_the_change() {
        let t = TempRoot::new("before");
        save_real(&t.0, 10);
        let g = snapshot_before_save(&t.0, 5, true).expect("taken");
        save_real(&t.0, 20);
        let (_, next) = read_for_restore(&t.0, &g.name, 0).unwrap();
        assert_eq!(next, 10, "履歴は変更前の状態");
    }

    #[test]
    fn identical_content_is_not_snapshotted_twice() {
        let t = TempRoot::new("same");
        save_real(&t.0, 10);
        assert!(snapshot_before_save(&t.0, 5, true).is_some());
        assert!(snapshot_before_save(&t.0, 5, true).is_none());
        assert_eq!(list(&t.0).len(), 1);
    }

    #[test]
    fn debounce_skips_a_burst_but_force_ignores_it() {
        let t = TempRoot::new("debounce");
        save_real(&t.0, 10);
        assert!(snapshot_before_save(&t.0, 5, false).is_some());
        save_real(&t.0, 11);
        assert!(snapshot_before_save(&t.0, 5, false).is_none(), "直後の編集はまとめる");
        assert!(snapshot_before_save(&t.0, 5, true).is_some(), "forceは間隔を無視");
        assert_eq!(list(&t.0).len(), 2);
    }

    #[test]
    fn rotation_keeps_newest_and_lists_newest_first() {
        let t = TempRoot::new("rotate");
        for n in 10..16 {
            save_real(&t.0, n);
            snapshot_before_save(&t.0, 3, true).expect("taken");
        }
        let gens = list(&t.0);
        assert_eq!(gens.len(), 3);
        let nexts: Vec<u64> = gens.iter().map(|g| read_for_restore(&t.0, &g.name, 0).unwrap().1).collect();
        assert_eq!(nexts, vec![15, 14, 13], "新しい順・古いものから消える");
        // 世代数は範囲に丸める（0でも最新1件は残る）。
        save_real(&t.0, 99);
        snapshot_before_save(&t.0, 0, true).unwrap();
        assert_eq!(list(&t.0).len(), 1);
    }

    #[test]
    fn restore_keeps_the_larger_next_tier_id() {
        let t = TempRoot::new("restore");
        save_real(&t.0, 10);
        let g = snapshot_before_save(&t.0, 5, true).unwrap();
        let (_, next) = read_for_restore(&t.0, &g.name, 50).unwrap();
        assert_eq!(next, 50, "現在の方が大きければ巻き戻さない");
        let (_, next) = read_for_restore(&t.0, &g.name, 3).unwrap();
        assert_eq!(next, 10);
        assert!(read_for_restore(&t.0, "../x.json", 0).is_none());
        assert!(read_for_restore(&t.0, "tags-20000101-000000.json", 0).is_none());
    }
}
