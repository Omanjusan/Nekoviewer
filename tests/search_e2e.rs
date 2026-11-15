//! 検索タブ「新」モード（台帳の全件走査）の自動テスト（実アプリ + Xephyr）。
//!
//! 実アプリに対して制御口（`test-control` feature、NEKO_TEST_CONTROL）から
//! 「フォルダを開く → タブ切替 → 検索文字列・サイズ → 開始／中断 → 履歴の選択」を行い、
//! `dump.json` の `app.search` で、履歴・進捗・表示中の一覧・台帳の件数を観測する。
//! 台帳（ID）は、フォルダを一覧表示したときに実アプリのワーカーが振る（実際の経路どおり）。
//! Xephyr が無い環境では各テストは何もせず成功扱いで抜ける。
//!
//! 実行: `cargo test --features test-control --test search_e2e -- --ignored --test-threads=1`
//! （Xephyr とGPU描画が要るため既定では ignore。実データには触れず、HOME/XDG_* は一時ディレクトリ。）
//! 窓位置のE2E（window_pos_e2e.rs）と同じ作りで、検索用に必要な分だけを持つ。
#![cfg(all(unix, feature = "test-control"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

static DISPLAY_SEQ: AtomicU32 = AtomicU32::new(0);

fn xephyr_available() -> bool {
    Command::new("Xephyr").arg("-help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

fn skip() -> bool {
    if xephyr_available() {
        return false;
    }
    eprintln!("Xephyr not found: skipping");
    true
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 1テスト分の環境: 専用 Xephyr と、隔離した HOME/XDG_*/制御ディレクトリ、検索対象の実ファイル。
struct Env {
    root: PathBuf,
    display: String,
    _xephyr: Guard,
    app: Option<Guard>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.app = None;
        if std::env::var_os("NEKO_E2E_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl Env {
    fn new(name: &str) -> Self {
        let num = loop {
            let n = 60 + DISPLAY_SEQ.fetch_add(1, Ordering::SeqCst) % 400;
            if !Path::new(&format!("/tmp/.X{n}-lock")).exists() && !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
                break n;
            }
        };
        let display = format!(":{num}");
        let root = std::env::temp_dir().join(format!("nekoviewer-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home", "run", "ctl", "pics", "files"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // 起動時に開く画像（ビューアーが出る）。検索対象のフォルダ（files）とは別にしておく。
        image::RgbImage::from_pixel(64, 48, image::Rgb([200, 60, 60])).save(root.join("pics/p0.png")).unwrap();
        let xephyr = Command::new("Xephyr")
            .args([&display, "-screen", "1920x1080", "-no-host-grab", "+extension", "RANDR"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xephyr");
        let sock = PathBuf::from(format!("/tmp/.X11-unix/X{num}"));
        assert!(
            wait_until(Duration::from_secs(10), || std::os::unix::net::UnixStream::connect(&sock).is_ok()),
            "Xephyr did not come up"
        );
        Self { root, display, _xephyr: Guard(xephyr), app: None }
    }

    fn ctl(&self) -> PathBuf {
        self.root.join("ctl")
    }

    fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    /// 検索対象のファイルを作る。名前は `{prefix}_{番号:03}.png`。内容は番号ごとに違う（FPが重ならない）。
    /// `side` を大きくするとファイルサイズが大きくなる（サイズ条件の確認用）。
    fn make_files(&self, prefix: &str, count: usize, side: u32) {
        for i in 0..count {
            let seed = (prefix.len() as u32).wrapping_mul(31).wrapping_add(i as u32);
            let img = image::RgbImage::from_fn(side, side, |x, y| {
                image::Rgb([(x.wrapping_add(seed) % 251) as u8, (y.wrapping_mul(7).wrapping_add(seed) % 241) as u8, (seed % 233) as u8])
            });
            img.save(self.files_dir().join(format!("{prefix}_{i:03}.png"))).unwrap();
        }
    }

    fn launch(&mut self) {
        self.app = None;
        let _ = std::fs::remove_file(self.ctl().join("dump.json"));
        let h = self.root.join("home");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nekoviewer"));
        cmd.arg(self.root.join("pics/p0.png"))
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_SESSION_TYPE", "x11")
            .env("DISPLAY", &self.display)
            .env("HOME", &h)
            .env("XDG_CONFIG_HOME", h.join("c"))
            .env("XDG_DATA_HOME", h.join("d"))
            .env("XDG_CACHE_HOME", h.join("k"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("NEKO_TEST_CONTROL", self.ctl())
            .env("NEKO_FAKE_MONITORS", "0,0,1920,1080")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.root.join("app.log")).unwrap());
        self.app = Some(Guard(cmd.spawn().expect("spawn nekoviewer")));
        assert!(
            wait_until(Duration::from_secs(40), || self.try_dump().is_some_and(|d| !d["app"]["search"].is_null())),
            "app did not come up"
        );
    }

    fn send(&self, lines: &[&str]) {
        let tmp = self.ctl().join("cmd.tmp");
        std::fs::write(&tmp, lines.join("\n")).unwrap();
        std::fs::rename(&tmp, self.ctl().join("cmd.txt")).unwrap();
        let consumed = wait_until(Duration::from_secs(10), || !self.ctl().join("cmd.txt").exists());
        if !consumed {
            let log = std::fs::read_to_string(self.root.join("app.log")).unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(15).collect();
            panic!("command not consumed ({lines:?}); app.log tail:\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
        }
    }

    fn try_dump(&self) -> Option<Value> {
        let p = self.ctl().join("dump.json");
        let _ = std::fs::remove_file(&p);
        self.send(&["dump"]);
        let mut out = None;
        wait_until(Duration::from_secs(5), || {
            out = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok());
            out.is_some()
        });
        out
    }

    /// `app.search`（検索タブ・履歴・台帳の状態）。
    fn search(&self) -> Value {
        self.try_dump().expect("dump.json was not written")["app"]["search"].clone()
    }

    /// 条件を満たすまで dump を取り直して待つ。満たせなければ最後の状態つきで panic。
    fn wait_search(&self, what: &str, timeout: Duration, cond: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + timeout;
        loop {
            let s = self.search();
            if cond(&s) {
                return s;
            }
            if Instant::now() > end {
                panic!("timeout waiting for: {what}\nlast search state: {}", serde_json::to_string_pretty(&s).unwrap());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// フォルダを開き、台帳（ID）がファイル数以上に増えるまで待つ。
    fn open_and_wait_ledger(&self, expect_at_least: u64) {
        let open = format!("open {}", self.files_dir().display());
        self.send(&["tab real", &open]);
        self.wait_search("ledger filled", Duration::from_secs(60), |s| s["ledger_total"].as_u64().unwrap_or(0) >= expect_at_least);
    }
}

fn hits(entry: &Value) -> u64 {
    entry["hits"].as_u64().unwrap()
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn new_search_finds_files_by_name_and_narrows_by_size() {
    if skip() { return; }
    let mut e = Env::new("search-basic");
    e.make_files("alpha", 40, 16);
    e.make_files("beta", 25, 16);
    e.make_files("gamma", 5, 220); // 大きめ（サイズ条件の確認用）
    e.launch();
    e.open_and_wait_ledger(70);

    // 検索タブへ。既定は新モード
    e.send(&["tab search"]);
    let s = e.search();
    assert_eq!(s["tab"], "search");
    assert_eq!(s["mode"], "new", "検索タブの既定は新モード");

    // ファイル名の部分一致（OR: alpha か beta）
    e.send(&["search-text alpha beta", "search-start"]);
    let s = e.wait_search("first search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false)
    });
    let h0 = &s["history"][0];
    assert_eq!(hits(h0), 65, "alpha 40 + beta 25: {h0}");
    assert_eq!(h0["mode"], "new");
    assert_eq!(h0["cancelled"], false);
    // 検索タブでその行を表示している: 一覧にも同じ件数が出ている
    assert_eq!(s["viewing"], 0);
    assert_eq!(s["archives"], 65);
    assert_eq!(s["any_running"], false);

    // サイズ条件だけ（文字列は空）: 大きい gamma だけが残る。下限は、最小の gamma より小さく、alpha/beta より大きい値
    let gamma_min = (0..5)
        .map(|i| std::fs::metadata(e.files_dir().join(format!("gamma_{i:03}.png"))).unwrap().len())
        .min()
        .unwrap();
    let other_max = (0..40)
        .map(|i| std::fs::metadata(e.files_dir().join(format!("alpha_{i:03}.png"))).unwrap().len())
        .max()
        .unwrap();
    assert!(gamma_min > other_max * 2, "フィクスチャ前提: gamma は alpha/beta よりずっと大きい");
    let min_mb = format!("{}", ((gamma_min + other_max) / 2) as f64 / (1024.0 * 1024.0));
    e.send(&["search-text", &format!("search-size {min_mb} -"), "search-start"]);
    let s = e.wait_search("size search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 2 && h[0]["running"] == false)
    });
    assert_eq!(hits(&s["history"][0]), 5, "gamma だけ: {}", s["history"][0]);
    assert_eq!(hits(&s["history"][1]), 65, "古い検索は下に残る");

    // 文字列とサイズの AND: gamma のうち名前に alpha を含むもの = 0
    e.send(&["search-text alpha", "search-start"]);
    let s = e.wait_search("and search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 3 && h[0]["running"] == false)
    });
    assert_eq!(hits(&s["history"][0]), 0, "alpha かつ 大きい = 0: {}", s["history"][0]);

    // 履歴の行を選ぶと、その結果の一覧に切り替わる
    e.send(&["search-select 2"]);
    let s = e.wait_search("history row shown", Duration::from_secs(10), |s| s["archives"] == 65 && s["viewing"] == 2);
    assert_eq!(s["selected"], 2);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn new_search_keeps_collecting_while_another_tab_is_shown() {
    if skip() { return; }
    let mut e = Env::new("search-bg");
    e.make_files("alpha", 40, 16);
    e.make_files("beta", 20, 16);
    e.launch();
    e.open_and_wait_ledger(60);
    // 実フォルダの一覧が出きるのを待つ（他タブへ移ったときに、その一覧が検索結果で汚れないことの基準）
    let real_count = e.wait_search("real folder listed", Duration::from_secs(30), |s| s["archives"] == 60)["archives"].as_u64().unwrap();

    // 検索を始めた直後に別のタブへ移る（同じバッチで送るので、検索が終わる前に移る）
    e.send(&["tab search", "search-text alpha", "search-start", "tab real"]);
    let s = e.wait_search("search finished in background", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false)
    });
    assert_eq!(s["tab"], "real");
    assert_eq!(hits(&s["history"][0]), 40, "他のタブにいる間も、結果は履歴の行へ溜まる: {}", s["history"][0]);
    assert_eq!(s["viewing"], Value::Null, "検索結果は表示していない");
    // 実フォルダの一覧は検索結果で汚れていない
    assert_eq!(s["archives"].as_u64().unwrap(), real_count, "実フォルダ表示の一覧は元のまま");

    // 検索タブへ戻って、その行を選ぶと、集まった結果が出る
    e.send(&["tab search", "search-select 0"]);
    let s = e.wait_search("results shown after returning", Duration::from_secs(10), |s| s["viewing"] == 0 && s["archives"] == 40);
    assert_eq!(s["tab"], "search");
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn cancel_stops_the_search_and_keeps_the_row_without_a_running_job() {
    if skip() { return; }
    let mut e = Env::new("search-cancel");
    e.make_files("alpha", 30, 16);
    e.launch();
    e.open_and_wait_ledger(30);

    // 開始と同じバッチで中断する（実行が速くても、中断の経路を通る）
    e.send(&["tab search", "search-text alpha", "search-start", "search-cancel"]);
    let s = e.wait_search("cancelled", Duration::from_secs(10), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false)
    });
    let h0 = &s["history"][0];
    assert_eq!(s["job_running"], false);
    assert_eq!(s["any_running"], false);
    assert!(hits(h0) <= 30);
    // 同じ検索を、完了まで走らせて比べる（中断後も次の検索を始められる）
    e.send(&["search-start"]);
    let s = e.wait_search("second search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 2 && h[0]["running"] == false)
    });
    assert_eq!(hits(&s["history"][0]), 30);
    assert_eq!(s["history"][0]["cancelled"], false);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn switching_search_mode_keeps_each_modes_input_and_hides_the_tree() {
    if skip() { return; }
    let mut e = Env::new("search-mode");
    e.make_files("alpha", 5, 16);
    e.launch();
    e.open_and_wait_ledger(5);
    e.send(&["tab search", "search-text alpha"]);
    assert_eq!(e.search()["mode"], "new");
    e.send(&["search-mode legacy"]);
    assert_eq!(e.search()["mode"], "legacy");
    // 旧モードに切り替えても、新の入力は消えない。戻って検索すると、その入力で動く
    e.send(&["search-mode new", "search-start"]);
    let s = e.wait_search("search with kept input", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false)
    });
    assert_eq!(hits(&s["history"][0]), 5);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn the_search_tab_never_shows_a_stuck_loading_label() {
    if skip() { return; }
    let mut e = Env::new("search-loading");
    e.make_files("alpha", 5, 16);
    e.launch();
    e.open_and_wait_ledger(5);
    // タグ検索タブから検索タブへ移ると、実フォルダのスキャンが（結果を受け取らないまま）保留になる。
    // 作業していなくても「読み込み中」を出し続けてはいけない。
    e.send(&["tab tag", "tab search"]);
    std::thread::sleep(Duration::from_millis(1500)); // 0.5秒の猶予を過ぎるまで待つ
    let s = e.search();
    assert_eq!(s["tab"], "search");
    assert_eq!(s["loading_shown"], false, "検索タブで読み込み中の表示が残っている: {s}");
    // 実フォルダのタブへ戻ると、保留されていた結果が届いて、一覧が出る（結果は失われない）
    e.send(&["tab real"]);
    let s = e.wait_search("real listing restored", Duration::from_secs(20), |s| s["scan_state"] == "done" && s["archives"] == 5);
    assert_eq!(s["loading_shown"], false);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn goto_real_opens_the_files_folder_in_the_folders_tab_and_selects_the_file() {
    if skip() { return; }
    let mut e = Env::new("search-goto-real");
    e.make_files("alpha", 12, 16);
    e.launch();
    e.open_and_wait_ledger(12);
    // いったん別のフォルダ（起動時の画像のフォルダ）へ移ってから、検索結果の「実フォルダ」アイコンで戻る
    let other = format!("open {}", e.root.join("pics").display());
    e.send(&["tab real", &other]);
    e.wait_search("moved away", Duration::from_secs(20), |s| {
        s["current_dir"].as_str().is_some_and(|d| d.ends_with("/pics")) && s["scan_state"] == "done"
    });
    e.send(&["tab search", "search-text alpha", "search-start"]);
    e.wait_search("search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false) && s["archives"] == 12
    });

    // 検索結果の 7 番目（名前順で alpha_006）の場所へ移る
    e.send(&["goto-real 6"]);
    let s = e.wait_search("opened in the folders tab", Duration::from_secs(20), |s| {
        s["tab"] == "real"
            && s["current_dir"].as_str().is_some_and(|d| d.ends_with("/files"))
            && s["scan_state"] == "done"
            && s["archives"] == 12
            && s["cursor_file"] == "alpha_006.png"
    });
    assert_eq!(s["viewing"], Value::Null, "実フォルダの表示に戻っている");
    // 検索の履歴は残っている（検索タブへ戻って見直せる）
    assert_eq!(s["history"].as_array().unwrap().len(), 1);
}

/// 検索条件の履歴ファイル（新モード）。アプリの設定フォルダ直下。
fn history_file(e: &Env) -> PathBuf {
    e.root.join("home/c/nekoview/search_history_new.jsonl")
}

fn history_lines(e: &Env) -> Vec<Value> {
    std::fs::read_to_string(history_file(e))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn search_conditions_are_saved_and_a_restored_row_rescans_after_a_restart() {
    if skip() { return; }
    let mut e = Env::new("search-history");
    e.make_files("alpha", 8, 16);
    e.make_files("beta", 4, 16);
    e.launch();
    e.open_and_wait_ledger(12);

    // 3回検索する（同じ条件を含めて、毎回1行ずつ積む）
    e.send(&["tab search"]);
    for (i, text) in ["alpha", "beta", "alpha"].iter().enumerate() {
        e.send(&[&format!("search-text {text}"), "search-start"]);
        e.wait_search("search done", Duration::from_secs(30), |s| {
            s["history"].as_array().is_some_and(|h| h.len() == i + 1 && h[0]["running"] == false)
        });
    }
    // 条件だけが保存される（結果は保存しない）。新しい順
    assert!(
        wait_until(Duration::from_secs(10), || history_lines(&e).len() == 3),
        "条件が履歴ファイルへ保存される"
    );
    let saved = history_lines(&e);
    let texts: Vec<&str> = saved.iter().map(|l| l["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["alpha", "beta", "alpha"], "新しいものが先頭");
    let raw = std::fs::read_to_string(history_file(&e)).unwrap();
    assert!(!raw.contains("alpha_000.png"), "ヒット（結果）は保存しない");

    // 再起動すると、履歴は「結果を持たない行」として戻る
    e.launch();
    let s = e.search();
    let rows = s["history"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "履歴が復元される: {s}");
    assert!(rows.iter().all(|r| r["restored"] == true && r["running"] == false && hits(r) == 0));
    assert!(rows[1]["label"].as_str().unwrap().contains("beta"));
    assert_eq!(rows[0]["mode"], "new");

    // 「beta」の行（2番目）を選ぶと、条件を復元して再走査する。その行は置き換わって先頭へ移る
    e.send(&["tab search", "search-select 1"]);
    let s = e.wait_search("restored row rescanned", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 3 && h[0]["restored"] == false && h[0]["running"] == false)
    });
    assert_eq!(hits(&s["history"][0]), 4, "beta は4件: {}", s["history"][0]);
    assert_eq!(s["form_text"], "beta", "条件がフォームへ復元されている");
    assert_eq!(s["archives"], 4);
    // ファイルでも、同じ行が重複せず先頭へ移っている
    assert!(wait_until(Duration::from_secs(10), || {
        let l = history_lines(&e);
        l.len() == 3 && l[0]["text"] == "beta"
    }));
    let texts: Vec<String> = history_lines(&e).iter().map(|l| l["text"].as_str().unwrap().to_string()).collect();
    assert_eq!(texts, vec!["beta", "alpha", "alpha"]);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn a_search_without_any_condition_cannot_start_in_either_mode() {
    if skip() { return; }
    let mut e = Env::new("search-empty");
    e.make_files("alpha", 3, 16);
    e.launch();
    e.open_and_wait_ledger(3);
    e.send(&["tab search", "search-text", "search-start"]);
    std::thread::sleep(Duration::from_millis(500));
    let s = e.search();
    assert_eq!(s["history"].as_array().unwrap().len(), 0, "空の文字列では始まらない: {s}");
    assert_eq!(s["any_running"], false);
    // 数値として読めないサイズだけでも、条件にならない
    e.send(&["search-size abc -", "search-start"]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(e.search()["history"].as_array().unwrap().len(), 0);
    // 旧モード（ファイル名・サイズ・日付が空）でも始まらない
    e.send(&["search-mode legacy", "search-start"]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(e.search()["history"].as_array().unwrap().len(), 0);
    assert!(!history_file(&e).exists(), "始まらなかった検索は、履歴にも残らない");
    // 有効な条件を入れれば始まる
    e.send(&["search-mode new", "search-text alpha", "search-start"]);
    e.wait_search("started", Duration::from_secs(30), |s| s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false));
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn each_search_mode_shows_only_its_own_history_and_clears_the_results_on_switching() {
    if skip() { return; }
    let mut e = Env::new("search-history-modes");
    e.make_files("alpha", 8, 16);
    // 旧モードの履歴を1行、あらかじめ保存しておく（起動時に、結果を持たない行として復元される）
    let dir = e.root.join("home/c/nekoview");
    std::fs::create_dir_all(&dir).unwrap();
    let legacy_line = serde_json::json!({
        "v": 1, "t": 1_700_000_000i64, "base_dir": e.files_dir().display().to_string(), "name": "alpha*",
        "subdirs": false, "size_min": "", "size_max": "", "date_after": "", "date_before": "",
    });
    std::fs::write(dir.join("search_history_legacy.jsonl"), format!("{legacy_line}\n")).unwrap();
    e.launch();
    e.open_and_wait_ledger(8);

    // 新モード: 新の行はまだ無い（旧の行は一覧に出ない）
    e.send(&["tab search"]);
    let s = e.search();
    assert_eq!(s["mode"], "new");
    assert_eq!(s["history"].as_array().unwrap().len(), 1, "旧の行が復元されている: {s}");
    assert_eq!(s["history_visible"], serde_json::json!([]), "新モードの一覧に、旧の行は出さない");

    // 新モードで検索すると、新の行が1つ出る。結果も表示される
    e.send(&["search-text alpha", "search-start"]);
    let s = e.wait_search("new search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 2 && h[0]["running"] == false)
    });
    assert_eq!(s["history_visible"], serde_json::json!([0]));
    assert_eq!(s["viewing"], 0);
    assert_eq!(s["archives"], 8);

    // 旧へ切り替える: 表示中の結果をクリアし、旧の行だけを出す
    e.send(&["search-mode legacy"]);
    let s = e.search();
    assert_eq!(s["mode"], "legacy");
    assert_eq!(s["history_visible"], serde_json::json!([1]), "旧モードの一覧には、旧の行だけ");
    assert_eq!(s["viewing"], Value::Null, "新の結果の表示はクリアされる");
    assert_eq!(s["archives"], 0);
    assert_eq!(s["selected"], Value::Null);

    // 新へ戻す: 新の行だけを出す。結果は、行を選ぶまで出ない
    e.send(&["search-mode new"]);
    let s = e.search();
    assert_eq!(s["history_visible"], serde_json::json!([0]));
    assert_eq!(s["viewing"], Value::Null);
    assert_eq!(s["archives"], 0);
    e.send(&["search-select 0"]);
    let s = e.wait_search("row shows its results", Duration::from_secs(10), |s| s["viewing"] == 0 && s["archives"] == 8);
    assert_eq!(s["selected"], 0);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn goto_virtual_opens_the_registered_virtual_folder_and_chooses_among_several() {
    if skip() { return; }
    let mut e = Env::new("search-goto-virtual");
    e.make_files("alpha", 12, 16);
    e.launch();
    e.open_and_wait_ledger(12);
    let other = format!("open {}", e.root.join("pics").display());
    e.send(&["tab real", &other]);
    e.wait_search("moved away", Duration::from_secs(20), |s| {
        s["current_dir"].as_str().is_some_and(|d| d.ends_with("/pics")) && s["scan_state"] == "done"
    });
    e.send(&["tab search", "search-text alpha", "search-start"]);
    e.wait_search("search done", Duration::from_secs(30), |s| {
        s["history"].as_array().is_some_and(|h| h.len() == 1 && h[0]["running"] == false) && s["archives"] == 12
    });

    // 登録した仮想フォルダが無いうちは、何も起きない（アイコンは灰色）
    e.send(&["goto-virtual 3"]);
    std::thread::sleep(Duration::from_millis(500));
    let s = e.search();
    assert_eq!(s["tab"], "search", "登録が無ければ移動しない: {s}");
    assert_eq!(s["goto_menu"], Value::Null);

    // 登録が1つ: そこへ移る。仮想フォルダタブで、そのノードが開き、ファイルが選択される
    let register = format!("virtual-add {}", e.files_dir().display());
    e.send(&[&register]);
    let ids = e.search()["virtual_ids"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 1);
    e.send(&["goto-virtual 5"]);
    let s = e.wait_search("opened in the virtual tab", Duration::from_secs(20), |s| {
        s["tab"] == "virtual"
            && s["virtual_node"] == ids[0]
            && s["current_dir"].as_str().is_some_and(|d| d.ends_with("/files"))
            && s["scan_state"] == "done"
            && s["archives"] == 12
            && s["cursor_file"] == "alpha_005.png"
    });
    assert_eq!(s["goto_menu"], Value::Null);
    // 検索の履歴は残っている
    assert_eq!(s["history"].as_array().unwrap().len(), 1);

    // 同じフォルダをもう1つ登録（同じ実パスの複数登録）。検索結果へ戻って押すと、選択メニューが出る
    e.send(&[&register, "tab search", "search-select 0"]);
    e.wait_search("results shown again", Duration::from_secs(10), |s| s["tab"] == "search" && s["archives"] == 12);
    let ids = e.search()["virtual_ids"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 2);
    e.send(&["goto-virtual 2"]);
    let s = e.search();
    assert_eq!(s["goto_menu"], 2, "登録が2つなので、選択メニューが出る: {s}");
    assert_eq!(s["tab"], "search", "選ぶまでは移らない");
    // 2つ目を選ぶと、そのノードが開く
    e.send(&["goto-virtual 2 1"]);
    let s = e.wait_search("second node opened", Duration::from_secs(20), |s| {
        s["tab"] == "virtual" && s["virtual_node"] == ids[1] && s["scan_state"] == "done" && s["cursor_file"] == "alpha_002.png"
    });
    assert_eq!(s["goto_menu"], Value::Null, "選んだらメニューは閉じる");
}
