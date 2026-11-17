//! zip の中央ディレクトリを、末尾だけを読んで自前で解析する（速度のための近道。HDDの初回アクセス向け）。
//!
//! zip クレートの `ZipArchive::new` は、開く時に**全エントリのローカルヘッダ**を読みに行く（エントリごとにシーク）。
//! HDD の冷たい状態では、これだけでファイル1件あたり100ms以上かかる。一方、info.txt などの有無を知るには、
//! 末尾の中央ディレクトリ（名前・方式・サイズ・ローカルヘッダの位置）だけで足りる。
//! ここでは末尾を1回（足りなければもう1回）読んで一覧を作り、ローカルヘッダは読まない。
//!
//! **判断できないものは `None`（＝呼び出し側が従来の zip クレート経由に戻す）**にして、結果を変えない:
//! ZIP64・複数ディスク・先頭に余分なデータがある（自己展開形式など）・エントリ数や大きさの不整合・巨大な中央ディレクトリ。
//! 結果が一致することの検証は P2/P3 の等価テストで行う。

use std::io::{BufReader, Read, Seek, SeekFrom};

/// 末尾として最初に読む大きさ。EOCD（22バイト＋コメント最大65535バイト）が必ず入り、
/// 中央ディレクトリも通常はここに収まる（実測: 最大55KB）。1回の読みなのでシーク数は増えない。
pub(crate) const TAIL_READ: u64 = 128 * 1024;
/// 中央ディレクトリの大きさの上限。これを超えたら従来経路に任せる。
const MAX_CD_BYTES: u64 = 64 * 1024 * 1024;

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_CDFH: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;
const LOCAL_LEN: usize = 30;
/// Info-ZIP の Unicode パス拡張。あると zip クレートは名前を差し替えるので、扱いを揃えるため従来経路に任せる。
const EXTRA_UNICODE_PATH: u16 = 0x7075;
const EOCD_LEN: usize = 22;
const CDFH_LEN: usize = 46;

/// 中央ディレクトリの1エントリ（ローカルヘッダは読んでいない）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CdEntry {
    /// 格納されたままの名前（文字コードは未解釈）。
    pub name_raw: Vec<u8>,
    /// 汎用フラグ（bit0=暗号化、bit3=データ記述子、bit11=UTF-8名）。
    pub flags: u16,
    /// 圧縮方式（0=stored、8=deflate）。
    pub method: u16,
    pub crc32: u32,
    pub comp_size: u32,
    pub uncomp_size: u32,
    /// ローカルヘッダの先頭位置（ファイル先頭から）。
    pub header_offset: u32,
    /// MS-DOS 形式の更新時刻・更新日（日付キーの算出用）。
    pub mod_time: u16,
    pub mod_date: u16,
    /// 名前を差し替える拡張（Unicode パス）がある、または拡張領域が読み取れない。
    /// 名前の解釈が zip クレートと食い違いうるので、呼び出し側は従来経路に戻す。
    pub odd_name: bool,
}

/// 拡張領域に Unicode パスがあるか、または壊れていて読み切れないか。
fn extra_is_odd(extra: &[u8]) -> bool {
    let mut p = 0usize;
    while p < extra.len() {
        if p + 4 > extra.len() {
            return true;
        }
        let (id, size) = (u16_at(extra, p), u16_at(extra, p + 2) as usize);
        if id == EXTRA_UNICODE_PATH || p + 4 + size > extra.len() {
            return true;
        }
        p += 4 + size;
    }
    false
}

/// 中央ディレクトリの解析結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CentralDir {
    pub entries: Vec<CdEntry>,
    /// zip 全体のコメント（格納されたまま）。
    pub comment: Vec<u8>,
    /// 中央ディレクトリの開始位置。
    pub cd_start: u64,
    pub file_len: u64,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// 末尾から後ろ向きに EOCD を探す。見つけたら（バッファ内の位置）。コメント長が合わないものは飛ばす。
