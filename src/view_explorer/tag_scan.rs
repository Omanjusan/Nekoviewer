//! 「現フォルダ内タグ情報取得」の走査ワーカー。表示中の実フォルダ直下のアーカイブを背景スレッドで1件ずつ調べ、
//! 次の情報をメモリに集める（ディスクには何も書かない）。
//!
//! - 形式タグの元（拡張子。フォルダ本は「フォルダ」）
//! - info.txt の有無と、あればその項目（zip/cbz のみ。ほかの形式は「対象外」）
//! - いま付いているタグ（追加のみのマージと、新規タグ数の見積もりに使う）
//!
//! 進捗は件数、キャンセルは次の1件の前に効く。結果は `NekoviewApp::tag_scan_result` に置き、
//! 候補の生成・確認ウィンドウ・適用（後続の単位）がそこから読む。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::Database;

use crate::i18n;
use crate::tag_source::{format_tag_for, parse_info_txt, FormatTag, InfoTxt};

use super::NekoviewApp;

/// info.txt の調査結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InfoState {
    /// zip/cbz 以外（または zip として開けない）。info.txt は調べていない。
    NotApplicable,
    /// zip として開けたが info.txt が無い。
    Absent,
    /// info.txt があり、解析した。
    Found(InfoTxt),
}

/// 1ファイルぶんの取得結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScanItem {
    pub path: PathBuf,
    pub format: Option<FormatTag>,
    pub info: InfoState,
    /// 走査時点で付いているタグ（tier_id）。
    pub existing_tags: Vec<u64>,
}

/// 走査の結果。
#[derive(Debug)]
pub(super) struct TagScanResult {
    /// 走査したフォルダ（実フォルダ）。
    pub dir: PathBuf,
    pub items: Vec<ScanItem>,
    /// 途中でキャンセルされた（`items` は処理済みの分だけ）。
    pub cancelled: bool,
    /// 対象の総数（キャンセル時に何件中かを出すため）。
    pub total: usize,
}

impl TagScanResult {
    /// （info.txt あり, なし, 対象外）の件数。
    pub(super) fn info_counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for i in &self.items {
            match i.info {
                InfoState::Found(_) => c.0 += 1,
                InfoState::Absent => c.1 += 1,
                InfoState::NotApplicable => c.2 += 1,
            }
        }
        c
    }
}

/// 1件を調べる。ファイルが読めなくても落とさず、対象外として返す。
fn scan_one(path: &Path, db: Option<&Arc<Mutex<Database>>>) -> ScanItem {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let is_dir = path.is_dir();
    let format = format_tag_for(name, is_dir);
    let info = if is_dir {
        InfoState::NotApplicable
    } else {
        match crate::fs::archive::meta::read_info_txt(path) {
            None => InfoState::NotApplicable,
            Some(None) => InfoState::Absent,
            // 本文が空・読めない場合も、info.txt はあったが項目は無いものとして扱う。
            Some(Some(text)) => InfoState::Found(parse_info_txt(&text)),
        }
    };
    let existing_tags = match (db, path.parent()) {
        (Some(db), Some(dir)) if !name.is_empty() => crate::spread_state::read_archive_tags(db, dir, name),
        _ => Vec::new(),
    };
    ScanItem { path: path.to_path_buf(), format, info, existing_tags }
}

/// `targets` を順に調べる。`done` に処理済みの件数を反映し、`cancel` が立ったら打ち切る。
pub(super) fn run_scan(
    dir: PathBuf,
    targets: &[PathBuf],
    db: Option<&Arc<Mutex<Database>>>,
    done: &AtomicUsize,
    cancel: &AtomicBool,
) -> TagScanResult {
    let mut items = Vec::with_capacity(targets.len());
    let mut cancelled = false;
    for path in targets {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        items.push(scan_one(path, db));
        done.fetch_add(1, Ordering::Relaxed);
    }
    TagScanResult { dir, items, cancelled, total: targets.len() }
}

/// 走査中のジョブ（背景スレッド＋進捗＋キャンセル）。
pub(super) struct TagScanJob {
    total: usize,
    done: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<TagScanResult>,
}

