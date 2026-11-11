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
use crate::i18n::Lang;
use crate::tag_candidates::{build_candidates_full, CandidateInput, CandidateSet, Role};
use crate::tag_rules::TagRule;
use crate::tag_meta::ArchiveMeta;
use crate::tag_model::Model;
use crate::tag_similarity::MatchOptions;
use crate::tag_source::{format_tag_for, parse_info_txt, FormatTag, InfoTxt};
use crate::view_explorer::TagManagerCategoryUi;

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
    /// アーカイブ内の「正の情報」（ComicInfo.xml・meta.json）。どちらも無い・読めないときは None。
    pub meta: Option<ArchiveMeta>,
    /// 走査時点で付いているタグ（tier_id）。
    pub existing_tags: Vec<u64>,
    /// 詳細ファイル（XML・JSON・TXT）の確認結果。リストの「詳細」列に反映する。
    pub detail: crate::neko_dir::DetailFlag,
    /// zip で数えた画像の数。分からなければ None（設定でTXTもページに数えるときは、開いた時の数とずれるので None）。
    pub page_count: Option<usize>,
    /// 読む前のファイルの更新日時（秒）とサイズ。記録が現物と一致するかの判定に使う。
    pub mtime: i64,
    pub size: u64,
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
    /// 走査結果から作った候補（キャンセル時は None）。確認ウィンドウ・適用が読む。
    pub candidates: Option<CandidateSet>,
}

impl TagScanResult {
    /// （ComicInfo.xml あり, meta.json あり）の件数。
    pub(super) fn meta_counts(&self) -> (usize, usize) {
        let xml = self.items.iter().filter(|i| i.meta.as_ref().is_some_and(ArchiveMeta::has_xml)).count();
        let json = self.items.iter().filter(|i| i.meta.as_ref().is_some_and(ArchiveMeta::has_json)).count();
        (xml, json)
    }

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
    let (mtime, size) = (crate::neko_dir::file_mtime(path), crate::neko_dir::file_size(path));
    // zip/cbz は1回だけ開いて、info.txt・ComicInfo.xml・meta.json・付属テキストなどをまとめて読む。
    let bundle = if is_dir { None } else { crate::fs::archive::meta::read_bundle(path) };
    let sources = bundle.as_ref().map(|b| &b.tag);
    let detail = crate::neko_dir::DetailFlag::from_read(
        bundle.as_ref().map(|b| &b.detail),
        crate::fs::archive::meta::detail_supported(path),
    );
    let page_count = bundle
        .as_ref()
        .map(|b| b.image_count)
        .filter(|_| !crate::fs::archive::text_page::options().count_in_pages);
    let info = match sources {
        None => InfoState::NotApplicable,
        Some(s) => match &s.info_txt {
            None => InfoState::Absent,
            // 本文が空・読めない場合も、info.txt はあったが項目は無いものとして扱う。
            Some(text) => InfoState::Found(parse_info_txt(text)),
        },
    };
    let meta = sources.and_then(|s| ArchiveMeta::from_sources(s.comic_info.as_ref(), s.meta_json.as_deref()));
    let existing_tags = match (db, path.parent()) {
        (Some(db), Some(dir)) if !name.is_empty() => crate::spread_state::read_archive_tags(db, dir, name),
        _ => Vec::new(),
    };
    ScanItem { path: path.to_path_buf(), format, info, meta, existing_tags, detail, page_count, mtime, size }
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
    TagScanResult { dir, items, cancelled, total: targets.len(), candidates: None }
}

/// 走査結果から候補を作って結果に付ける（キャンセル済みなら何もしない）。
/// 重い照合になりうるので、画面のスレッドではなくワーカーの中で呼ぶ。
fn attach_candidates(result: &mut TagScanResult, master: &[TagManagerCategoryUi], opts: &MatchOptions, lang: Lang, model: Option<&Model>, rules: &[TagRule]) {
    if result.cancelled {
        return;
    }
    let inputs = inputs_from_items(&result.items);
    result.candidates = Some(build_candidates_full(&inputs, master, opts, lang, model, rules));
}