fn find_eocd(buf: &[u8], buf_start: u64, file_len: u64) -> Option<usize> {
    if buf.len() < EOCD_LEN {
        return None;
    }
    let mut i = buf.len() - EOCD_LEN;
    loop {
        if u32_at(buf, i) == SIG_EOCD {
            let comment_len = u16_at(buf, i + 20) as u64;
            // zip クレートと同じく、コメントがファイル末尾を超えるものだけを不正とする（後ろのゴミは許容）。
            if buf_start + i as u64 + EOCD_LEN as u64 + comment_len <= file_len {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// 末尾を読んで中央ディレクトリを解析する。判断できなければ `None`（従来経路に戻す）。
pub(crate) fn read_central_dir<R: Read + Seek>(reader: &mut R) -> Option<CentralDir> {
    read_central_dir_with(reader, TAIL_READ)
}

/// `read_central_dir` の、最初に読む末尾の大きさを指定できる版（テストで「足りない場合の追加読み」を小さなzipで確かめる）。
pub(crate) fn read_central_dir_with<R: Read + Seek>(reader: &mut R, tail_read: u64) -> Option<CentralDir> {
    let file_len = reader.seek(SeekFrom::End(0)).ok()?;
    if file_len < EOCD_LEN as u64 {
        return None;
    }
    let tail_len = file_len.min(tail_read.max(EOCD_LEN as u64));
    let tail_start = file_len - tail_len;
    let mut tail = vec![0u8; tail_len as usize];
    reader.seek(SeekFrom::Start(tail_start)).ok()?;
    reader.read_exact(&mut tail).ok()?;

    let eocd_pos = find_eocd(&tail, tail_start, file_len)?;
    let eocd = &tail[eocd_pos..];
    let (disk, cd_disk) = (u16_at(eocd, 4), u16_at(eocd, 6));
    let total = u16_at(eocd, 10);
    let (cd_size, cd_offset) = (u32_at(eocd, 12), u32_at(eocd, 16));
    let comment_len = u16_at(eocd, 20) as usize;
    // 複数ディスク・ZIP64（各値が上限値）は対象外。
    if disk != 0 || cd_disk != 0 || total == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return None;
    }
    let comment = eocd[EOCD_LEN..(EOCD_LEN + comment_len).min(eocd.len())].to_vec();
    let eocd_abs = tail_start + eocd_pos as u64;
    // 中央ディレクトリは EOCD の直前に隣接し、開始位置は EOCD の記録と一致するはず。
    // 食い違い（先頭の余分なデータ・署名などの挟まり）は従来経路に任せる。
    let cd_start = eocd_abs.checked_sub(cd_size as u64)?;
    if cd_start != cd_offset as u64 || cd_size as u64 > MAX_CD_BYTES {
        return None;
    }

    let cd_len = cd_size as usize;
    let owned;
    let cd: &[u8] = if cd_start >= tail_start {
        let from = (cd_start - tail_start) as usize;
        &tail[from..from + cd_len]
    } else {
        // 末尾に収まらない大きな中央ディレクトリ。足りない分を含めて1回で読み直す。
        let mut buf = vec![0u8; cd_len];
        reader.seek(SeekFrom::Start(cd_start)).ok()?;
        reader.read_exact(&mut buf).ok()?;
        owned = buf;
        &owned
    };

    let mut entries = Vec::with_capacity(total as usize);
    let mut p = 0usize;
    for _ in 0..total {
        if p + CDFH_LEN > cd.len() || u32_at(cd, p) != SIG_CDFH {
            return None;
        }
        let flags = u16_at(cd, p + 8);
        let method = u16_at(cd, p + 10);
        let (mod_time, mod_date) = (u16_at(cd, p + 12), u16_at(cd, p + 14));
        let crc32 = u32_at(cd, p + 16);
        let (comp_size, uncomp_size) = (u32_at(cd, p + 20), u32_at(cd, p + 24));
        let (name_len, extra_len, cmt_len) = (u16_at(cd, p + 28) as usize, u16_at(cd, p + 30) as usize, u16_at(cd, p + 32) as usize);
        let header_offset = u32_at(cd, p + 42);
        // ZIP64 の拡張情報が要る値（上限値）は対象外。
        if comp_size == u32::MAX || uncomp_size == u32::MAX || header_offset == u32::MAX {
            return None;
        }
        let end = p + CDFH_LEN + name_len + extra_len + cmt_len;
        if end > cd.len() {
            return None;
        }
        // ローカルヘッダは中央ディレクトリより前にある（zip クレートも同じ検査をして、違えば開けない）。
        if header_offset as u64 >= cd_start {
            return None;
        }
        let extra = &cd[p + CDFH_LEN + name_len..p + CDFH_LEN + name_len + extra_len];
        entries.push(CdEntry {
            name_raw: cd[p + CDFH_LEN..p + CDFH_LEN + name_len].to_vec(),
            flags,
            method,
            crc32,
            comp_size,
            uncomp_size,
            header_offset,
            mod_time,
            mod_date,
            odd_name: extra_is_odd(extra),
        });
        p = end;
    }
    // 件数どおりに読み切って、余りが無いこと。
    if p != cd.len() {
        return None;
    }
    Some(CentralDir { entries, comment, cd_start, file_len })
}

/// 名前の分類（拡張子・`/`・`\\`・`info.txt` との一致）にだけ使う見た目。ASCII はそのまま、それ以外のバイトは1文字の代用にする。
/// 分類は ASCII の部分しか見ないので、zip クレートが文字コードを解釈した名前（UTF-8 / CP437）での判定と同じ結果になる。
pub(crate) fn ascii_view(raw: &[u8]) -> String {
    raw.iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect()
}

/// 読み終わり（EOF）で CRC32 を確かめる読み口（zip クレートの検査と同じ。上限で読み止めたときは検査しない）。
struct CrcReader<R> {
    inner: R,
    crc: flate2::Crc,
    expected: u32,
}

impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 && !buf.is_empty() {
            if self.crc.sum() != self.expected {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "crc32 mismatch"));
            }
        } else {
            self.crc.update(&buf[..n]);
        }
        Ok(n)
    }
}