impl TagScanJob {
    pub(super) fn spawn(dir: PathBuf, targets: Vec<PathBuf>, db: Option<Arc<Mutex<Database>>>) -> Self {
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let total = targets.len();
        let (w_dir, w_done, w_cancel) = (dir.clone(), Arc::clone(&done), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let result = run_scan(w_dir, &targets, db.as_ref(), &w_done, &w_cancel);
            // 受信側が破棄済み（アプリ終了）でもエラーは無視する。
            let _ = tx.send(result);
        });
        Self { total, done, cancel, rx }
    }

    fn progress(&self) -> (usize, usize) {
        (self.done.load(Ordering::Relaxed).min(self.total), self.total)
    }
}

impl NekoviewApp {
    /// 一括情報取得の対象（表示中の実フォルダ直下のアーカイブ。単体画像と無効アーカイブは除く）。
    fn tag_scan_targets(&self) -> Vec<PathBuf> {
        self.archives
            .iter()
            .filter(|p| !self.raw_image_files.contains(*p) && !self.invalid_archives.contains(*p))
            .cloned()
            .collect()
    }

    /// ボタンを押せるか。実フォルダ表示中（仮想・お気に入り・検索・類似・タグ検索を除く）で、対象があり、実行中でないとき。
    pub(super) fn can_start_tag_scan(&self) -> bool {
        self.tag_scan_job.is_none()
            && self.viewing_favorites.is_none()
            && self.viewing_similar.is_none()
            && self.viewing_search.is_none()
            && self.viewing_virtual_node.is_none()
            && self.folder_pane_tab != super::FolderPaneTab::TagSearch
            && !self.tag_scan_targets().is_empty()
    }

    pub(super) fn start_tag_scan(&mut self) {
        if !self.can_start_tag_scan() {
            return;
        }
        let targets = self.tag_scan_targets();
        self.tag_scan_result = None;
        self.tag_scan_job = Some(TagScanJob::spawn(self.current_dir.clone(), targets, self.spread_db.clone()));
    }

