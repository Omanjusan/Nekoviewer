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
    /// 「次回起動時に X11 で起動する」の保存値を直接変える（メニューのチェック操作と同じ保存経路）
    LaunchX11(bool),
    /// ビューアー窓の OS クローズ（X ボタン）と同じ処理
    Close,
    Dump,
}

/// 左ペインのタブ（`tab` コマンドの引数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabArg {
    Real,
    Favorites,
    Search,
    Virtual,
    Tag,
}

/// エクスプローラー側（検索タブなど）を操作するコマンド。ビューアーのスロット操作とは別に扱う。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExploreCmd {
    /// 実フォルダを開く（`open <パス>`。パスは行の残り全部）
    Open(PathBuf),
    Tab(TabArg),
    /// 検索モード。true=新、false=旧
    SearchMode(bool),
    /// 新モードの検索文字列（`search-text <文字列>`。行の残り全部。空も可）
    SearchText(String),
    /// 新モードのサイズ（MB）。空は `-`
    SearchSize { min: String, max: String },
    SearchStart,
    SearchCancel,
    /// 新モードの「注意点」ダイアログを開く（ボタンを押すのと同じ）
    SearchNotice,
    /// 表示モード。true=リスト、false=カード
    ListMode(bool),
    /// 一覧の idx 番目のファイルについて、リストの「実フォルダ」アイコンを押したのと同じ移動をする
    GotoReal(usize),
    /// 実フォルダを、仮想ルート直下に登録する（名前はフォルダ名）。仮想フォルダ列のテスト用
    VirtualAdd(PathBuf),
    /// 一覧の idx 番目のファイルについて、「仮想フォルダ」アイコンを押したのと同じ移動をする。
    /// 登録が複数でメニューが出たときは、`pick`（0始まり）の項目を押したことにする。
    GotoVirtual { idx: usize, pick: Option<usize> },
    /// 履歴の idx 番目（0 始まり）を選んで表示する
    SearchSelect(usize),
}

/// `cmd.txt` の1行の解釈結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Window(Cmd),
    Explore(ExploreCmd),
}

/// 行を「コマンド語」と「残り全部」に分ける（残りは空白を保つ。先頭の区切り1つだけ除く）。
fn split_word(line: &str) -> (&str, &str) {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest),
        None => (line, ""),
    }
}

pub fn parse_explore_line(line: &str) -> Option<ExploreCmd> {
    let (word, rest) = split_word(line.trim_start());
    let arg = rest.trim();
    match word {
        "open" => (!arg.is_empty()).then(|| ExploreCmd::Open(PathBuf::from(arg))),
        "tab" => Some(ExploreCmd::Tab(match arg {
            "real" => TabArg::Real,
            "favorites" => TabArg::Favorites,
            "search" => TabArg::Search,
            "virtual" => TabArg::Virtual,
            "tag" => TabArg::Tag,
            _ => return None,
        })),
        "search-mode" => match arg {
            "new" => Some(ExploreCmd::SearchMode(true)),
            "legacy" => Some(ExploreCmd::SearchMode(false)),
            _ => None,
        },
        "search-text" => Some(ExploreCmd::SearchText(rest.to_string())),
        "search-size" => {
            let mut it = arg.split_whitespace();
            let (min, max) = (it.next()?, it.next()?);
            if it.next().is_some() {
                return None;
            }
            let norm = |s: &str| if s == "-" { String::new() } else { s.to_string() };
            Some(ExploreCmd::SearchSize { min: norm(min), max: norm(max) })
        }
        "search-start" if arg.is_empty() => Some(ExploreCmd::SearchStart),
        "search-cancel" if arg.is_empty() => Some(ExploreCmd::SearchCancel),
        "search-notice" if arg.is_empty() => Some(ExploreCmd::SearchNotice),
        "goto-real" => arg.parse().ok().map(ExploreCmd::GotoReal),
        "virtual-add" => (!arg.is_empty()).then(|| ExploreCmd::VirtualAdd(PathBuf::from(arg))),
        "goto-virtual" => {
            let mut it = arg.split_whitespace();
            let idx = it.next()?.parse().ok()?;
            let pick = match it.next() {
                Some(p) => Some(p.parse().ok()?),
                None => None,
            };
            if it.next().is_some() {
                return None;
            }
            Some(ExploreCmd::GotoVirtual { idx, pick })
        }
        "view-mode" => match arg {
            "list" => Some(ExploreCmd::ListMode(true)),
            "card" => Some(ExploreCmd::ListMode(false)),
            _ => None,
        },
        "search-select" => arg.parse().ok().map(ExploreCmd::SearchSelect),
        _ => None,
    }
}

