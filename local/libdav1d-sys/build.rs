fn main() {
    let vcpkg_root = std::env::var("VCPKG_ROOT")
        .expect("VCPKG_ROOT environment variable must be set");

    let dav1d_lib = format!("{vcpkg_root}/packages/dav1d_x64-windows-static/lib");
    let dav1d_include = format!("{vcpkg_root}/packages/dav1d_x64-windows-static/include");
    let dav1d_staticlib =
        format!("{vcpkg_root}/packages/dav1d_x64-windows-static/lib/dav1d.lib");

    println!("cargo:rustc-link-search=native={dav1d_lib}");
    println!("cargo:rustc-link-lib=static=dav1d");
    println!("cargo:include={dav1d_include}");
    println!("cargo:staticlib={dav1d_staticlib}");
}
