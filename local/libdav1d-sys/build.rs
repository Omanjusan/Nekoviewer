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
        //
        // cargo_metadata(false)にして cargo:rustc-link-lib の自動出力を止め、
        // static= を明示的に自前で出す。pkg-config crateの自動判定に任せると
        // （.aしか存在しない環境でも）dylib扱いの `-ldav1d` が出ることがあり、
        // 通常のgcc/ldは.soが無ければ.aへ暗黙フォールバックするため気づきにくいが、
        // フォールバックしないリンカ（zig cc等）では「.soが見つからない」エラーになる。
        let lib = pkg_config::Config::new()
            .statik(true)
            .cargo_metadata(false)
            .probe("dav1d")
            .expect("dav1d not found via pkg-config. Install dav1d-dev (Alpine: apk add dav1d-dev).");

        for path in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        println!("cargo:rustc-link-lib=static=dav1d");

        for path in &lib.include_paths {
            println!("cargo:include={}", path.display());
        }
    }
}
