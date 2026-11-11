//! 詳細ウィンドウ。リストの「詳細」列のアイコンから開き、アーカイブ内の詳細ファイル
//! （ComicInfo.xml・meta.json・内部の .nfo / info.txt / readme.txt・zipコメント）を**ファイルごとの別ウィンドウ**で見せる。
//!
//! - 読み出し（`archive::meta::read_detail`）は別スレッドで行い、窓は読み込み中表示を出して待つ
//! - 同じアーカイブのアイコンを続けて押すと窓が増える（同じファイルの窓が既にあれば増やさない）。別のアーカイブのアイコンなら全部閉じて作り直す
//! - 窓は作った順に、タイトルバー1つ分下・右20pxずつずらして出す（`cascade_pos`）
//! - 他の画像やアーカイブをビューアーで開いたときは、まとめて閉じる（`close_all`）
//! - 文字は読み出し時に安全化済み（制御文字・BiDi・過剰な結合文字を除去）。ここではさらに長い本文を途中で切る
//! - 本文はコピーできるが、リンクとしては扱わない（ComicInfo の Web も文字のまま。勝手に開かない）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use crate::fs::archive::meta::{self, ComicInfo, DetailKinds, DetailMeta};
use crate::i18n;
use crate::tag_meta::JsonMeta;

use super::NekoviewApp;

/// 1つの本文として画面に出す最大文字数。レイアウトの負荷を抑えるための上限。
const DISPLAY_CHAR_LIMIT: usize = 20_000;
/// 窓を重ねて出すときの、1枚ごとの右へのずらし幅。
const CASCADE_DX: f32 = 20.0;
/// ずらしが画面から外れないよう、この枚数で最初の位置に戻す。
const CASCADE_WRAP: usize = 12;

/// 窓の中身。
enum PaneBody {
    Comic(ComicInfo),
    /// meta.json。整形できたら `Some(項目)`、できなければ本文をそのまま出す。
    Json(String, Option<JsonMeta>),
    Text(String),
    Message(&'static str),
}

/// 同じ種類の窓を二重に作らないための識別。
#[derive(Clone, Debug, PartialEq, Eq)]
enum PaneKey {
    Xml,
    Json,
    /// `DetailMeta::nfo` の添字。
    Text(usize),
    Comment,
    Notice,
}

struct Pane {
    key: PaneKey,
    uid: u64,
    title: String,
    body: PaneBody,
    open: bool,
    /// 何枚目に作った窓か（ずらし位置の元）。
    slot: usize,
}

struct Pending {
    rx: mpsc::Receiver<Option<DetailMeta>>,
    kinds: DetailKinds,
}

/// 1つのアーカイブについて開いている詳細窓の集まり。
pub(super) struct DetailWindow {
    path: PathBuf,
    pending: Vec<Pending>,
    panes: Vec<Pane>,
    /// 窓を作った数（閉じても減らさない。ずらしが重ならないようにする）。
    made: usize,
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// `slot` 枚目の窓の位置。`base` からタイトルバー1つ分下・右20pxずつずらす。
pub(super) fn cascade_pos(base: egui::Pos2, slot: usize, title_h: f32) -> egui::Pos2 {
    let n = (slot % CASCADE_WRAP) as f32;
    egui::pos2(base.x + CASCADE_DX * n, base.y + title_h * n)
}

impl DetailWindow {
    fn new(path: PathBuf) -> Self {
        Self { path, pending: Vec::new(), panes: Vec::new(), made: 0 }
    }

    /// `kinds` の種類の窓を開く（読み出しは別スレッド）。
    fn request(&mut self, kinds: DetailKinds) {
        let (tx, rx) = mpsc::channel();
        let worker_path = self.path.clone();
        std::thread::spawn(move || {
            // 受信側が破棄済み（窓を閉じた・差し替えた）でもエラーは無視する。
            let _ = tx.send(meta::read_detail(&worker_path));
        });
        self.pending.push(Pending { rx, kinds });
    }

