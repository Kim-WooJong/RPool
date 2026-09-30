//! With the `winfsp` feature (default) on Windows MSVC, link WinFsp's DLL
//! delay-loaded: it lives in WinFsp's install directory, which is loaded at
//! runtime (`winfsp_wrs::init`) before the first call. A PC without WinFsp
//! therefore still starts, and `--frontend auto` uses WebDAV. Other targets
//! (and `--no-default-features`) emit nothing here.
fn main() {
    #[cfg(feature = "winfsp")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winfsp_wrs_build::build();
    }
}
