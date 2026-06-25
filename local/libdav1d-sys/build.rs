fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        let vcpkg_root = std::env::var("VCPKG_ROOT")
            .expect("VCPKG_ROOT environment variable must be set on Windows");

        let base = format!("{vcpkg_root}/packages/dav1d_x64-windows-static");
        println!("cargo:rustc-link-search=native={base}/lib");
        println!("cargo:rustc-link-lib=static=dav1d");
        println!("cargo:include={base}/include");
        println!("cargo:staticlib={base}/lib/dav1d.lib");
    } else {
        // musl (Alpine) でも glibc でも pkg-config で解決する。
        // statik(true) で静的リンクを要求し、libavif-sys の cmake が
        // DEP_DAV1D_INCLUDE 経由で dav1d ヘッダを参照できるようにする。
        pkg_config::Config::new()
            .statik(true)
            .probe("dav1d")
            .expect("dav1d not found via pkg-config. Install dav1d-dev (Alpine: apk add dav1d-dev).");

        let pc_dir = std::process::Command::new("pkg-config")
            .args(["--variable=pcfiledir", "dav1d"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "/usr/lib/pkgconfig".to_string());
        println!("cargo:pkgconfig={pc_dir}");
    }
}