/// 走査結果を、候補生成の入力にする。
pub(super) fn inputs_from_items(items: &[ScanItem]) -> Vec<CandidateInput> {
    items
        .iter()
        .map(|it| CandidateInput {
            file_name: it.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
            format: it.format.clone(),
            info: match &it.info {
                InfoState::Found(i) => Some(i.clone()),
                _ => None,
            },
            meta: it.meta.clone(),
        })
        .collect()
}

/// 走査中のジョブ（背景スレッド＋進捗＋キャンセル）。
pub(super) struct TagScanJob {
    total: usize,
    done: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<TagScanResult>,
}

impl TagScanJob {
    pub(super) fn spawn(
        dir: PathBuf,
        targets: Vec<PathBuf>,
        db: Option<Arc<Mutex<Database>>>,
        master: Vec<TagManagerCategoryUi>,
        opts: MatchOptions,
        lang: Lang,
        model: Option<Arc<Model>>,
        rules: Arc<Vec<TagRule>>,
    ) -> Self {
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let total = targets.len();
        let (w_dir, w_done, w_cancel) = (dir.clone(), Arc::clone(&done), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let mut result = run_scan(w_dir, &targets, db.as_ref(), &w_done, &w_cancel);
            attach_candidates(&mut result, &master, &opts, lang, model.as_deref(), &rules);
            // 受信側が破棄済み（アプリ終了）でもエラーは無視する。
            let _ = tx.send(result);
        });
        Self { total, done, cancel, rx }
    }

    fn progress(&self) -> (usize, usize) {
        (self.done.load(Ordering::Relaxed).min(self.total), self.total)
    }
}

/// 取得ボタンを押せる状態か（`can_start_tag_scan` の判定。仮想フォルダは条件に含まない）。
fn scan_allowed(running: bool, favorites: bool, similar: bool, search: bool, tag_search_tab: bool, has_targets: bool) -> bool {
    !running && !favorites && !similar && !search && !tag_search_tab && has_targets
}

impl NekoviewApp {
    /// 一括情報取得の対象（表示中の実フォルダ直下のアーカイブ。単体画像と無効アーカイブは除く）。
    pub(super) fn tag_scan_targets(&self) -> Vec<PathBuf> {
        self.archives
            .iter()
            .filter(|p| !self.raw_image_files.contains(*p) && !self.invalid_archives.contains(*p))
            .cloned()
            .collect()
    }

    /// ボタンを押せるか。フォルダ表示中（お気に入り・検索・類似・タグ検索を除く。仮想フォルダは対象）で、
    /// 対象があり、実行中でないとき。仮想フォルダは、開いたノードの実パスを実スキャンした一覧を出すので、
    /// 通常のフォルダと同じに扱える（仮想ルート `/` とリンク切れは一覧が空になり、対象なしで押せない）。
    pub(super) fn can_start_tag_scan(&self) -> bool {
        scan_allowed(
            self.tag_scan_job.is_some() || self.tag_trial_active,
            self.viewing_favorites.is_some(),
            self.viewing_similar.is_some(),
            self.viewing_search.is_some(),
            self.folder_pane_tab == super::FolderPaneTab::TagSearch,
            !self.tag_scan_targets().is_empty(),
        )
    }