    fn add_pane(&mut self, key: PaneKey, title: String, body: PaneBody) {
        if self.panes.iter().any(|p| p.key == key) {
            return;
        }
        let uid = NEXT_UID.fetch_add(1, Ordering::Relaxed);
        self.panes.push(Pane { key, uid, title, body, open: true, slot: self.made });
        self.made += 1;
    }

    /// 読み出しが終わった要求を窓にする。
    fn poll(&mut self) {
        let mut done: Vec<(Option<DetailMeta>, DetailKinds)> = Vec::new();
        self.pending.retain(|p| match p.rx.try_recv() {
            Ok(r) => {
                done.push((r, p.kinds));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                done.push((None, p.kinds));
                false
            }
        });
        for (result, kinds) in done {
            self.build_panes(result.as_ref(), kinds);
        }
    }

    fn build_panes(&mut self, result: Option<&DetailMeta>, kinds: DetailKinds) {
        let t = i18n::t();
        let file = self.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        let title = |section: &str| format!("{section} - {file}");
        let before = self.panes.len();
        if let Some(d) = result {
            if kinds.xml
                && let Some(info) = &d.comic_info
            {
                self.add_pane(PaneKey::Xml, title(t.detail_section_comic_info()), PaneBody::Comic(info.clone()));
            }
            if kinds.json
                && let Some(json) = d.meta_json.as_deref().filter(|j| !j.trim().is_empty())
            {
                self.add_pane(PaneKey::Json, title("meta.json"), PaneBody::Json(json.to_owned(), JsonMeta::parse(json)));
            }
            if kinds.txt {
                for (i, nfo) in d.nfo.iter().enumerate() {
                    self.add_pane(PaneKey::Text(i), title(&nfo.name), PaneBody::Text(nfo.text.clone()));
                }
                if !d.comment.trim().is_empty() {
                    self.add_pane(PaneKey::Comment, title(t.detail_section_comment()), PaneBody::Text(d.comment.clone()));
                }
            }
        }
        // 何も出せず、ほかの窓も無いときは、その旨を知らせる窓を1枚出す。
        if self.panes.len() == before && self.panes.is_empty() {
            self.add_pane(PaneKey::Notice, t.detail_window_title(&file), PaneBody::Message(t.detail_none()));
        }
    }
}

/// 本文を `limit` 文字までに切る。切ったら true。
pub(super) fn clip_display(text: &str, limit: usize) -> (String, bool) {
    match text.char_indices().nth(limit) {
        Some((byte, _)) => (text[..byte].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// 選択してコピーできる、リンクにならない本文。
fn body_label(ui: &mut egui::Ui, text: &str) {
    let (shown, clipped) = clip_display(text, DISPLAY_CHAR_LIMIT);
    ui.add(egui::Label::new(shown).selectable(true).wrap());
    if clipped {
        ui.label(egui::RichText::new(i18n::t().detail_truncated(DISPLAY_CHAR_LIMIT)).weak());
    }
}

impl NekoviewApp {
    /// 「詳細」アイコンの押下なら、`kinds` の詳細窓を開いて true（選択や起動はしない）。
    /// ダブルクリックの2回目も、起動させないよう握りつぶす。
    pub(super) fn try_click_detail(&mut self, ui: &egui::Ui, response: &egui::Response, real_idx: usize) -> bool {
        // 当たり判定はリスト描画のたびに作り直す。カードに切り替えた直後の古い矩形は拾わない。
        if self.view_mode.mode == super::view_mode::ViewMode::Card || !(response.clicked() || response.double_clicked()) {
            return false;
        }
        let Some(rect) = self.detail_icon_hits.get(&real_idx).copied() else { return false };
        if !ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p)) {
            return false;
        }
        if response.clicked()
            && let Some(path) = self.archives.get(real_idx).cloned()
        {
            // 暫定: 3種類まとめて開く（種類別のアイコンは列UIの段階で付ける）。
            self.open_detail_windows(path, DetailKinds { xml: true, json: true, txt: true });
        }
        true
    }

    /// `path` の `kinds` の詳細窓を開く。別のアーカイブの窓が開いていれば閉じてから。
    pub(super) fn open_detail_windows(&mut self, path: PathBuf, kinds: DetailKinds) {
        if !self.detail_window.as_ref().is_some_and(|w| w.path == path) {
            self.detail_window = Some(DetailWindow::new(path));
        }
        if let Some(w) = self.detail_window.as_mut() {
            w.request(kinds);
        }
    }

    /// 詳細窓をすべて閉じる（ほかの画像・アーカイブを開いたときの掃除）。
    pub(super) fn close_detail_windows(&mut self) {
        self.detail_window = None;
    }

    pub(super) fn draw_detail_window(&mut self, ctx: &egui::Context) {
        let Some(win) = self.detail_window.as_mut() else { return };
        win.poll();
        let t = i18n::t();
        if !win.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let screen = ctx.content_rect();
        let base = egui::pos2((screen.center().x - 300.0).max(screen.min.x + 20.0), (screen.center().y - 240.0).max(screen.min.y + 20.0));
        let title_h = ctx.global_style().spacing.interact_size.y + 8.0;
        // 読み込み中は、窓が1枚もない間だけ小さな表示を出す。
        if win.panes.is_empty() && !win.pending.is_empty() {
            let name = win.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
            egui::Window::new(t.detail_window_title(&name))
                .id(egui::Id::new("detail_window_loading"))
                .default_pos(base)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(t.detail_loading());
                    });
                });
        }
        for pane in &mut win.panes {
            let mut open = pane.open;
            egui::Window::new(&pane.title)
                .id(egui::Id::new(("detail_pane", pane.uid)))
                .open(&mut open)
                .default_pos(cascade_pos(base, pane.slot, title_h))
                .default_size([560.0, 420.0])
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| draw_pane_body(ui, &pane.body));
                });
            pane.open = open;
        }
        win.panes.retain(|p| p.open);
        if win.panes.is_empty() && win.pending.is_empty() {
            self.detail_window = None;
        }
    }
}

