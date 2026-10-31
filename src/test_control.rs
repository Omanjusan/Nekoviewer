//! 自動テスト用の制御口（Cargo feature `test-control`、通常ビルドには入らない）。
//! Wayland など窓位置を操作できない環境でも、Xephyr 上の実アプリに対して「窓を動かす・スロットを
//! 記録/展開/削除する・状態をダンプする」をスクリプトから行うためのもの。
//!
//! 使い方: 環境変数 `NEKO_TEST_CONTROL=<ディレクトリ>` を付けて起動し、そのディレクトリに
//! `cmd.txt`（1行1コマンド）を原子的に置く。アプリが読み取って削除し、順に実行する。
//! `dump` は同ディレクトリの `dump.json` に状態を書き出す。
//! `NEKO_FAKE_MONITORS="x,y,w,h;x,y,w,h"` でモニター一覧を差し替えられる（論理座標）。

use std::path::PathBuf;

use crate::monitor_geom::MonitorRect;

/// スロット番号はコマンド上 1 始まり（F5=1）。内部では 0 始まりに直して持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// ビューアー窓を物理座標で動かす・リサイズする（画面外へ飛ばす用途）
    Move { x: i32, y: i32, w: u32, h: u32 },
    /// メニューバーの「ビューアー復帰」と同じ
    Recover,
    Apply(usize),
    Record(usize),
    Delete(usize),
    /// 「開いたとき自動展開」。None = 全チェックを外す
    Auto(Option<usize>),
    Dump,
}

fn slot_arg(s: &str) -> Option<usize> {
    let n: usize = s.parse().ok()?;
    (1..=4).contains(&n).then(|| n - 1)
}

pub fn parse_line(line: &str) -> Option<Cmd> {
    let mut it = line.split_whitespace();
    let cmd = it.next()?;
    let args: Vec<&str> = it.collect();
    match (cmd, args.as_slice()) {
        ("move", [x, y, w, h]) => Some(Cmd::Move {
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            w: w.parse().ok()?,
            h: h.parse().ok()?,
        }),
        ("recover", []) => Some(Cmd::Recover),
        ("apply", [n]) => slot_arg(n).map(Cmd::Apply),
        ("record", [n]) => slot_arg(n).map(Cmd::Record),
        ("delete", [n]) => slot_arg(n).map(Cmd::Delete),
        ("auto", ["none"]) => Some(Cmd::Auto(None)),
        ("auto", [n]) => slot_arg(n).map(|i| Cmd::Auto(Some(i))),
        ("dump", []) => Some(Cmd::Dump),
        _ => None,
    }
}

/// `"0,0,1920,1080;1920,0,1920,1080"` 形式。不正な要素は無視する。左→右・上→下に並べ替える。
pub fn parse_fake_monitors(s: &str) -> Vec<MonitorRect> {
    let mut v: Vec<MonitorRect> = s
        .split(';')
        .filter_map(|m| {
            let p: Vec<&str> = m.trim().split(',').collect();
            if let [x, y, w, h] = p.as_slice() {
                Some(MonitorRect { x: x.trim().parse().ok()?, y: y.trim().parse().ok()?, w: w.trim().parse().ok()?, h: h.trim().parse().ok()? })
            } else {
                None
            }
        })
        .collect();
    crate::monitor_geom::sort_for_numbering(&mut v);
    v
}

/// `NEKO_FAKE_MONITORS` が設定されていればその一覧。
pub fn fake_monitors() -> Option<Vec<MonitorRect>> {
    let v = parse_fake_monitors(&std::env::var("NEKO_FAKE_MONITORS").ok()?);
    (!v.is_empty()).then_some(v)
}

pub struct Control {
    dir: PathBuf,
}

impl Control {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("NEKO_TEST_CONTROL")?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    /// `cmd.txt` があれば読んで削除し、解釈できた行をコマンドとして返す。
    pub fn take_commands(&self) -> Vec<Cmd> {
        let path = self.dir.join("cmd.txt");
        let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
        let _ = std::fs::remove_file(&path);
        text.lines().filter_map(parse_line).collect()
    }

    pub fn write_dump(&self, json: &str) {
        let tmp = self.dir.join("dump.json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, self.dir.join("dump.json"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_with_one_based_slots() {
        assert_eq!(parse_line("apply 1"), Some(Cmd::Apply(0)));
        assert_eq!(parse_line("record 4"), Some(Cmd::Record(3)));
        assert_eq!(parse_line("delete 2"), Some(Cmd::Delete(1)));
        assert_eq!(parse_line("auto 3"), Some(Cmd::Auto(Some(2))));
        assert_eq!(parse_line("auto none"), Some(Cmd::Auto(None)));
        assert_eq!(parse_line("move -5000 100 800 600"), Some(Cmd::Move { x: -5000, y: 100, w: 800, h: 600 }));
        assert_eq!(parse_line("recover"), Some(Cmd::Recover));
        assert_eq!(parse_line("dump"), Some(Cmd::Dump));
    }

    #[test]
    fn rejects_bad_lines() {
        assert_eq!(parse_line("apply 0"), None);
        assert_eq!(parse_line("apply 5"), None);
        assert_eq!(parse_line("apply"), None);
        assert_eq!(parse_line("move 1 2 3"), None);
        assert_eq!(parse_line("bogus"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn fake_monitors_are_parsed_and_sorted_skipping_garbage() {
        let v = parse_fake_monitors("1920,0,1920,1080; 0,0,1920,1080;oops;1,2,3");
        assert_eq!(v, vec![MonitorRect { x: 0, y: 0, w: 1920, h: 1080 }, MonitorRect { x: 1920, y: 0, w: 1920, h: 1080 }]);
        assert!(parse_fake_monitors("").is_empty());
    }
}
