//! タグ自動抽出の速度調査用の計測（開発用。設定の「perfログ」がONのときだけ標準エラーへ出す）。
//!
//! 走査の1件ごとに、段階別の時間（stat・マジック判定・zipを開く・名前の分類・本体読み・DB読み）と、
//! zipの大きさ・中央ディレクトリの大きさ・読んだエントリの位置を集め、走査の終わりに1回だけ集計して出す。
//! 目的は「HDDのシークを減らす案（先頭と末尾だけ読む・物理順で処理）が効くか」の判断材料を取ること。
//! 冷たいキャッシュ（初回）と温かいキャッシュ（直後の再実行）を見比べる。

use std::time::Duration;

use crate::fs::archive::meta::ZipTiming;

/// 先頭側として扱う範囲（先頭からここまでに収まるエントリは「先頭」）。
pub(super) const HEAD_BYTES: u64 = 64 * 1024;
/// 末尾側として扱う範囲（中央ディレクトリの手前、ここまでに始まるエントリは「末尾」）。
pub(super) const TAIL_MARGIN: u64 = 64 * 1024;

/// 1件ぶんの計測。
#[derive(Clone, Debug, Default)]
pub(super) struct ScanPerf {
    pub stat: Duration,
    pub magic: Duration,
    /// zip として開けて読めたときだけ入る。
    pub zip: Option<ZipTiming>,
    /// zip の読み全体（マジック判定を除く。開けなかった場合の時間も含む）。
    pub bundle: Duration,
    pub db: Duration,
    pub total: Duration,
}

/// 本体を読んだエントリの位置の分類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    /// 読んだエントリが無い（info.txt 等が無い）。
    None,
    /// すべて先頭 `HEAD_BYTES` に収まる。
    Head,
    /// すべて中央ディレクトリの手前 `TAIL_MARGIN` 以内から始まる。
    Tail,
    /// 先頭にも末尾にも収まらないものがある。
    Mid,
}

pub(super) fn classify(z: &ZipTiming) -> Place {
    if z.ranges.is_empty() {
        return Place::None;
    }
    if z.ranges.iter().all(|&(_, end)| end <= HEAD_BYTES) {
        return Place::Head;
    }
    let tail_from = z.cd_start.saturating_sub(TAIL_MARGIN);
    if z.ranges.iter().all(|&(start, _)| start >= tail_from) {
        return Place::Tail;
    }
    // 先頭と末尾の混在は、どちらか一方に寄せられないので Mid とする（追加の読みが要る）。
    Place::Mid
}