    pub(super) fn start_tag_scan(&mut self) {
        if !self.can_start_tag_scan() {
            return;
        }
        let targets = self.tag_scan_targets();
        self.tag_scan_result = None;
        self.tag_scan_job = Some(TagScanJob::spawn(
            self.current_dir.clone(),
            targets,
            self.spread_db.clone(),
            self.tag_manager_categories.clone(),
            self.config.tag_match,
            i18n::t(),
            self.active_tag_model(),
            Arc::new(self.tag_rules.rules.clone()),
        ));
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
        // 走査で分かった詳細・ページ数はキャッシュに書く（別フォルダへ移っていても、そのフォルダの分は有効）。
        self.record_scan_meta(&result);
        // 走査中に別フォルダへ移っていたら、結果は捨てる（別フォルダのタグ候補になってしまうため）。
        if result.dir != self.current_dir {
            return;
        }
        let t = i18n::t();
        if result.cancelled {
            self.set_toast(t.tag_scan_cancelled(result.items.len(), result.total));
        } else if let Some(n) = result.candidates.as_ref().and_then(|c| c.limits.error_count) {
            // どちらかの窓で新規要素が上限を超えたら、何もせずに終わる（片方だけの確認には進ませない）。
            self.set_toast(t.tag_scan_too_many(n, crate::tag_candidates::ERROR_NEW_ELEMENTS));
        } else {
            let (found, absent, other) = result.info_counts();
            let mut text = t.tag_scan_done(result.items.len(), found, absent, other);
            if let Some(set) = &result.candidates {
                let c = |r: Role| set.count_by_role(r);
                text.push('\n');
                text.push_str(&t.tag_candidates_summary(
                    [c(Role::Format), c(Role::Main), c(Role::Language), c(Role::Event), c(Role::Author), c(Role::Group)],
                    (set.limits.window1_new, set.limits.window2_new),
                    (set.limits.window1_warn, set.limits.window2_warn),
                ));
            }
            self.set_toast(text);
            // 候補があれば、確認ウィンドウ（1枚目）を開く。
            if let Some(set) = result.candidates.clone() {
                let inputs = inputs_from_items(&result.items);
                let paths = result.items.iter().map(|i| i.path.clone()).collect();
                self.open_tag_confirm(set, inputs, paths, self.tag_manager_categories.clone());
            }
            self.tag_scan_result = Some(result);
        }
    }

