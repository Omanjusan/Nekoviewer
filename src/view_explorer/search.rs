use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::fs::dir;
use crate::neko_dir;

use super::{push_search_result, NekoviewApp, SearchFormState, SearchResultEntry};
use crate::i18n;

/// フォーム入力をパース済みにした検索条件。バックグラウンドスレッドへそのまま渡す
/// （String/PathBuf/数値のみで構成されるため Send）。
pub(super) struct SearchConditions {
    pub name_pattern: String,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub mtime_min: Option<i64>,
    pub mtime_max: Option<i64>,
}

/// SearchFormState の文字列群をパースする。数値・日付として読めない入力は
/// 無条件（None）として扱う（Phase3時点ではエラー表示までは行わない）。
pub(super) fn parse_search_form(form: &SearchFormState) -> SearchConditions {
    SearchConditions {
        name_pattern: form.name_pattern.clone(),
        size_min: parse_mb(&form.size_min_mb),
        size_max: parse_mb(&form.size_max_mb),
        mtime_min: parse_date(&form.date_after),
        mtime_max: parse_date(&form.date_before),
    }
}

fn parse_mb(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(|mb| (mb * 1024.0 * 1024.0) as u64)
}

/// "YYYY-MM-DD" を UTC 00:00:00 の Unix秒に変換する。chrono非依存
/// （panels.rs の format_mtime と対称。Howard Hinnant's civil_from_days の逆算）。
fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts[..] else { return None };
    let y: i64 = y.parse().ok()?;
    let m: i64 = m.parse().ok()?;
    let d: i64 = d.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86400)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// root配下のディレクトリを軽量列挙する（ディレクトリ名の列挙のみ、ファイルのstatはしない）。
/// include_subdirs=false なら root 自身のみ。
fn list_dirs_recursive(root: &Path, include_subdirs: bool) -> Vec<PathBuf> {
    if !include_subdirs {
        return vec![root.to_path_buf()];
    }
    let mut out = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in dir::list_subdirs(&dir) {
            out.push(child.clone());
            stack.push(child);
        }
    }
    out
}

/// 1ディレクトリ分の照会。対応する cache.redb が無ければ（＝サムネ未生成＝未索引）空を返す。
fn search_in_dir(dir: &Path, cache_root: &Path, conditions: &SearchConditions) -> Vec<PathBuf> {
    let neko_dir = neko_dir::neko_dir_for_root(dir, cache_root);
    let Some(db) = neko_dir::open_cache_db_if_exists(&neko_dir, dir) else {
        return Vec::new();
    };
    let pattern = conditions.name_pattern.as_str();
    let names = neko_dir::search_files(
        &db,
        |name| crate::fs::dir::name_matches(pattern, name),
        conditions.size_min,
        conditions.size_max,
        conditions.mtime_min,
        conditions.mtime_max,
    );
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// PWD配下の検索をバックグラウンドスレッドで実行する。findのような全ファイルstatは行わず、
/// サムネDBに既に記録済みのファイル索引だけを照会する（サムネ未取得ファイルはヒットしない）。
pub(super) fn spawn_search(
    root: PathBuf,
    include_subdirs: bool,
    cache_root: PathBuf,
    conditions: SearchConditions,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let dirs = list_dirs_recursive(&root, include_subdirs);
        let mut hits = Vec::new();
        for dir in dirs {
            hits.extend(search_in_dir(&dir, &cache_root, &conditions));
        }
        let _ = tx.send(hits);
        wake();
    });
    rx
}

impl NekoviewApp {
    /// 「検索開始」ボタンから呼ぶ。実行中（search_running）なら何もしない
    /// （多重実行不可、呼び出し側でもボタンを無効化済み）。
    pub(super) fn start_search(&mut self, ctx: &egui::Context) {
        if self.search_running {
            return;
        }
        let Some(cache_root) = self.config.cache_root() else { return };
        let conditions = parse_search_form(&self.search_form);
        let root = self.current_dir.clone();
        let include_subdirs = self.search_form.include_subdirs;
        let ctx = ctx.clone();
        self.search_pending = Some(spawn_search(root, include_subdirs, cache_root, conditions, move || ctx.request_repaint()));
        self.search_running = true;
    }