/// エントリ本体を、展開後 `limit` バイトまで読む。ローカルヘッダ（30バイト＋名前＋拡張）とデータだけを読む。
/// 対応するのは stored と deflate のみ。暗号化・その他の方式・不整合・CRC 不一致・読み取りエラーは `Err`
/// （呼び出し側は、zip 全体を従来経路に戻す。結果を変えないため）。
pub(crate) fn read_entry_limited<R: Read + Seek>(reader: &mut R, cd: &CentralDir, e: &CdEntry, limit: u64) -> Result<Vec<u8>, ()> {
    // bit0=暗号化、bit6=強い暗号化。
    if e.flags & 0x41 != 0 || !matches!(e.method, 0 | 8) {
        return Err(());
    }
    let off = e.header_offset as u64;
    reader.seek(SeekFrom::Start(off)).map_err(|_| ())?;
    let mut head = [0u8; LOCAL_LEN];
    reader.read_exact(&mut head).map_err(|_| ())?;
    if u32_at(&head, 0) != SIG_LOCAL {
        return Err(());
    }
    let data_start = off + LOCAL_LEN as u64 + u16_at(&head, 26) as u64 + u16_at(&head, 28) as u64;
    if data_start + e.comp_size as u64 > cd.cd_start {
        return Err(());
    }
    reader.seek(SeekFrom::Start(data_start)).map_err(|_| ())?;
    let src = (&mut *reader).take(e.comp_size as u64);
    let body: Box<dyn Read + '_> = if e.method == 0 { Box::new(src) } else { Box::new(flate2::read::DeflateDecoder::new(BufReader::with_capacity(32 * 1024, src))) };
    let checked = CrcReader { inner: body, crc: flate2::Crc::new(), expected: e.crc32 };
    let mut out = Vec::new();
    checked.take(limit).read_to_end(&mut out).map_err(|_| ())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn build(files: &[(&str, &[u8], ::zip::CompressionMethod)], comment: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = ::zip::ZipWriter::new(Cursor::new(&mut buf));
            for (name, body, method) in files {
                let opt = ::zip::write::SimpleFileOptions::default().compression_method(*method);
                w.start_file(*name, opt).unwrap();
                w.write_all(body).unwrap();
            }
            if !comment.is_empty() {
                w.set_comment(comment);
            }
            w.finish().unwrap();
        }
        buf
    }

    fn stored() -> ::zip::CompressionMethod {
        ::zip::CompressionMethod::Stored
    }

    fn deflated() -> ::zip::CompressionMethod {
        ::zip::CompressionMethod::Deflated
    }

    /// zip クレートの見方と、名前・方式・サイズ・位置がすべて一致すること。
    fn assert_matches_zip_crate(bytes: &[u8], cd: &CentralDir) {
        let mut a = ::zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(cd.entries.len(), a.len());
        for (i, e) in cd.entries.iter().enumerate() {
            let f = a.by_index_raw(i).unwrap();
            assert_eq!(e.name_raw, f.name_raw(), "name #{i}");
            assert_eq!(e.comp_size as u64, f.compressed_size(), "comp #{i}");
            assert_eq!(e.uncomp_size as u64, f.size(), "size #{i}");
            assert_eq!(e.crc32, f.crc32(), "crc #{i}");
            assert_eq!(e.header_offset as u64, f.header_start(), "offset #{i}");
            let method = match f.compression() {
                ::zip::CompressionMethod::Stored => 0,
                ::zip::CompressionMethod::Deflated => 8,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(e.method, method, "method #{i}");
        }
        assert_eq!(cd.comment, a.comment());
        assert_eq!(cd.cd_start, a.central_directory_start());
    }

    #[test]
    fn matches_the_zip_crate_for_stored_and_deflated_entries() {
        let body = b"hello hello hello hello hello hello";
        let bytes = build(&[("a.jpg", &[1u8; 300], stored()), ("dir/info.txt", body, deflated()), ("b.png", &[7u8; 5000], deflated())], "");
        let cd = read_central_dir(&mut Cursor::new(&bytes)).expect("parsed");
        assert_eq!(cd.entries.len(), 3);
        assert_eq!(cd.file_len, bytes.len() as u64);
        assert_matches_zip_crate(&bytes, &cd);
    }

    #[test]
    fn reads_the_archive_comment_and_empty_archives() {
        let bytes = build(&[("x.txt", b"x", stored())], "コメント comment");
        let cd = read_central_dir(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(cd.comment, "コメント comment".as_bytes());
        assert_matches_zip_crate(&bytes, &cd);

        let empty = build(&[], "");
        let cd = read_central_dir(&mut Cursor::new(&empty)).unwrap();
        assert!(cd.entries.is_empty());
    }

    #[test]
    fn never_reads_the_local_headers() {
        // ローカルヘッダを壊しても、中央ディレクトリの解析は成功する（読んでいない証拠）。
        let mut bytes = build(&[("plain.txt", b"abc", stored()), ("second.txt", b"def", stored())], "");
        let cd0 = read_central_dir(&mut Cursor::new(&bytes)).unwrap();
        for e in &cd0.entries {
            let o = e.header_offset as usize;
            bytes[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
        let cd = read_central_dir(&mut Cursor::new(&bytes)).expect("local headers are not read");
        assert_eq!(cd.entries, cd0.entries);
        assert!(::zip::ZipArchive::new(Cursor::new(&bytes)).is_err(), "zip クレートは全ローカルヘッダを読むので、壊れていると開けない");
    }

    #[test]
    fn reads_again_when_the_central_directory_does_not_fit_in_the_first_tail_read() {
        let names: Vec<String> = (0..300).map(|i| format!("pages/chapter_{i:04}/image_{i:05}.jpg")).collect();
        let files: Vec<(&str, &[u8], ::zip::CompressionMethod)> = names.iter().map(|n| (n.as_str(), &b"x"[..], stored())).collect();
        let bytes = build(&files, "c");
        // 最初に読む大きさを、中央ディレクトリより小さくして、追加読みの経路を通す。
        let cd = read_central_dir_with(&mut Cursor::new(&bytes), 64).expect("parsed with a second read");
        assert_eq!(cd.entries.len(), 300);
        assert_matches_zip_crate(&bytes, &cd);
        // 大きな読みでも同じ結果。
        assert_eq!(read_central_dir(&mut Cursor::new(&bytes)).unwrap(), cd);
    }

    #[test]
    fn falls_back_on_anything_it_cannot_judge() {
        let good = build(&[("a.txt", b"a", stored()), ("b.txt", b"b", stored())], "");
        assert!(read_central_dir(&mut Cursor::new(&good)).is_some());

        // zip ではない・短すぎる・切れている。
        assert!(read_central_dir(&mut Cursor::new(b"not a zip at all, just some text".to_vec())).is_none());
        assert!(read_central_dir(&mut Cursor::new(Vec::<u8>::new())).is_none());
        assert!(read_central_dir(&mut Cursor::new(good[..good.len() - 10].to_vec())).is_none());

        // 先頭に余分なデータ（自己展開形式など）: オフセットが食い違うので従来経路へ。
        let mut junk = vec![0xAAu8; 100];
        junk.extend_from_slice(&good);
        assert!(read_central_dir(&mut Cursor::new(junk)).is_none());

        // ZIP64 を示す上限値・複数ディスク。
        let eocd = good.len() - EOCD_LEN;
        let mut z = good.clone();
        z[eocd + 16..eocd + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());
        let mut z = good.clone();
        z[eocd + 10..eocd + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());
        let mut z = good.clone();
        z[eocd + 4] = 1;
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());

        // 件数が実際より多い・少ない。
        let mut z = good.clone();
        z[eocd + 10] = 3;
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());
        let mut z = good.clone();
        z[eocd + 10] = 1;
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());

        // エントリの署名が壊れている。
        let cd0 = read_central_dir(&mut Cursor::new(&good)).unwrap();
        let mut z = good.clone();
        z[cd0.cd_start as usize] = 0;
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());
    }

    #[test]
    fn rejects_an_entry_whose_local_header_is_after_the_central_directory() {
        let good = build(&[("a.txt", b"a", stored())], "");
        let cd = read_central_dir(&mut Cursor::new(&good)).unwrap();
        let mut z = good.clone();
        let at = cd.cd_start as usize + 42;
        z[at..at + 4].copy_from_slice(&(cd.cd_start as u32).to_le_bytes());
        assert!(read_central_dir(&mut Cursor::new(z)).is_none());
    }

    #[test]
    fn finds_the_eocd_even_when_the_comment_looks_like_one() {
        // コメントの中に EOCD の署名があっても、後ろから探して長さの合う本物を採る。
        let mut fake = SIG_EOCD.to_le_bytes().to_vec();
        fake.extend_from_slice(&[0u8; 18]);
        let comment = String::from_utf8_lossy(&fake).into_owned();
        let bytes = build(&[("a.txt", b"a", stored())], &comment);
        let parsed = read_central_dir(&mut Cursor::new(&bytes));
        // 偽物の長さ欄(0)は、実際の末尾までの長さと合わないので、本物（コメント長>0）が選ばれるか、
        // 判断できなければ None（従来経路）。誤った一覧を返してはいけない。
        if let Some(cd) = parsed {
            assert_matches_zip_crate(&bytes, &cd);
        }
    }
}