/// 昇順に並べた値の百分位（nearest-rank）。空なら 0。
fn pct<T: Copy + Default>(sorted: &[T], p: usize) -> T {
    if sorted.is_empty() {
        return T::default();
    }
    let idx = (sorted.len() * p).div_ceil(100).saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn kb(b: u64) -> f64 {
    b as f64 / 1024.0
}

/// 集計して出力用の行にする。
pub(super) fn render(perfs: &[ScanPerf], scan_wall: Duration, cand_wall: Option<Duration>) -> Vec<String> {
    let n = perfs.len();
    let mut out = Vec::new();
    out.push(format!("[perf/tag-scan] files={n} scan_wall={:.2}s candidates_wall={}", scan_wall.as_secs_f64(), cand_wall.map_or("-".to_string(), |d| format!("{:.2}s", d.as_secs_f64()))));
    if n == 0 {
        return out;
    }
    let sum = |f: &dyn Fn(&ScanPerf) -> Duration| perfs.iter().map(f).sum::<Duration>();
    let total = sum(&|p| p.total);
    let share = |d: Duration| if total.is_zero() { 0.0 } else { d.as_secs_f64() / total.as_secs_f64() * 100.0 };
    let zips: Vec<&ZipTiming> = perfs.iter().filter_map(|p| p.zip.as_ref()).collect();
    let open = zips.iter().map(|z| z.open).sum::<Duration>();
    let tail = zips.iter().map(|z| z.tail).sum::<Duration>();
    let parse = zips.iter().map(|z| z.parse).sum::<Duration>();
    let list = zips.iter().map(|z| z.list).sum::<Duration>();
    let read = zips.iter().map(|z| z.read).sum::<Duration>();
    let stat = sum(&|p| p.stat);
    let magic = sum(&|p| p.magic);
    let db = sum(&|p| p.db);
    // zipを開けなかった件の時間は、bundle の合計から zip の内訳を引いた残りに入る。
    let bundle_other = sum(&|p| p.bundle).saturating_sub(open + tail + parse + list + read);
    // 上のどれにも入らない時間（`scan_one` の中の、計測の外の処理）。大きければ、計測の外に重い処理が隠れている。
    let unattributed = total.saturating_sub(stat + magic + sum(&|p| p.bundle) + db);
    out.push(format!(
        "  段階別の合計(全体に対する割合): stat={:.2}s({:.0}%) magic={:.2}s({:.0}%) file_open={:.2}s({:.0}%) tail_raw={:.2}s({:.0}%) zip_parse={:.2}s({:.0}%) name_list={:.2}s({:.0}%) entry_read={:.2}s({:.0}%) zip_other={:.2}s({:.0}%) db={:.2}s({:.0}%) 計測外={:.2}s({:.0}%)",
        stat.as_secs_f64(), share(stat),
        magic.as_secs_f64(), share(magic),
        open.as_secs_f64(), share(open),
        tail.as_secs_f64(), share(tail),
        parse.as_secs_f64(), share(parse),
        list.as_secs_f64(), share(list),
        read.as_secs_f64(), share(read),
        bundle_other.as_secs_f64(), share(bundle_other),
        db.as_secs_f64(), share(db),
        unattributed.as_secs_f64(), share(unattributed),
    ));
    let mut per: Vec<Duration> = perfs.iter().map(|p| p.total).collect();
    per.sort();
    out.push(format!(
        "  1件あたり ms: avg={:.1} p50={:.1} p90={:.1} p99={:.1} max={:.1}",
        ms(total) / n as f64,
        ms(pct(&per, 50)),
        ms(pct(&per, 90)),
        ms(pct(&per, 99)),
        ms(per[n - 1]),
    ));
    if zips.is_empty() {
        out.push("  zipとして読めた件: 0".to_string());
        return out;
    }
    let mut sizes: Vec<u64> = zips.iter().map(|z| z.file_size).collect();
    sizes.sort();
    let mut cds: Vec<u64> = zips.iter().map(|z| z.file_size.saturating_sub(z.cd_start)).collect();
    cds.sort();
    let mut ents: Vec<usize> = zips.iter().map(|z| z.entries).collect();
    ents.sort();
    out.push(format!(
        "  zip {}件: サイズ MB min={:.1} p50={:.1} p90={:.1} max={:.1} 合計={:.0} / 末尾(中央ディレクトリ〜) KB p50={:.0} p90={:.0} max={:.0} / エントリ数 p50={} p90={} max={}",
        zips.len(),
        mb(sizes[0]), mb(pct(&sizes, 50)), mb(pct(&sizes, 90)), mb(sizes[sizes.len() - 1]), mb(sizes.iter().sum()),
        kb(pct(&cds, 50)), kb(pct(&cds, 90)), kb(cds[cds.len() - 1]),
        pct(&ents, 50), pct(&ents, 90), ents[ents.len() - 1],
    ));
    let count = |p: Place| zips.iter().filter(|z| classify(z) == p).count();
    out.push(format!(
        "  読んだエントリの位置(先頭{}KB / 末尾{}KB以内): 無し={} 先頭={} 末尾={} 中間にある={}  ※「中間にある」は先頭+末尾読みでは取りこぼす件数",
        HEAD_BYTES / 1024,
        TAIL_MARGIN / 1024,
        count(Place::None),
        count(Place::Head),
        count(Place::Tail),
        count(Place::Mid),
    ));
    out.push("  見方: 同じフォルダを「初回」と「直後の再実行」で比べる。file_open=ファイルを開く(inode引き) / tail_raw=末尾64KBの生read / zip_parse=解析(tail_rawで温まった後)。初回で file_open が大きければメタデータ待ち、tail_raw が大きければ末尾の読み待ち。".to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(file_size: u64, cd_start: u64, ranges: &[(u64, u64)]) -> ZipTiming {
        ZipTiming { file_size, cd_start, ranges: ranges.to_vec(), ..Default::default() }
    }

    #[test]
    fn classify_places_entries_by_head_tail_and_middle() {
        let big = 100 * 1024 * 1024;
        assert_eq!(classify(&z(big, big - 1000, &[])), Place::None);
        assert_eq!(classify(&z(big, big - 1000, &[(0, 2_000)])), Place::Head);
        assert_eq!(classify(&z(big, big - 1000, &[(big - 5_000, big - 1_500)])), Place::Tail);
        assert_eq!(classify(&z(big, big - 1000, &[(50 * 1024 * 1024, 50 * 1024 * 1024 + 100)])), Place::Mid);
        // 先頭と末尾の混在は中間扱い（取りこぼす側に数える）。
        assert_eq!(classify(&z(big, big - 1000, &[(0, 100), (big - 5_000, big - 1_500)])), Place::Mid);
        // 小さいzipは全体が先頭に収まる。
        assert_eq!(classify(&z(30_000, 29_000, &[(0, 500)])), Place::Head);
    }

    #[test]
    fn percentile_handles_edges() {
        assert_eq!(pct::<u64>(&[], 50), 0);
        assert_eq!(pct(&[7u64], 99), 7);
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(pct(&v, 50), 50);
        assert_eq!(pct(&v, 90), 90);
        assert_eq!(pct(&v, 100), 100);
    }

    #[test]
    fn render_summarizes_without_panicking_on_empty_and_mixed_input() {
        assert_eq!(render(&[], Duration::from_secs(1), None).len(), 1);
        let mut zt = z(10_000_000, 9_990_000, &[(0, 300)]);
        zt.open = Duration::from_millis(12);
        zt.entries = 40;
        let perfs = vec![
            ScanPerf { total: Duration::from_millis(20), zip: Some(zt), ..Default::default() },
            ScanPerf { total: Duration::from_millis(3), ..Default::default() },
        ];
        let lines = render(&perfs, Duration::from_millis(30), Some(Duration::from_millis(5)));
        assert!(lines[0].contains("files=2"));
        assert!(lines.iter().any(|l| l.contains("zip 1件")));
        assert!(lines.iter().any(|l| l.contains("先頭=1")));
    }
}