    /// 検索ワーカーの結果をポーリングする。完了したら履歴の先頭に追加し、
    /// その結果を選択状態にする（検索したらすぐ見えるように）。
    pub(super) fn poll_search(&mut self) {
        let Some(rx) = &self.search_pending else { return };
        let Ok(hits) = rx.try_recv() else { return };
        self.search_pending = None;
        self.search_running = false;
        let label = i18n::t().search_result_label(&self.search_form.name_pattern, hits.len());
        push_search_result(&mut self.search_history, SearchResultEntry { label, hits });
        self.search_selected = Some(0);
        self.enter_search_view(0);
    }

    /// 検索結果履歴の idx 番目を中央グリッドにフラット一覧として表示する
    /// （enter_favorite_view と同じ「archivesを差し替える」方式で、既存のグリッド描画・
    /// サムネ取得ワーカーをそのまま流用する）。
    pub(super) fn enter_search_view(&mut self, idx: usize) {
        let Some(entry) = self.search_history.get(idx) else { return };
        self.archives = entry.hits.clone();
        self.raw_image_files.clear();
        // 階層概念を持ち込まない平坦一覧という契約のため、サブフォルダ一覧も明示的に空にする
        // （grid_entries/draw_archive_grid側もviewing_searchをガードしているが二重の防御）。
        self.subdirs.clear();
        // 複数ディレクトリ横断のため単一ディレクトリ前提のキャッシュDB/セッション状態は無効化する
        self.cache_db = None;
        self.invalid_archives.clear();
        self.thumb_failed.clear();
        self.viewing_search = Some(idx);
        self.search_selected = Some(idx);
        self.sort_archives();
        self.recompute_filter();
        self.selected_archive_index = if self.archives.is_empty() { None } else { Some(0) };
        self.selected_archive_meta = None;
        self.multi_selected.clear();
        self.select_anchor = None;
    }

    /// 検索結果表示を終え、実ディレクトリ（current_dir）表示に戻す。
    pub(super) fn exit_search_view(&mut self) {
        if self.viewing_search.is_none() {
            return;
        }
        self.viewing_search = None;
        self.start_scan();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nekoviewer_search_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_date_matches_known_unix_seconds() {
        // date -u -d "YYYY-MM-DDT00:00:00Z" +%s と一致することを確認済みの既知値
        assert_eq!(parse_date("2024-06-01"), Some(1_717_200_000));
        assert_eq!(parse_date("2024-01-01"), Some(1_704_067_200));
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date("2024-13-01"), None, "月13は不正なのでNone");
    }

    #[test]
    fn parse_mb_converts_to_bytes() {
        assert_eq!(parse_mb("10"), Some(10 * 1024 * 1024));
        assert_eq!(parse_mb("0.5"), Some((0.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_mb(""), None);
        assert_eq!(parse_mb("abc"), None);
    }

    #[test]
    fn list_dirs_recursive_respects_include_subdirs_flag() {
        let root = unique_tmp("list_dirs");
        let sub1 = root.join("sub1");
        let sub2 = root.join("sub2");
        std::fs::create_dir_all(&sub1).unwrap();
        std::fs::create_dir_all(&sub2).unwrap();

        let shallow = list_dirs_recursive(&root, false);
        assert_eq!(shallow, vec![root.clone()]);

        let mut deep = list_dirs_recursive(&root, true);
        deep.sort();
        let mut expected = vec![root.clone(), sub1.clone(), sub2.clone()];
        expected.sort();
        assert_eq!(deep, expected);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_in_dir_finds_indexed_files_and_skips_unindexed_dirs() {
        let source_dir = unique_tmp("search_in_dir_source");
        let cache_root = unique_tmp("search_in_dir_cache");

        let neko_dir = neko_dir::neko_dir_for_root(&source_dir, &cache_root);
        let db = neko_dir::open_cache_db(&neko_dir, &source_dir).expect("db should open");
        neko_dir::write_file_record(&db, "a.zip", 0, 100);

        let conditions = SearchConditions {
            name_pattern: String::new(),
            size_min: None,
            size_max: None,
            mtime_min: None,
            mtime_max: None,
        };
        let hits = search_in_dir(&source_dir, &cache_root, &conditions);
        assert_eq!(hits, vec![source_dir.join("a.zip")]);

        // cache.redb が無いディレクトリ（未索引＝サムネ未取得）は空を返す
        let unindexed_dir = unique_tmp("search_in_dir_unindexed");
        let hits_unindexed = search_in_dir(&unindexed_dir, &cache_root, &conditions);
        assert!(hits_unindexed.is_empty());

        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&cache_root);
        let _ = std::fs::remove_dir_all(&unindexed_dir);
    }
}
