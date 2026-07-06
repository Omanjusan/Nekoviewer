//! フォーマット・拡張子判定。マジックバイトによるアーカイブ形式判定とその結果キャッシュ、
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

/// アーカイブのコンテナ形式。中身の画像フォーマットではなくアーカイブそのものの種別。
/// zip は基幹形式のため常時、7z/tar は対応 feature が有効なときのみ variant を持つ。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArchiveFormat {
    Zip,
    #[cfg(feature = "fmt-7z")]
    SevenZ,
    /// TAR/CBT（raw および gzip 圧縮の tar.gz/tgz）。7z 同様ソリッド扱いで一括展開する。
    #[cfg(feature = "fmt-tar")]
    Tar,
}

#[cfg(feature = "fmt-7z")]
const SEVEN_Z_SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
/// ZIP のローカルファイルヘッダ / 空アーカイブ(EOCD) / 分割アーカイブの各シグネチャ先頭4バイト。
const ZIP_SIGNATURES: [[u8; 4]; 3] = [
    [0x50, 0x4B, 0x03, 0x04], // 通常
    [0x50, 0x4B, 0x05, 0x06], // 空アーカイブ
    [0x50, 0x4B, 0x07, 0x08], // 分割/スパンド
];
/// gzip のマジック（tar.gz/tgz 判定用）
#[cfg(feature = "fmt-tar")]
const GZIP_SIGNATURE: [u8; 2] = [0x1F, 0x8B];
/// zstd フレームのマジック（tar.zst/tzst 判定用）
#[cfg(feature = "tar-zstd")]
const ZSTD_SIGNATURE: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// POSIX ustar の magic。tar ヘッダの offset 257 に "ustar" が入る。
#[cfg(feature = "fmt-tar")]
const USTAR_OFFSET: usize = 257;
#[cfg(feature = "fmt-tar")]
const USTAR_MAGIC: [u8; 5] = *b"ustar";
/// マジック読み取り長。ustar magic(offset 257)まで届くよう先頭ブロック分読む。
/// tar 無効時は先頭シグネチャ(最大6バイト)だけで足りる。
#[cfg(feature = "fmt-tar")]
const MAGIC_READ_LEN: usize = 512;
#[cfg(not(feature = "fmt-tar"))]
const MAGIC_READ_LEN: usize = 8;

/// ファイル先頭バイトを最大 MAGIC_READ_LEN バイト読む。開けない・空の場合は None。
fn read_magic(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; MAGIC_READ_LEN];
    let n = f.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    Some(buf[..n].to_vec())
}

/// マジックバイトからアーカイブ形式を判定する。判別できない場合は None。
fn detect_by_magic(buf: &[u8]) -> Option<ArchiveFormat> {
    #[cfg(feature = "fmt-7z")]
    if buf.len() >= SEVEN_Z_SIGNATURE.len() && buf[..SEVEN_Z_SIGNATURE.len()] == SEVEN_Z_SIGNATURE {
        return Some(ArchiveFormat::SevenZ);
    }
    if buf.len() >= 4 && ZIP_SIGNATURES.iter().any(|sig| &buf[..4] == sig) {
        return Some(ArchiveFormat::Zip);
    }
    #[cfg(feature = "fmt-tar")]
    {
        // gzip はこのアプリの文脈では tar.gz とみなす（単体 .gz は非対応スコープ）。
        if buf.len() >= GZIP_SIGNATURE.len() && buf[..GZIP_SIGNATURE.len()] == GZIP_SIGNATURE {
            return Some(ArchiveFormat::Tar);
        }
        // zstd フレームは tar.zst とみなす（単体 .zst は非対応スコープ）。
        #[cfg(feature = "tar-zstd")]
        if buf.len() >= ZSTD_SIGNATURE.len() && buf[..ZSTD_SIGNATURE.len()] == ZSTD_SIGNATURE {
            return Some(ArchiveFormat::Tar);
        }
        // raw tar は先頭マジックを持たず、offset 257 に ustar magic がある。
        if buf.len() >= USTAR_OFFSET + USTAR_MAGIC.len()
            && buf[USTAR_OFFSET..USTAR_OFFSET + USTAR_MAGIC.len()] == USTAR_MAGIC
        {
            return Some(ArchiveFormat::Tar);
        }
    }
    None
}

