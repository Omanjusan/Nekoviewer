//! 7z/CB7 バックエンド。ソリッド圧縮のため一括展開してエントリマップを返す方式を採る。

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;

use super::decode::decode_image;
use super::detect::is_image_entry_raw;
use super::{ArchiveMemoryEstimate, EntryEstimate, ImageEntry};

/// 7z のヘッダ(ファイル一覧・メタデータ)のみを読み、画像エントリを一覧化する。
/// データストリームの展開は行わない。
pub(crate) fn list_images_7z(path: &Path) -> Vec<ImageEntry> {
    let Ok(archive) = sevenz_rust2::Archive::open(path) else {
        return Vec::new();
    };

    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for entry in &archive.files {
        if entry.is_directory || !entry.has_stream {
            continue;
        }
        if !is_image_entry_raw(entry.name.as_bytes()) {
            continue;
        }
        let date_key = if entry.has_last_modified_date {
            nt_time_to_date_key(entry.last_modified_date)
        } else {
            0
        };
        pairs.push((entry.name.clone(), entry.name.clone(), date_key));
    }

    super::finalize_entries(pairs)
}

/// 7zの`NtTime`(Windows FILETIME)をZIP版と同じ形式(年月日時分秒を1桁ずつパックしたu64)に変換する。
/// 変換不能(エポック未満・オーバーフロー等)なら0を返す。
fn nt_time_to_date_key(t: sevenz_rust2::NtTime) -> u64 {
    let st: std::time::SystemTime = t.into();
    let Ok(dur) = st.duration_since(std::time::SystemTime::UNIX_EPOCH) else {
        return 0;
    };
    super::unix_secs_to_date_key(dur.as_secs())
}

/// 7z(ソリッド圧縮)は1エントリだけの取り出しがブロック先頭からの再展開になり非効率なため、
/// 開いた時点で画像エントリを一括デコードし `entry_name -> 生バイト列` の対応表にまとめて返す。
/// 以降のページ送りはこの表を引くだけで済み、再展開は発生しない。
pub fn extract_all_images_7z<R: Read + Seek>(source: R) -> HashMap<String, Vec<u8>> {
    let Ok(mut reader) = sevenz_rust2::ArchiveReader::new(source, sevenz_rust2::Password::empty()) else {
        return HashMap::new();
    };

    let mut out = HashMap::new();
    let _ = reader.for_each_entries(&mut |entry: &sevenz_rust2::ArchiveEntry, r: &mut dyn Read| {
        if !entry.is_directory && entry.has_stream && is_image_entry_raw(entry.name.as_bytes()) {
            let mut buf = Vec::with_capacity(entry.size as usize);
            if r.read_to_end(&mut buf).is_ok() {
                out.insert(entry.name.clone(), buf);
            }
        }
        Ok(true) // 全エントリを処理する
    });
    out
}

/// ディスク上の7zファイルを開いて `extract_all_images_7z` を実行する
pub fn extract_all_images_7z_path(path: &Path) -> HashMap<String, Vec<u8>> {
    let Ok(file) = std::fs::File::open(path) else {
        return HashMap::new();
    };
    extract_all_images_7z(file)
}

/// 7zの先頭画像1枚をデコードして返す（サムネイル用）。
/// 7zはヘッダがアーカイブ末尾に集約されるためZIP版のような先頭シーケンシャル最適化は
/// 使えないが、目的の画像が見つかった時点でブロック展開を打ち切ることで、
/// アーカイブ全体を一括展開する`extract_all_images_7z`より軽量に済ませる。
pub(crate) fn load_first_image_7z(path: &Path) -> Option<image::DynamicImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty()).ok()?;

    let mut result: Option<image::DynamicImage> = None;
    let _ = reader.for_each_entries(|entry: &sevenz_rust2::ArchiveEntry, r: &mut dyn Read| {
        let mut buf = Vec::with_capacity(entry.size as usize);
        // ソリッドストリーム内の位置整合のため、対象外エントリでも必ず読み切る。
        let read_ok = r.read_to_end(&mut buf).is_ok();
        if read_ok && !entry.is_directory && entry.has_stream && is_image_entry_raw(entry.name.as_bytes()) {
            if let Some(img) = decode_image(&buf) {
                result = Some(img);
                return Ok(false); // 見つかった時点でこのブロックの展開を打ち切る
            }
        }
        Ok(true)
    });
    result
}

/// フェーズ4: 7z版の見積もり。ソリッド圧縮では「軽くサンプリング」が成立しないため、
/// どのみち開く際に必要になる一括展開(フェーズ2の`extract_all_images_7z_path`と同じ処理)を
/// 先に行い、全エントリのヘッダ寸法から実際の合計値を厳密に計算する
/// （サンプル平均による概算ではなく全数チェックなので、ZIP版より判定精度は高い）。
pub(crate) fn estimate_archive_memory_7z(
    path: &Path,
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
) -> ArchiveMemoryEstimate {
    let map = extract_all_images_7z_path(path);
    if map.is_empty() {
        return ArchiveMemoryEstimate::Ok;
    }

    let mut total: usize = 0;
    for entry in entries {
        let Some(buf) = map.get(&entry.entry_name) else { continue };
        match super::estimate_bytes_for_entry(buf, &entry.display_name, budget_bytes, ring_bounds) {
            Some(EntryEstimate::OverBudget) => return ArchiveMemoryEstimate::OverBudget,
            Some(EntryEstimate::Bytes(n)) => {
                total = total.saturating_add(n);
                if total > budget_bytes {
                    return ArchiveMemoryEstimate::OverBudget;
                }
            }
            None => {} // 読み込み/デコード失敗エントリは合計から除外
        }
    }
    ArchiveMemoryEstimate::Ok
}
