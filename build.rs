// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::env;

use winresource::WindowsResource;

fn main() {
    slint_build::compile("ui/appwindow.slint").unwrap();

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = WindowsResource::new();

        res.set_language(0x0409); // English US
        res.set_icon("./assets/logo.ico");
        res.compile().expect("Failed to compile Windows resources");
    }
}
