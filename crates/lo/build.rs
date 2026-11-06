//! Link settings for the macOS app, as Zed's binary sets them (`crates/zed/build.rs` in Zed),
//! minus the ones only Zed's screen sharing needs.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // The oldest macOS rozed runs on, matching gpui's Metal shaders.
        println!("cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET=12.0");
        // Load every Objective-C class and category from the static libraries linked in.
        println!("cargo:rustc-link-arg=-Wl,-ObjC");
    }
}