    /// 走査結果の詳細・ページ数を、そのフォルダの cache.redb に一括で書く（リストの列がすぐ埋まる）。
    /// 書けなくても走査の結果は使う。ビューアーで数えた確かな値は残す（`neko_dir::merge_archive_meta_batch`）。
    fn record_scan_meta(&mut self, result: &TagScanResult) {
        let rows: Vec<crate::neko_dir::ArchiveMetaWrite> = result
            .items
            .iter()
            .filter_map(|it| {
                let filename = it.path.file_name()?.to_str()?.to_owned();
                Some(crate::neko_dir::ArchiveMetaWrite {
                    filename,
                    mtime: it.mtime,
                    size: it.size,
                    page_count: it.page_count,
                    detail: it.detail,
                })
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let dir = &result.dir;
        let db = if *dir == self.current_dir {
            self.cache_db.clone()
        } else {
            crate::neko_dir::neko_dir_for(dir, &self.config).and_then(|nd| crate::neko_dir::open_cache_db(&nd, dir))
        };
        let Some(db) = db else { return };
        crate::neko_dir::merge_archive_meta_batch(&db, &rows);
        self.archive_info.invalidate_dir(dir);
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

    #[test]
    fn scan_button_rules() {
        // 通常のフォルダ（仮想フォルダも、実パスの一覧なので同じ）。
        assert!(scan_allowed(false, false, false, false, false, true));
        // 対象なし（仮想ルート・リンク切れ・アーカイブの無いフォルダ）。
        assert!(!scan_allowed(false, false, false, false, false, false));
        // 実行中、お気に入り・類似・検索の一覧、タグ検索タブ。
        assert!(!scan_allowed(true, false, false, false, false, true));
        assert!(!scan_allowed(false, true, false, false, false, true));
        assert!(!scan_allowed(false, false, true, false, false, true));
        assert!(!scan_allowed(false, false, false, true, false, true));
        assert!(!scan_allowed(false, false, false, false, true, true));
    }

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
    fn scan_records_detail_kinds_page_count_and_stamp() {
        use crate::fs::archive::meta::DetailKinds;
        use crate::neko_dir::DetailFlag;
        let dir = temp_dir("detail");
        let rich = write_zip(
            &dir,
            "rich.zip",
            &[("1.jpg", b"x"), ("2.png", b"x"), ("meta.json", br#"{"tags":[]}"#), ("info.txt", b"T\nCategory: Manga\n")],
        );
        let plain = write_zip(&dir, "plain.zip", &[("1.jpg", b"x")]);
        let broken = dir.join("broken.zip");
        std::fs::write(&broken, b"not a zip").unwrap();
        let book = dir.join("book");
        std::fs::create_dir_all(&book).unwrap();
        let r = run(&dir, &[rich.clone(), plain.clone(), broken.clone(), book.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r.items[0].detail, DetailFlag::Checked(DetailKinds { xml: false, json: true, txt: true }));
        assert_eq!(r.items[0].page_count, Some(2));
        assert_eq!(r.items[1].detail, DetailFlag::Checked(DetailKinds::default()), "確認済みで無し");
        assert_eq!(r.items[2].detail, DetailFlag::Unknown, "壊れたzipは未確認のまま（N/Aにしない）");
        assert_eq!(r.items[3].detail, DetailFlag::NotApplicable, "フォルダは対応外");
        assert_eq!(r.items[3].page_count, None);
        assert!(r.items[0].mtime > 0 && r.items[0].size > 0);
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

    /// 目視用: `NEKO_SCAN_DIR` のフォルダを走査して、候補の一覧を表示する
    /// （`NEKO_SCAN_DIR=<dir> cargo test dump_candidates_for_dir -- --ignored --nocapture`）。
    /// マスターは、初期状態（メインカテゴリ＋アーカイブ形式）に、作者「Kouji」・グループ「Hawk Bit」・言語「日本語」を足したもの。
    #[test]
    #[ignore]
    fn dump_candidates_for_dir() {
        use crate::tag_master::TagMaster;
        let Ok(dir) = std::env::var("NEKO_SCAN_DIR") else { return };
        let dir = PathBuf::from(dir);
        let mut targets: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        targets.sort();
        let r = run_scan(dir.clone(), &targets, None, &AtomicUsize::new(0), &AtomicBool::new(false));
        let (mut cats, mut next) = crate::tag_manager::default_state();
        let mut m = TagMaster::new(&mut cats, &mut next);
        m.ensure_system_categories();
        for (name, el) in [("作者", "Kouji"), ("グループ", "Hawk Bit"), ("言語", "日本語")] {
            let c = m.add_category(name.to_string(), false, None);
            m.push_element(c, el);
        }
        let mut result = r;
        attach_candidates(&mut result, &cats, &MatchOptions::default(), Lang::Japanese, None, &[]);
        let set = result.candidates.unwrap();
        println!("files={} with_info={} limits={:?}", set.file_count, set.files_with_info, set.limits);
        for c in &set.candidates {
            println!(
                "{:?} {:?} files={} choice={:?} unconfirmed={} cluster={} suggestions={:?}",
                c.role, c.name, c.files.len(), c.choice, c.unconfirmed, c.cluster,
                c.suggestions.iter().map(|s| format!("{}:{}%", s.name, s.score)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn job_uses_the_learned_model_for_roles_when_given() {
        use crate::tag_corpus::{CorpusEntry, Label};
        use crate::tag_learning::LearnedSet;
        // 「外側が作者・内側がサークル」の流儀を学習したモデルを渡すと、モデルなしとは逆の向きになる。
        let pairs = [("Aoi", "Blue Moon"), ("Bell", "Red Sun"), ("Cato", "Green Hill"), ("Dune", "Gray Sea"), ("Echo", "Pink Sky"), ("Fumi", "Gold Sand")];
        let entries: Vec<CorpusEntry> = pairs
            .iter()
            .map(|(a, c)| CorpusEntry {
                file_name: format!("[{a} ({c})] T.zip"),
                labels: vec![(Label::Author, a.to_string()), (Label::Group, c.to_string())],
                hard: false,
            })
            .collect();
        let model = Arc::new(crate::tag_model::Model::train(&LearnedSet { version: 1, entries }.training_data(&MatchOptions::default())).unwrap());
        let dir = temp_dir("job_model");
        let t = write_zip(&dir, "[Kai (Rock Fall)] Book.zip", &[("x.txt", b"x")]);
        let run = |m: Option<Arc<crate::tag_model::Model>>| {
            let job = TagScanJob::spawn(dir.clone(), vec![t.clone()], None, Vec::new(), MatchOptions::default(), Lang::Japanese, m, Arc::new(Vec::new()));
            let mut got = None;
            for _ in 0..300 {
                if let Ok(r) = job.rx.try_recv() {
                    got = Some(r);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            got.expect("結果が届く").candidates.expect("候補まで作られる")
        };
        let role_of = |set: &CandidateSet, name: &str| set.candidates.iter().find(|c| c.name == name).unwrap().role;
        let plain = run(None);
        assert_eq!((role_of(&plain, "Kai"), role_of(&plain, "Rock Fall")), (Role::Group, Role::Author), "モデルなしは外側=グループ");
        let learned = run(Some(model));
        assert_eq!((role_of(&learned, "Kai"), role_of(&learned, "Rock Fall")), (Role::Author, Role::Group), "学習後は外側=作者");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn job_runs_on_a_thread_and_delivers_the_result() {
        let dir = temp_dir("job");
        let t = write_zip(&dir, "a.zip", &[("info.txt", b"Category: Manga")]);
        let job = TagScanJob::spawn(dir.clone(), vec![t], None, Vec::new(), MatchOptions::default(), Lang::Japanese, None, Arc::new(Vec::new()));
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
        let set = r.candidates.as_ref().expect("候補まで作られる");
        assert_eq!(set.count_by_role(Role::Main), 1, "info.txt の Category=Manga");
        assert_eq!(set.count_by_role(Role::Format), 1);
        assert_eq!(job.progress(), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reads_comic_info_and_meta_json_together_with_info_txt() {
        let dir = temp_dir("scan_meta");
        let both = write_zip(
            &dir,
            "a.zip",
            &[
                ("info.txt", b"Title\n\nCategory: Doujinshi\n"),
                ("ComicInfo.xml", b"<ComicInfo><Writer>kirome</Writer><Year>2023</Year><Month>12</Month></ComicInfo>"),
                ("meta.json", br#"{"tags":[{"type":"artist","name":"kayumidome"},{"type":"tag","name":"elf"}]}"#),
            ],
        );
        let only_info = write_zip(&dir, "b.zip", &[("info.txt", b"Title\n")]);
        let none = write_zip(&dir, "c.zip", &[("001.jpg", b"img")]);
        let broken_json = write_zip(&dir, "d.zip", &[("meta.json", b"{broken")]);
        let r = run(&dir, &[both, only_info, none, broken_json]);
        assert_eq!(r.info_counts(), (2, 2, 0));
        let m = r.items[0].meta.as_ref().expect("XML と JSON の両方");
        assert!(m.has_xml() && m.has_json());
        assert_eq!(m.comic.as_ref().unwrap().writers, vec!["kirome"]);
        assert_eq!(m.json.as_ref().unwrap().artists, vec!["kayumidome"]);
        assert!(r.items[1].meta.is_none() && r.items[2].meta.is_none(), "付属データが無ければ None");
        assert!(r.items[3].meta.is_none(), "壊れた meta.json は無し扱い（走査は止めない）");
        assert_eq!(r.meta_counts(), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanned_comic_info_reaches_the_candidates() {
        let dir = temp_dir("scan_meta_cands");
        let z = write_zip(&dir, "[Kirome] Elf Mama-San no manga.zip", &[("ComicInfo.xml", b"<ComicInfo><Writer>kirome</Writer><LanguageISO>ja</LanguageISO></ComicInfo>")]);
        let r = run(&dir, &[z]);
        let set = crate::tag_candidates::build_candidates(&inputs_from_items(&r.items), &[], &MatchOptions::default(), Lang::Japanese);
        let a = set.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert!(!a.unconfirmed, "XML の Writer で、単独の括弧が作者に確定する");
        assert!(set.candidates.iter().any(|c| c.role == Role::Language && c.name == "日本語"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