fn draw_pane_body(ui: &mut egui::Ui, body: &PaneBody) {
    let t = i18n::t();
    match body {
        PaneBody::Comic(info) => {
            egui::Grid::new("detail_comic_info").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                for (tag, value) in &info.fields {
                    ui.label(egui::RichText::new(t.comic_info_field(tag)).weak());
                    body_label(ui, value);
                    ui.end_row();
                }
            });
        }
        PaneBody::Json(raw, Some(j)) => {
            let _ = raw;
            egui::Grid::new("detail_meta_json").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                let one = |ui: &mut egui::Ui, key: &str, v: &Option<String>| {
                    if let Some(v) = v {
                        ui.label(egui::RichText::new(t.detail_json_field(key)).weak());
                        body_label(ui, v);
                        ui.end_row();
                    }
                };
                let many = |ui: &mut egui::Ui, key: &str, v: &[String]| {
                    if !v.is_empty() {
                        ui.label(egui::RichText::new(t.detail_json_field(key)).weak());
                        body_label(ui, &v.join(", "));
                        ui.end_row();
                    }
                };
                one(ui, "title_japanese", &j.title_japanese);
                one(ui, "title_english", &j.title_english);
                many(ui, "language", &j.languages);
                many(ui, "category", &j.categories);
                many(ui, "artist", &j.artists);
                many(ui, "group", &j.groups);
                many(ui, "parody", &j.parodies);
                many(ui, "character", &j.characters);
                many(ui, "tag", &j.tags);
            });
        }
        // 項目を拾えなかった meta.json は、本文をそのまま出す。
        PaneBody::Json(raw, None) => body_label(ui, raw),
        PaneBody::Text(text) => body_label(ui, text),
        PaneBody::Message(m) => {
            ui.label(*m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: DetailKinds = DetailKinds { xml: true, json: true, txt: true };

    fn settle(w: &mut DetailWindow) {
        for _ in 0..300 {
            w.poll();
            if w.pending.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn build_zip(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("nekoviewer_detail_win_{}_{name}.zip", std::process::id()));
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (n, data) in files {
            w.start_file(*n, ::zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        path
    }

    #[test]
    fn clip_display_cuts_on_char_boundaries() {
        assert_eq!(clip_display("あいうえお", 3), ("あいう".to_owned(), true));
        assert_eq!(clip_display("あいう", 3), ("あいう".to_owned(), false));
        assert_eq!(clip_display("", 3), (String::new(), false));
        assert_eq!(clip_display("abc", 0), (String::new(), true));
    }

    #[test]
    fn unreadable_file_gives_one_notice_window() {
        let bad = std::env::temp_dir().join(format!("nekoviewer_detail_win_{}_bad.zip", std::process::id()));
        std::fs::write(&bad, b"not a zip").unwrap();
        let mut w = DetailWindow::new(bad.clone());
        w.request(ALL);
        settle(&mut w);
        let _ = std::fs::remove_file(&bad);
        assert_eq!(w.panes.len(), 1);
        assert_eq!(w.panes[0].key, PaneKey::Notice);
    }

    #[test]
    fn one_window_per_file_and_no_duplicates_on_repeat() {
        let path = build_zip(
            "multi",
            &[
                ("001.jpg", b"i"),
                ("ComicInfo.xml", b"<ComicInfo><Title>T</Title></ComicInfo>"),
                ("meta.json", br#"{"title":{"english":"E"},"tags":[{"type":"artist","name":"kirome"}]}"#),
                ("a/info.txt", b"one"),
                ("b.nfo", b"two"),
            ],
        );
        let mut w = DetailWindow::new(path.clone());
        w.request(ALL);
        settle(&mut w);
        let keys: Vec<_> = w.panes.iter().map(|p| p.key.clone()).collect();
        assert_eq!(keys, vec![PaneKey::Xml, PaneKey::Json, PaneKey::Text(0), PaneKey::Text(1)], "xml・json・txt各ファイルで別窓");
        assert!(matches!(&w.panes[1].body, PaneBody::Json(_, Some(j)) if j.artists == ["kirome"]));
        let slots: Vec<_> = w.panes.iter().map(|p| p.slot).collect();
        assert_eq!(slots, vec![0, 1, 2, 3]);
        // もう一度押しても増えない。
        w.request(ALL);
        settle(&mut w);
        assert_eq!(w.panes.len(), 4);
        // 種類を絞ると、その種類だけ。
        let mut only_json = DetailWindow::new(path.clone());
        only_json.request(DetailKinds { xml: false, json: true, txt: false });
        settle(&mut only_json);
        let _ = std::fs::remove_file(&path);
        assert_eq!(only_json.panes.len(), 1);
        assert_eq!(only_json.panes[0].key, PaneKey::Json);
    }

    #[test]
    fn cascade_moves_right_and_down_per_window_and_wraps() {
        let base = egui::pos2(100.0, 50.0);
        assert_eq!(cascade_pos(base, 0, 26.0), base);
        assert_eq!(cascade_pos(base, 2, 26.0), egui::pos2(140.0, 102.0));
        assert_eq!(cascade_pos(base, CASCADE_WRAP, 26.0), base, "一周したら最初の位置へ");
    }

    #[test]
    fn comic_info_labels_fall_back_to_the_tag_name() {
        assert_eq!(i18n::Lang::Japanese.comic_info_field("Title"), "タイトル");
        assert_eq!(i18n::Lang::English.comic_info_field("Writer"), "Writer");
        assert_eq!(i18n::Lang::Chinese.comic_info_field("Summary"), "简介");
        assert_eq!(i18n::Lang::Japanese.comic_info_field("Unknown"), "Unknown");
    }
}
