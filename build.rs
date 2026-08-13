// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::fs;
use std::path::PathBuf;

use winresource::WindowsResource;

fn main() {
    slint_build::compile("ui/appwindow.slint").unwrap();

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = WindowsResource::new();

        res.set_language(0x0409); // English US
        res.set_icon("./assets/logo.ico");
        res.compile().expect("Failed to compile Windows resources");
    }

    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        embed_macos_info_plist();
    }
}

/// Embeds an Info.plist into the binary so macOS can show the app name,
/// version and copyright in the About panel without an .app bundle.
fn embed_macos_info_plist() {
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>in.sapk.bouwer</string>
    <key>CFBundleName</key>
    <string>Bouwer</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 George Sapkin
https://github.com/GeorgeSapkin/bouwer</string>
</dict>
</plist>
"#
    );

    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("Info.plist");
    fs::write(&path, plist).expect("Failed to write Info.plist");
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        path.display()
    );
}