/// 1行を、エクスプローラー側→ビューアー側の順に解釈する。
pub fn parse_any(line: &str) -> Option<Parsed> {
    parse_explore_line(line).map(Parsed::Explore).or_else(|| parse_line(line).map(Parsed::Window))
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
        ("launchx11", [v]) => match *v { "1" | "on" => Some(Cmd::LaunchX11(true)), "0" | "off" => Some(Cmd::LaunchX11(false)), _ => None },
        ("close", []) => Some(Cmd::Close),
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
    pub fn take_commands(&self) -> Vec<Parsed> {
        let path = self.dir.join("cmd.txt");
        let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
        let _ = std::fs::remove_file(&path);
        text.lines().filter_map(parse_any).collect()
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
        assert_eq!(parse_line("close"), Some(Cmd::Close));
        assert_eq!(parse_line("launchx11 1"), Some(Cmd::LaunchX11(true)));
        assert_eq!(parse_line("launchx11 off"), Some(Cmd::LaunchX11(false)));
        assert_eq!(parse_line("launchx11 maybe"), None);
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

    #[test]
    fn parses_explorer_commands_keeping_spaces_in_the_rest_of_the_line() {
        assert_eq!(parse_explore_line("open /tmp/my dir/x"), Some(ExploreCmd::Open(PathBuf::from("/tmp/my dir/x"))));
        assert_eq!(parse_explore_line("open"), None);
        assert_eq!(parse_explore_line("tab search"), Some(ExploreCmd::Tab(TabArg::Search)));
        assert_eq!(parse_explore_line("tab real"), Some(ExploreCmd::Tab(TabArg::Real)));
        assert_eq!(parse_explore_line("tab nope"), None);
        assert_eq!(parse_explore_line("search-mode new"), Some(ExploreCmd::SearchMode(true)));
        assert_eq!(parse_explore_line("search-mode legacy"), Some(ExploreCmd::SearchMode(false)));
        assert_eq!(parse_explore_line("search-mode x"), None);
        assert_eq!(
            parse_explore_line("search-text a \"b c\"  d"),
            Some(ExploreCmd::SearchText("a \"b c\"  d".to_string()))
        );
        assert_eq!(parse_explore_line("search-text"), Some(ExploreCmd::SearchText(String::new())));
        assert_eq!(
            parse_explore_line("search-size 1.5 -"),
            Some(ExploreCmd::SearchSize { min: "1.5".to_string(), max: String::new() })
        );
        assert_eq!(parse_explore_line("search-size 1"), None);
        assert_eq!(parse_explore_line("search-start"), Some(ExploreCmd::SearchStart));
        assert_eq!(parse_explore_line("search-start now"), None);
        assert_eq!(parse_explore_line("search-cancel"), Some(ExploreCmd::SearchCancel));
        assert_eq!(parse_explore_line("search-notice"), Some(ExploreCmd::SearchNotice));
        assert_eq!(parse_explore_line("goto-real 3"), Some(ExploreCmd::GotoReal(3)));
        assert_eq!(parse_explore_line("goto-real x"), None);
        assert_eq!(parse_explore_line("virtual-add /tmp/my dir"), Some(ExploreCmd::VirtualAdd(PathBuf::from("/tmp/my dir"))));
        assert_eq!(parse_explore_line("virtual-add"), None);
        assert_eq!(parse_explore_line("goto-virtual 4"), Some(ExploreCmd::GotoVirtual { idx: 4, pick: None }));
        assert_eq!(parse_explore_line("goto-virtual 4 1"), Some(ExploreCmd::GotoVirtual { idx: 4, pick: Some(1) }));
        assert_eq!(parse_explore_line("goto-virtual 4 x"), None);
        assert_eq!(parse_explore_line("goto-virtual 4 1 2"), None);
        assert_eq!(parse_explore_line("search-select 2"), Some(ExploreCmd::SearchSelect(2)));
        assert_eq!(parse_explore_line("search-select x"), None);
    }

    #[test]
    fn parse_any_routes_explorer_and_viewer_commands() {
        assert_eq!(parse_any("tab tag"), Some(Parsed::Explore(ExploreCmd::Tab(TabArg::Tag))));
        assert_eq!(parse_any("apply 2"), Some(Parsed::Window(Cmd::Apply(1))));
        assert_eq!(parse_any("dump"), Some(Parsed::Window(Cmd::Dump)));
        assert_eq!(parse_any("bogus"), None);
    }
}