/// ファイル名の小文字サフィックスがいずれかに一致するか。
#[cfg(any(feature = "fmt-7z", feature = "fmt-tar"))]
fn name_ends_with_any(path: &Path, suffixes: &[&str]) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    suffixes.iter().any(|s| name.ends_with(s))
}

/// 拡張子からアーカイブ形式を推定する（マジック判定不能時のフォールバック）。
/// 7z/cb7 → SevenZ、tar系 → Tar、それ以外は Zip とみなす（従来の既定動作）。
fn detect_by_ext(path: &Path) -> ArchiveFormat {
    #[cfg(feature = "fmt-7z")]
    if name_ends_with_any(path, &[".7z", ".cb7"]) {
        return ArchiveFormat::SevenZ;
    }
    #[cfg(feature = "fmt-tar")]
    if name_ends_with_any(path, &[".tar", ".cbt", ".tar.gz", ".tgz"]) {
        return ArchiveFormat::Tar;
    }
    #[cfg(feature = "tar-zstd")]
    if name_ends_with_any(path, &[".tar.zst", ".tzst"]) {
        return ArchiveFormat::Tar;
    }
    let _ = path;
    ArchiveFormat::Zip
}

/// マジックバイト優先、読めない場合のみ拡張子でアーカイブ形式を判定する（キャッシュ無し）。
/// マジックで判定できれば拡張子偽装（.cbz に中身が別形式等）でも実体を優先できる。
fn detect_format_uncached(path: &Path) -> ArchiveFormat {
    if let Some(buf) = read_magic(path) {
        if let Some(f) = detect_by_magic(&buf) {
            return f;
        }
    }
    detect_by_ext(path)
}

/// ディレクトリ単位で世代管理するアーカイブ形式判定キャッシュ。
/// 呼び出しごとにファイルヘッダを読み直すコスト（特にネットワーク越しのパス）を避けるため、
/// path -> ArchiveFormat を覚えておく。無制限に肥大化しないよう、
/// 「直近何ディレクトリ分を覚えておくか（世代数）」と「総推定バイト数」の両方で頭打ちにする。
struct FormatCache {
    current_gen: u64,
    last_dir: Option<PathBuf>,
    total_bytes: usize,
    dirs: HashMap<PathBuf, DirGeneration>,
}

struct DirGeneration {
    generation: u64,
    entries: HashMap<PathBuf, ArchiveFormat>,
    bytes: usize,
}

/// 直近何ディレクトリ分の判定結果を保持するか
const FORMAT_CACHE_MAX_GENERATIONS: u64 = 15;
/// キャッシュ全体の推定使用量の上限（16MB。ファイル収集家がディレクトリに
/// 数十万ファイル溜め込んでいても頭打ちにするための安全弁）
const FORMAT_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// 1エントリあたりの固定オーバーヘッド見積もり（PathBuf構造体+ヒープ確保+HashMapスロット分）
const FORMAT_CACHE_ENTRY_OVERHEAD: usize = 64;

impl FormatCache {
    fn get_or_compute(&mut self, path: &Path) -> ArchiveFormat {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if self.last_dir.as_deref() != Some(dir.as_path()) {
            self.current_gen += 1;
            self.last_dir = Some(dir.clone());
            self.evict_old_generations();
        }
        let generation = self.current_gen;

        if let Some(g) = self.dirs.get(&dir) {
            if let Some(&format) = g.entries.get(path) {
                return format;
            }
        }

        let format = detect_format_uncached(path);
        let entry_bytes = path.as_os_str().len() + FORMAT_CACHE_ENTRY_OVERHEAD;
        let g = self.dirs.entry(dir).or_insert_with(|| DirGeneration {
            generation,
            entries: HashMap::new(),
            bytes: 0,
        });
        g.generation = generation;
        g.entries.insert(path.to_path_buf(), format);
        g.bytes += entry_bytes;
        self.total_bytes += entry_bytes;

        self.evict_over_budget();
        format
    }

    /// 最新世代から FORMAT_CACHE_MAX_GENERATIONS より古いディレクトリを丸ごと削除する
    fn evict_old_generations(&mut self) {
        let cutoff = self.current_gen.saturating_sub(FORMAT_CACHE_MAX_GENERATIONS);
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
        while self.total_bytes > FORMAT_CACHE_MAX_BYTES {
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

fn format_cache() -> &'static std::sync::Mutex<FormatCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<FormatCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(FormatCache {
            current_gen: 0,
            last_dir: None,
            total_bytes: 0,
            dirs: HashMap::new(),
        })
    })
}

