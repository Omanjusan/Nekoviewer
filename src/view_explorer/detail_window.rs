//! 詳細ウィンドウ。リストの「詳細」列のアイコンから開き、アーカイブ内の詳細テキスト
//! （ComicInfo.xml・内部の .nfo・zipコメント）を一覧で見せる。
//!
//! - 読み出し（`archive::meta::read_detail`）は別スレッドで行い、窓は読み込み中表示を出して待つ
//! - 窓は1つだけ。別のファイルのアイコンを押すと中身を差し替える（窓の位置は保つ）
//! - 文字は読み出し時に安全化済み（制御文字・BiDi・過剰な結合文字を除去）。ここではさらに長い本文を途中で切る
//! - 本文はコピーできるが、リンクとしては扱わない（ComicInfo の Web も文字のまま。勝手に開かない）

use std::path::PathBuf;
use std::sync::mpsc;

use crate::fs::archive::meta::{self, DetailMeta};
use crate::i18n;

use super::NekoviewApp;

/// 1つの本文として画面に出す最大文字数。レイアウトの負荷を抑えるための上限。
const DISPLAY_CHAR_LIMIT: usize = 20_000;

pub(super) struct DetailWindow {
    path: PathBuf,
    rx: Option<mpsc::Receiver<Option<DetailMeta>>>,
    /// `None` = 読み込み中。`Some(None)` = 読めなかった／詳細なし。
    result: Option<Option<DetailMeta>>,
}

impl DetailWindow {
    fn spawn(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            // 受信側が破棄済み（窓を閉じた・差し替えた）でもエラーは無視する。
            let _ = tx.send(meta::read_detail(&worker_path));
        });
        Self { path, rx: Some(rx), result: None }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok(r) => {
                self.result = Some(r);
                self.rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.result = Some(None);
                self.rx = None;
            }
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
    /// 「詳細」アイコンの押下なら、詳細ウィンドウを開いて true（選択や起動はしない）。
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
            self.detail_window = Some(DetailWindow::spawn(path));
        }
        true
    }

    pub(super) fn draw_detail_window(&mut self, ctx: &egui::Context) {
        let Some(win) = self.detail_window.as_mut() else { return };
        win.poll();
        if win.rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let t = i18n::t();
        let name = win.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        let mut open = true;
        egui::Window::new(t.detail_window_title(&name))
            .id(egui::Id::new("detail_window"))
            .open(&mut open)
            .default_size([640.0, 480.0])
            .resizable(true)
            .show(ctx, |ui| match &win.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(t.detail_loading());
                    });
                }
                Some(Some(d)) if d.has_detail() => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| draw_detail_body(ui, d));
                }
                Some(_) => {
                    ui.label(t.detail_none());
                }
            });
        if !open {
            self.detail_window = None;
        }
    }
}

fn draw_detail_body(ui: &mut egui::Ui, d: &DetailMeta) {
    let t = i18n::t();
    if let Some(info) = &d.comic_info {
        ui.strong(t.detail_section_comic_info());
        egui::Grid::new("detail_comic_info").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (tag, value) in &info.fields {
                ui.label(egui::RichText::new(t.comic_info_field(tag)).weak());
                body_label(ui, value);
                ui.end_row();
            }
        });
        ui.separator();
    }
    for nfo in &d.nfo {
        ui.strong(t.detail_section_nfo(&nfo.name));
        body_label(ui, &nfo.text);
        ui.separator();
    }
    if !d.comment.trim().is_empty() {
        ui.strong(t.detail_section_comment());
        body_label(ui, &d.comment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_display_cuts_on_char_boundaries() {
        assert_eq!(clip_display("あいうえお", 3), ("あいう".to_owned(), true));
        assert_eq!(clip_display("あいう", 3), ("あいう".to_owned(), false));
        assert_eq!(clip_display("", 3), (String::new(), false));
        assert_eq!(clip_display("abc", 0), (String::new(), true));
    }

    #[test]
    fn worker_delivers_the_detail_and_reports_unreadable_files() {
        // zipでないファイル: 読み込み完了後に Some(None)。
        let bad = std::env::temp_dir().join(format!("nekoviewer_detail_win_{}_bad.zip", std::process::id()));
        std::fs::write(&bad, b"not a zip").unwrap();
        let mut w = DetailWindow::spawn(bad.clone());
        for _ in 0..200 {
            w.poll();
            if w.result.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&bad);
        assert!(matches!(w.result, Some(None)));
        assert!(w.rx.is_none());
    }

    #[test]
    fn comic_info_labels_fall_back_to_the_tag_name() {
        assert_eq!(i18n::Lang::Japanese.comic_info_field("Title"), "タイトル");
        assert_eq!(i18n::Lang::English.comic_info_field("Writer"), "Writer");
        assert_eq!(i18n::Lang::Chinese.comic_info_field("Summary"), "简介");
        assert_eq!(i18n::Lang::Japanese.comic_info_field("Unknown"), "Unknown");
    }
}