    /// 完了を確認する（毎フレーム呼んでよい）。完了したら結果を保持し、件数をトーストで知らせる。
    pub(super) fn poll_tag_scan(&mut self) {
        let Some(job) = self.tag_scan_job.as_ref() else { return };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.tag_scan_job = None;
                return;
            }
        };
        self.tag_scan_job = None;
        // 走査中に別フォルダへ移っていたら、結果は捨てる（別フォルダのタグ候補になってしまうため）。
        if result.dir != self.current_dir {
            return;
        }
        let t = i18n::t();
        if result.cancelled {
            self.set_toast(t.tag_scan_cancelled(result.items.len(), result.total));
        } else {
            let (found, absent, other) = result.info_counts();
            self.set_toast(t.tag_scan_done(result.items.len(), found, absent, other));
            self.tag_scan_result = Some(result);
        }
    }

    /// 走査中の進捗オーバーレイ（背後のマウス入力を遮断する。キャンセルボタン付き）。
    pub(super) fn draw_tag_scan_overlay(&mut self, ctx: &egui::Context) {
        let Some(job) = self.tag_scan_job.as_ref() else { return };
        let (done, total) = job.progress();
        let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        let mut cancel_clicked = false;
        egui::Modal::new(egui::Id::new("tag_scan_progress")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(280.0)
                        .text(i18n::t().tag_scan_progress(done, total)),
                );
                ui.add_space(12.0);
                if ui.button(i18n::t().archive_open_cancel()).clicked() {
                    cancel_clicked = true;
                }
            });
        });
        if cancel_clicked {
            job.cancel.store(true, Ordering::Relaxed);
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_tag_scan_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (n, d) in files {
            w.start_file(*n, ::zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn run(dir: &Path, targets: &[PathBuf]) -> TagScanResult {
        run_scan(dir.to_path_buf(), targets, None, &AtomicUsize::new(0), &AtomicBool::new(false))
    }

    #[test]
    fn scan_collects_format_info_and_marks_inapplicable_files() {
        let dir = temp_dir("collect");
        let with_info = write_zip(
            &dir,
            "(C105) [Hawk Bit (Kouji)] T [日本語].zip",
            &[("0001.jpg", b"x"), ("info.txt", "Title line\nCategory: Doujinshi\nLanguage: Japanese\n".as_bytes())],
        );
        let no_info = write_zip(&dir, "plain.CBZ", &[("0001.jpg", b"x")]);
        let not_zip = dir.join("broken.zip");
        std::fs::write(&not_zip, b"this is not a zip").unwrap();
        let book = dir.join("book_dir");
        std::fs::create_dir_all(&book).unwrap();
        let missing = dir.join("gone.cbz");

        let r = run(&dir, &[with_info.clone(), no_info.clone(), not_zip.clone(), book.clone(), missing.clone()]);
        assert!(!r.cancelled);
        assert_eq!(r.total, 5);
        assert_eq!(r.items.len(), 5, "順序と件数を保つ");
        assert_eq!(r.items[0].path, with_info);
        assert_eq!(r.items[0].format, Some(FormatTag::Extension("zip".into())));
        match &r.items[0].info {
            InfoState::Found(i) => {
                assert_eq!(i.category.as_deref(), Some("Doujinshi"));
                assert_eq!(i.info_language(), Some("日本語"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.items[1].format, Some(FormatTag::Extension("cbz".into())));
        assert_eq!(r.items[1].info, InfoState::Absent);
        assert_eq!(r.items[2].info, InfoState::NotApplicable, "zipとして開けない");
        assert_eq!(r.items[3].format, Some(FormatTag::Folder));
        assert_eq!(r.items[3].info, InfoState::NotApplicable);
        assert_eq!(r.items[4].info, InfoState::NotApplicable, "存在しないファイルでも落ちない");
        assert_eq!(r.info_counts(), (1, 1, 3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reports_progress_and_stops_on_cancel() {
        let dir = temp_dir("cancel");
        let targets: Vec<PathBuf> = (0..5).map(|i| write_zip(&dir, &format!("{i}.zip"), &[("a.jpg", b"x")])).collect();
        let done = AtomicUsize::new(0);
        let r = run_scan(dir.clone(), &targets, None, &done, &AtomicBool::new(false));
        assert_eq!((done.load(Ordering::Relaxed), r.items.len()), (5, 5));

        let done = AtomicUsize::new(0);
        let cancel = AtomicBool::new(true);
        let r = run_scan(dir.clone(), &targets, None, &done, &cancel);
        assert!(r.cancelled);
        assert!(r.items.is_empty());
        assert_eq!((done.load(Ordering::Relaxed), r.total), (0, 5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reads_existing_tags_from_the_spread_db() {
        let root = temp_dir("tags");
        let db = crate::spread_state::open_spread_db(&root).unwrap();
        let tagged = root.join("tagged.zip");
        std::fs::write(&tagged, (0..3000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        crate::file_identity::ensure_record(&db, &root, "tagged.zip").unwrap();
        assert!(crate::spread_state::write_archive_tags(&db, &root, "tagged.zip", &[7, 9]));
        let untagged = root.join("untagged.zip");
        std::fs::write(&untagged, (0..3100u32).map(|i| (i % 249) as u8).collect::<Vec<u8>>()).unwrap();
        crate::file_identity::ensure_record(&db, &root, "untagged.zip").unwrap();

        let r = run_scan(root.clone(), &[tagged, untagged], Some(&db), &AtomicUsize::new(0), &AtomicBool::new(false));
        assert_eq!(r.items[0].existing_tags, vec![7, 9]);
        assert!(r.items[1].existing_tags.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn job_runs_on_a_thread_and_delivers_the_result() {
        let dir = temp_dir("job");
        let t = write_zip(&dir, "a.zip", &[("info.txt", b"Category: Manga")]);
        let job = TagScanJob::spawn(dir.clone(), vec![t], None);
        let mut got = None;
        for _ in 0..300 {
            if let Ok(r) = job.rx.try_recv() {
                got = Some(r);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let r = got.expect("結果が届く");
        assert_eq!(r.info_counts(), (1, 0, 0));
        assert_eq!(job.progress(), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
