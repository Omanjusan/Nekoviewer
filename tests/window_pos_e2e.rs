//! 「ウィンドウ位置」メニュー・ビューアー復帰・画面外補正の自動テスト（実アプリ + Xephyr）。
//!
//! Wayland など窓位置を操作できない環境でも回せるよう、入れ子の X サーバー Xephyr の中で実アプリを
//! 起動し、`test-control` feature の制御口（NEKO_TEST_CONTROL）経由で操作・観測する。
//! Xephyr が無い環境では各テストは何もせず成功扱いで抜ける。
//!
//! 実行: `cargo test --features test-control --test window_pos_e2e -- --ignored --test-threads=1`
//! （Xephyr とGPU描画が要るため既定では ignore。実データには触れず、HOME/XDG_* は一時ディレクトリ。）
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

impl Drop for Env {
    fn drop(&mut self) {
        self.app = None;
        if std::env::var_os("NEKO_E2E_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// 1テスト分の環境: 専用 Xephyr と、隔離した HOME/XDG_*/制御ディレクトリ。
struct Env {
    root: PathBuf,
    display: String,
    _xephyr: Guard,
    app: Option<Guard>,
}

impl Env {
    fn new(name: &str) -> Self {
        // 前のテストの Xephyr が終了しきる前に同じ番号を使うと、残骸のソケットに繋がって失敗する。
        // ロックファイルもソケットも無い番号だけを使う。
        let num = loop {
            let n = 60 + DISPLAY_SEQ.fetch_add(1, Ordering::SeqCst) % 400;
            if !Path::new(&format!("/tmp/.X{n}-lock")).exists() && !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
                break n;
            }
        };
        let display = format!(":{num}");
        let root = std::env::temp_dir().join(format!("nekoviewer-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home", "run", "ctl", "pics"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        for (i, c) in [[200u8, 60, 60], [60, 200, 60], [60, 60, 200]].iter().enumerate() {
            image::RgbImage::from_pixel(640, 480, image::Rgb(*c)).save(root.join(format!("pics/p{i}.png"))).unwrap();
        }
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

    fn state_file(&self) -> PathBuf {
        self.root.join("home/c/nekoview/nekoviewer.state")
    }

    /// アプリを起動して、ビューアー窓が出るまで待つ。`fake_monitors` はモニター一覧の差し替え。
    fn launch(&mut self, fake_monitors: Option<&str>) {
        self.stop_app();
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
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.root.join("app.log")).unwrap());
        if let Some(m) = fake_monitors {
            cmd.env("NEKO_FAKE_MONITORS", m);
        }
        self.app = Some(Guard(cmd.spawn().expect("spawn nekoviewer")));
        assert!(
            wait_until(Duration::from_secs(40), || self.try_dump().is_some_and(|d| !d["viewer_window"].is_null())),
            "viewer window did not appear"
        );
        self.settle();
    }

    fn stop_app(&mut self) {
        self.app = None; // Drop で kill + wait
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

    fn dump(&self) -> Value {
        self.try_dump().expect("dump.json was not written")
    }

    /// egui が窓の最新位置を取り込むのを待つ。
    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(500));
    }

    /// ビューアー窓の外寸位置とサイズ (x, y, outer_w, outer_h)。
    fn viewer_geom(&self) -> (i64, i64, i64, i64) {
        let d = self.dump();
        let w = &d["viewer_window"];
        (
            w["outer_x"].as_i64().unwrap(),
            w["outer_y"].as_i64().unwrap(),
            w["outer_w"].as_i64().unwrap(),
            w["outer_h"].as_i64().unwrap(),
        )
    }

    /// この Xephyr 上に、タイトルが `title_part` を含むトップレベル窓があるか。
    fn has_x_window(&self, title_part: &str) -> bool {
        let out = Command::new("xwininfo").args(["-display", &self.display, "-root", "-tree"]).output().expect("run xwininfo");
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(title_part))
    }

    fn slot(&self, idx: usize) -> Value {
        self.dump()["app"]["slots"][idx].clone()
    }
}

fn skip() -> bool {
    if xephyr_available() {
        return false;
    }
    eprintln!("Xephyr not found: skipping");
    true
}

fn state_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn recover_brings_an_offscreen_viewer_back_to_the_center() {
    if skip() { return; }
    let mut e = Env::new("recover");
    // この Xephyr の winit は実モニターを返さないため、復帰先の判断に使う一覧は差し替える
    e.launch(Some("0,0,1920,1080"));

    e.send(&["move -5000 -5000 800 600"]);
    e.settle();
    let (x, y, _, _) = e.viewer_geom();
    assert!(x < -1000 && y < -1000, "window should be far off-screen, got ({x},{y})");

    e.send(&["recover"]);
    e.settle();
    assert_eq!(e.viewer_geom(), (560, 240, 800, 600), "centered on the 1920x1080 screen, size kept");

    // 画面より大きい窓は、収まるよう縮められる
    e.send(&["move -300 -300 3000 2000"]);
    e.settle();
    e.send(&["recover"]);
    e.settle();
    assert_eq!(e.viewer_geom(), (0, 0, 1920, 1080));
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn closing_the_viewer_destroys_its_os_window() {
    if skip() { return; }
    let mut e = Env::new("close");
    e.launch(Some("0,0,1920,1080"));
    // メニューの操作後に閉じても同じ。モニター一覧（窓ハンドル）を引いた後でも窓が残らないこと
    e.send(&["record 1", "auto 1"]);
    e.settle();
    assert!(e.has_x_window("Imageview"));

    e.send(&["close"]);
    assert!(
        wait_until(Duration::from_secs(5), || !e.has_x_window("Imageview")),
        "the viewer's OS window must be destroyed when the viewer closes (a leftover window is frozen)"
    );
    assert_eq!(e.dump()["app"]["viewer_open"], false);
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn record_apply_delete_roundtrip() {
    if skip() { return; }
    let mut e = Env::new("roundtrip");
    e.launch(None);

    assert!(e.slot(0).is_null());
    e.send(&["move 100 50 700 500"]);
    e.settle();
    e.send(&["record 1"]);
    e.settle();
    let s = e.slot(0);
    assert_eq!((s["x"].as_i64(), s["y"].as_i64(), s["w"].as_i64(), s["h"].as_i64()), (Some(100), Some(50), Some(700), Some(500)));

    e.send(&["move 900 300 800 600"]);
    e.settle();
    e.send(&["apply 1"]);
    e.settle();
    assert_eq!(e.viewer_geom(), (100, 50, 700, 500), "apply restores the recorded position and size");

    e.send(&["delete 1"]);
    e.settle();
    assert!(e.slot(0).is_null());
    e.send(&["move 900 300 800 600"]);
    e.settle();
    e.send(&["apply 1"]);
    e.settle();
    assert_eq!(e.viewer_geom(), (900, 300, 800, 600), "applying an empty slot is a no-op");
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn apply_pulls_offscreen_slots_in_and_keeps_second_monitor_positions() {
    if skip() { return; }
    let mut e = Env::new("clamp");
    // 擬似的に 1920x1080 を2枚並べる。X サーバー自体は1画面だが、補正は一覧だけを見る。
    e.launch(Some("0,0,1920,1080;1920,0,1920,1080"));
    assert_eq!(e.dump()["monitors"].as_array().unwrap().len(), 2);

    // 2枚目モニター上の記録は、そのまま展開される（旧実装は x=1820 に丸めていた）
    e.send(&["move 2000 50 800 600"]);
    e.settle();
    e.send(&["record 1"]);
    e.settle();
    e.send(&["move 0 0 800 600"]);
    e.settle();
    e.send(&["apply 1"]);
    e.settle();
    let (x, y, _, _) = e.viewer_geom();
    assert_eq!((x, y), (2000, 50));

    // どのモニターの外でもない記録は、近いモニターの内側へ寄せられる
    e.send(&["move 100 100 800 600"]);
    e.settle();
    e.send(&["record 2"]);
    e.settle();
    e.send(&["move -9000 100 800 600"]);
    e.settle();
    e.send(&["record 3"]);
    e.settle();
    e.send(&["move 500 500 800 600", "apply 3"]);
    e.settle();
    let (x, y, _, _) = e.viewer_geom();
    assert_eq!((x, y), (-700, 100), "left edge keeps 100px visible");
}

#[test]
#[ignore = "needs Xephyr + GPU rendering; run explicitly"]
fn auto_apply_persists_across_restart_and_survives_deleting_its_slot() {
    if skip() { return; }
    let mut e = Env::new("auto");
    e.launch(None);

    e.send(&["move 300 200 640 480"]);
    e.settle();
    e.send(&["record 3", "auto 3"]);
    e.settle();
    assert_eq!(e.dump()["app"]["default_slot"], 2);
    assert!(state_text(&e.state_file()).contains("app_default_slot=7"), "auto-apply slot is written to the state file");

    // 再起動すると、ビューアーは記録した位置・サイズで開く
    e.launch(None);
    assert_eq!(e.viewer_geom(), (300, 200, 640, 480));

    // 自動展開中のスロットを削除してもチェックは外れず、記録だけが消える
    e.send(&["delete 3"]);
    e.settle();
    let d = e.dump();
    assert!(d["app"]["slots"][2].is_null());
    assert_eq!(d["app"]["default_slot"], 2);
    assert!(state_text(&e.state_file()).contains("app_default_slot=7"));
    assert!(!state_text(&e.state_file()).contains("slot3_x"), "deleted slot is gone from the state file");

    // 再起動しても空のまま（no-op）で、既定位置のまま開く
    e.launch(None);
    let d = e.dump();
    assert!(d["app"]["slots"][2].is_null());
    assert_eq!(d["app"]["default_slot"], 2);
    let (x, y, w, h) = e.viewer_geom();
    assert_eq!((w, h), (800, 600), "no slot applied: default size");
    assert!(x != 300 || y != 200);

    // 全てのチェックを外すと「なし」になり、それも永続する
    e.send(&["auto none"]);
    e.settle();
    assert!(e.dump()["app"]["default_slot"].is_null());
    assert!(state_text(&e.state_file()).contains("app_default_slot=\n"));
}
