//! With the `winfsp` feature on Windows MSVC, link WinFsp's DLL delay-loaded:
//! it lives in WinFsp's install directory, which the frontend adds at runtime
//! (`winfsp_wrs::init`) before the first call.
fn main() {
    #[cfg(feature = "winfsp")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winfsp_wrs_build::build();
    }
}
