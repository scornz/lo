//! Link settings for the macOS app, as Zed's binary sets them (`crates/zed/build.rs` in Zed),
//! minus the ones only Zed's screen sharing needs, plus the app's name for the menu bar.

use std::path::Path;

/// The binary's `Info.plist`. macOS reads it from the `__TEXT,__info_plist` section of an
/// executable that isn't in an app bundle, such as `cargo run`'s, and names the app in the menu
/// bar after its `CFBundleName`, which would otherwise be the executable's name, `lo`. It has no
/// bundle identifier: gpui turns on bundle-only features, such as system notifications, when
/// there is one.
const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Lo</string>
</dict>
</plist>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // The oldest macOS Lo runs on, matching gpui's Metal shaders.
        println!("cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET=12.0");
        // Load every Objective-C class and category from the static libraries linked in.
        println!("cargo:rustc-link-arg=-Wl,-ObjC");

        let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
        let plist = Path::new(&out_dir).join("Info.plist");
        std::fs::write(&plist, INFO_PLIST).expect("writing Info.plist");
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
