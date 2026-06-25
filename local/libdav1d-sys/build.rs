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
        pkg_config::probe_library("dav1d").expect("dav1d not found via pkg-config. Install libdav1d-dev.");
    }
}
