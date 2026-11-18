//! Puts clipr's icon into clipr.exe, so Explorer, the Start menu and shortcuts show it.
//!
//! assets/clipr.res is a compiled Windows resource made by scripts/make_icon.py; the MSVC linker takes
//! it as an input file, so no resource compiler is needed at build time.

fn main() {
    let res = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/clipr.res");
    println!("cargo:rerun-if-changed={}", res.display());
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-msvc") && res.exists() {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}
