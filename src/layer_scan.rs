//! 起動オプション `--scan-layers`（テスト用）: 起動時に復帰するフォルダ（[`pick_scan_dir`]）直下のzip/cbzを開き、
//! 中に層つきのファイル名（末尾の半角 ` (n)`。[`crate::view_reader::split_layer`] と同じ判定）を持つ
//! ものを**全件**探して標準出力に出す。サブフォルダ・7z・tarは見ない。
//! 走査は別スレッドで走るので、起動は止まらない。

use std::path::{Path, PathBuf};

use crate::gui_config::{SavedTab, TabPositions};

/// 名前列のうち層つき（層番号 > 0）の件数。
pub(crate) fn count_layered<'a>(names: impl IntoIterator<Item = &'a str>) -> usize {
    names.into_iter().filter(|n| crate::view_reader::split_layer(n).0 > 0).count()
}

/// 走査対象にするアーカイブか（軽いzip/cbzだけ。拡張子は大小無視）。
fn is_zip_like(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip") || e.eq_ignore_ascii_case("cbz"))
}

/// `dir` 直下のzip/cbzを名前順に開き、中に層つきの名前を持つものを全件探す。
/// 見つけるたびに `on_found(ファイル名, 層つきの件数)` を呼び、走査したzip/cbzの総数を返す。
/// 読めないアーカイブ・ディレクトリは飛ばす。
pub(crate) fn scan_layered_zips(dir: &Path, mut on_found: impl FnMut(&str, usize)) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut paths: Vec<_> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_zip_like(p))
        .collect();
    paths.sort();
    for p in &paths {
        let entries = crate::fs::archive::list_images(p);
        let n = count_layered(entries.iter().map(|e| e.display_name.as_str()));
        if n > 0 {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                on_found(name, n);
            }
        }
    }
    paths.len()
}

/// 起動時の走査対象フォルダを決める。実フォルダ・仮想フォルダの両対応。
/// 前回のタブが仮想フォルダで、保存されたノードがルート以外（実パスを持つ）かつ実在するフォルダなら、その実パス。
/// それ以外（実ツリー・ルート・実パスに触れない等）は、実ツリー側の復帰先 `start_dir`。
/// CLI引数で開いたときは呼び出し側（main）が先にタブを実ツリーに固定するので、`start_dir` になる。
pub(crate) fn pick_scan_dir(tabs: &TabPositions, start_dir: &Path) -> PathBuf {
    if tabs.active == Some(SavedTab::VirtualFolders) {
        if let Some(node) = &tabs.virtual_node {
            if node.path.is_dir() {
                return node.path.clone();
            }
        }
    }
    start_dir.to_path_buf()
}

/// 別スレッドで `dir` を走査し、結果を標準出力に出す（起動は止めない）。
pub(crate) fn spawn_scan(dir: PathBuf) {
    let _ = std::thread::Builder::new().name("layer-scan".into()).spawn(move || {
        println!("[scan-layers] scanning: {}", dir.display());
        let mut hits = 0usize;
        let total = scan_layered_zips(&dir, |name, n| {
            hits += 1;
            println!("[scan-layers] FOUND: {name} (layered entries: {n})");
        });
        println!("[scan-layers] done: {hits} of {total} zip/cbz have layered entries");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_zip(path: &Path, names: &[&str]) {
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for n in names {
            w.start_file(*n, ::zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn count_layered_counts_only_paren_number_names() {
        assert_eq!(count_layered(["001.jpg", "001 (2).jpg", "002 (3).jpg", "a (b).jpg", "(2).jpg"]), 2);
        assert_eq!(count_layered(Vec::<&str>::new()), 0);
    }

    #[test]
    fn scan_reports_every_layered_zip_in_name_order_and_skips_others() {
        let dir = std::env::temp_dir().join(format!("nv_layer_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        write_zip(&dir.join("a_plain.zip"), &["001.jpg", "002.jpg"]);
        write_zip(&dir.join("b_layered.CBZ"), &["001.jpg", "001 (2).jpg", "002 (2).jpg"]);
        write_zip(&dir.join("c_layered.zip"), &["001 (2).jpg"]);
        // サブフォルダ内・zip以外は対象外
        write_zip(&dir.join("sub").join("deep.zip"), &["001 (2).jpg"]);
        std::fs::write(dir.join("memo (2).txt"), b"x").unwrap();

        let mut found = Vec::new();
        let total = scan_layered_zips(&dir, |n, c| found.push((n.to_string(), c)));
        assert_eq!(total, 3);
        assert_eq!(found, vec![("b_layered.CBZ".to_string(), 2), ("c_layered.zip".to_string(), 1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_follows_virtual_node_real_path_only_on_virtual_tab() {
        use crate::gui_config::VirtualPosition;
        let node_dir = std::env::temp_dir().join(format!("nv_layer_pick_{}", std::process::id()));
        std::fs::create_dir_all(&node_dir).unwrap();
        let start = PathBuf::from("/real/start");
        let pos = Some(VirtualPosition { id: 7, path: node_dir.clone() });

        let mut tabs = TabPositions { active: Some(SavedTab::VirtualFolders), virtual_node: pos.clone(), ..Default::default() };
        assert_eq!(pick_scan_dir(&tabs, &start), node_dir);
        // ルート（未保存）は実ツリーの復帰先
        tabs.virtual_node = None;
        assert_eq!(pick_scan_dir(&tabs, &start), start);
        // 実パスが無くなっていれば実ツリーの復帰先
        tabs.virtual_node = Some(VirtualPosition { id: 7, path: PathBuf::from("/nonexistent/nv_layer_pick") });
        assert_eq!(pick_scan_dir(&tabs, &start), start);
        // 仮想タブ以外では仮想ノードを無視
        tabs.virtual_node = pos;
        tabs.active = Some(SavedTab::RealTree);
        assert_eq!(pick_scan_dir(&tabs, &start), start);
        tabs.active = None;
        assert_eq!(pick_scan_dir(&tabs, &start), start);
        let _ = std::fs::remove_dir_all(&node_dir);
    }

    #[test]
    fn missing_dir_yields_empty() {
        assert_eq!(scan_layered_zips(Path::new("/nonexistent/nv_layer_scan"), |_, _| panic!("no hit expected")), 0);
    }
}
