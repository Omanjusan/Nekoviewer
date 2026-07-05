//! フォーマット・拡張子判定。マジックバイトによる 7z 判定とその結果キャッシュ、
//! および画像拡張子の判定を担う。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "avif"];

/// 生バイト列で拡張子を確認する（Shift-JIS等でもASCII拡張子は正しく判定できる）
pub(crate) fn is_image_entry_raw(raw: &[u8]) -> bool {
    let lower: Vec<u8> = raw.iter().map(|b| b.to_ascii_lowercase()).collect();
    IMAGE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(format!(".{ext}").as_bytes()))
}

/// ディレクトリ直下の生画像ファイルかどうかを拡張子で判定する
pub fn is_supported_image_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    IMAGE_EXTENSIONS
        .iter()
        .any(|e| Some(e.to_string()) == ext)
}

const SEVEN_Z_SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

/// ファイル先頭バイトの7zシグネチャを読む。ファイルが開けない・短すぎる場合は None。
fn read_7z_signature(path: &Path) -> Option<bool> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 6];
    f.read_exact(&mut buf).ok()?;
    Some(buf == SEVEN_Z_SIGNATURE)
}

fn detect_is_7z(path: &Path) -> bool {
    if let Some(is_7z) = read_7z_signature(path) {
        return is_7z;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("7z") || e.eq_ignore_ascii_case("cb7"))
        .unwrap_or(false)
}

/// ディレクトリ単位で世代管理する is_7z 判定結果キャッシュ。
/// 呼び出しごとにファイルヘッダを読み直すコスト（特にネットワーク越しのパス）を避けるため、
/// path -> bool を覚えておく。無制限に肥大化しないよう、
/// 「直近何ディレクトリ分を覚えておくか（世代数）」と「総推定バイト数」の両方で頭打ちにする。
struct Is7zCache {
    current_gen: u64,
    last_dir: Option<PathBuf>,
    total_bytes: usize,
    dirs: HashMap<PathBuf, DirGeneration>,
}

struct DirGeneration {
    generation: u64,
    entries: HashMap<PathBuf, bool>,
    bytes: usize,
}

/// 直近何ディレクトリ分の判定結果を保持するか
const IS_7Z_CACHE_MAX_GENERATIONS: u64 = 15;
/// キャッシュ全体の推定使用量の上限（16MB。ファイル収集家がディレクトリに
/// 数十万ファイル溜め込んでいても頭打ちにするための安全弁）
const IS_7Z_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// 1エントリあたりの固定オーバーヘッド見積もり（PathBuf構造体+ヒープ確保+HashMapスロット分）
const IS_7Z_CACHE_ENTRY_OVERHEAD: usize = 64;

impl Is7zCache {
    fn get_or_compute(&mut self, path: &Path) -> bool {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if self.last_dir.as_deref() != Some(dir.as_path()) {
            self.current_gen += 1;
            self.last_dir = Some(dir.clone());
            self.evict_old_generations();
        }
        let generation = self.current_gen;

        if let Some(g) = self.dirs.get(&dir) {
            if let Some(&is_7z) = g.entries.get(path) {
                return is_7z;
            }
        }

        let is_7z = detect_is_7z(path);
        let entry_bytes = path.as_os_str().len() + IS_7Z_CACHE_ENTRY_OVERHEAD;
        let g = self.dirs.entry(dir).or_insert_with(|| DirGeneration {
            generation,
            entries: HashMap::new(),
            bytes: 0,
        });
        g.generation = generation;
        g.entries.insert(path.to_path_buf(), is_7z);
        g.bytes += entry_bytes;
        self.total_bytes += entry_bytes;

        self.evict_over_budget();
        is_7z
    }

    /// 最新世代から IS_7Z_CACHE_MAX_GENERATIONS より古いディレクトリを丸ごと削除する
    fn evict_old_generations(&mut self) {
        let cutoff = self.current_gen.saturating_sub(IS_7Z_CACHE_MAX_GENERATIONS);
        let stale: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|(_, g)| g.generation <= cutoff)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            if let Some(g) = self.dirs.remove(&k) {
                self.total_bytes -= g.bytes;
            }
        }
    }

    /// 総推定バイト数が上限を超えている間、最も古い世代のディレクトリから削除する
    fn evict_over_budget(&mut self) {
        while self.total_bytes > IS_7Z_CACHE_MAX_BYTES {
            let Some(oldest) = self.dirs.iter().min_by_key(|(_, g)| g.generation).map(|(k, _)| k.clone()) else {
                break;
            };
            match self.dirs.remove(&oldest) {
                Some(g) => self.total_bytes -= g.bytes,
                None => break,
            }
        }
    }
}

fn is_7z_cache() -> &'static std::sync::Mutex<Is7zCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Is7zCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(Is7zCache {
            current_gen: 0,
            last_dir: None,
            total_bytes: 0,
            dirs: HashMap::new(),
        })
    })
}

/// 7z/CB7 かどうかを判定する。先頭バイトのシグネチャを優先し、
/// ファイルが読めない場合のみ拡張子（7z/cb7）にフォールバックする。
/// 判定結果はディレクトリ単位・世代管理でキャッシュし、同じパスへの再判定でファイルI/Oを避ける。
pub fn is_7z_path(path: &Path) -> bool {
    is_7z_cache().lock().unwrap().get_or_compute(path)
}