/// アーカイブ形式を判定する。先頭バイトのマジック（7z/zip）を優先し、
/// 読めない・判別不能な場合のみ拡張子にフォールバックする。
/// 判定結果はディレクトリ単位・世代管理でキャッシュし、同じパスへの再判定でファイルI/Oを避ける。
pub fn detect_format(path: &Path) -> ArchiveFormat {
    format_cache().lock().unwrap().get_or_compute(path)
}

/// 7z/CB7 かどうかを判定する（`detect_format` の薄いラッパ）。
/// fmt-7z 無効時は常に false。
pub fn is_7z_path(path: &Path) -> bool {
    #[cfg(feature = "fmt-7z")]
    {
        detect_format(path) == ArchiveFormat::SevenZ
    }
    #[cfg(not(feature = "fmt-7z"))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detect_by_magic_recognizes_signatures() {
        assert_eq!(detect_by_magic(b"PK\x03\x04rest"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_by_magic(b"PK\x05\x06"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_by_magic(b"\xff\xd8\xff\xe0jpeg"), None); // JPEG等は非アーカイブ
        assert_eq!(detect_by_magic(b"PK"), None); // 短すぎるものは判別不能

        #[cfg(feature = "fmt-7z")]
        assert_eq!(detect_by_magic(&SEVEN_Z_SIGNATURE), Some(ArchiveFormat::SevenZ));

        #[cfg(feature = "fmt-tar")]
        {
            assert_eq!(detect_by_magic(b"\x1f\x8b\x08rest"), Some(ArchiveFormat::Tar)); // gzip(tar.gz)
            // raw tar: offset 257 に ustar magic
            let mut tar_head = vec![0u8; 512];
            tar_head[USTAR_OFFSET..USTAR_OFFSET + USTAR_MAGIC.len()].copy_from_slice(&USTAR_MAGIC);
            assert_eq!(detect_by_magic(&tar_head), Some(ArchiveFormat::Tar));

            #[cfg(feature = "tar-zstd")]
            assert_eq!(detect_by_magic(b"\x28\xb5\x2f\xfdrest"), Some(ArchiveFormat::Tar)); // zstd(tar.zst)
        }
    }

    fn write_temp(name: &str, head: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("nekoviewer_detect_{}_{n}_{name}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(head).unwrap();
        path
    }

    /// マジックバイトが拡張子より優先されること（.cbz偽装等への対応）を確認する。
    #[cfg(feature = "fmt-7z")]
    #[test]
    fn magic_overrides_misleading_extension() {
        // 中身がZIPなのに .7z 拡張子 → マジック優先でZip判定
        let zip_as_7z = write_temp("fake.7z", b"PK\x03\x04payload");
        assert_eq!(detect_format(&zip_as_7z), ArchiveFormat::Zip);

        // 中身が7zなのに .zip 拡張子 → マジック優先でSevenZ判定
        let sevenz_as_zip = write_temp("fake.zip", &SEVEN_Z_SIGNATURE);
        assert_eq!(detect_format(&sevenz_as_zip), ArchiveFormat::SevenZ);

        std::fs::remove_file(&zip_as_7z).ok();
        std::fs::remove_file(&sevenz_as_zip).ok();
    }

    #[test]
    fn falls_back_to_extension_when_magic_unknown() {
        // マジックで判別できない .cbz → Zip
        let unknown_cbz = write_temp("data.cbz", b"not-an-archive-header");
        assert_eq!(detect_format(&unknown_cbz), ArchiveFormat::Zip);
        std::fs::remove_file(&unknown_cbz).ok();

        #[cfg(feature = "fmt-7z")]
        {
            let unknown_7z = write_temp("data.7z", b"not-an-archive-header");
            assert_eq!(detect_format(&unknown_7z), ArchiveFormat::SevenZ);
            std::fs::remove_file(&unknown_7z).ok();
        }

        #[cfg(feature = "fmt-tar")]
        {
            // マジックを持たない古い tar / .cbt は拡張子で Tar 判定
            let unknown_tar = write_temp("data.tar", b"short-header");
            assert_eq!(detect_format(&unknown_tar), ArchiveFormat::Tar);
            let unknown_cbt = write_temp("data.cbt", b"short-header");
            assert_eq!(detect_format(&unknown_cbt), ArchiveFormat::Tar);
            std::fs::remove_file(&unknown_tar).ok();
            std::fs::remove_file(&unknown_cbt).ok();
        }
    }
}
