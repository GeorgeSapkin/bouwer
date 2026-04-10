// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

// The reason for the strange syntax is for rustfm to see this as Rust code and
// not a custom DSL, so it can format it.
macro_rules! clone {
    // clone!((a, b), move |x| { ... })
    (($($n:ident),+), async move |$($arg:pat_param),+| $body:expr) => {
        clone!(@gen ($($n),+) (async move) |$($arg),+| $body)
    };
    (($($n:ident),+), move |$($arg:pat_param),+| $body:expr) => {
        clone!(@gen ($($n),+) (move) |$($arg),+| $body)
    };

    // clone!((a, b), move || { ... })
    (($($n:ident),+), async move || $body:expr) => {
        clone!(@gen ($($n),+) (async move) || $body)
    };
    (($($n:ident),+), move || $body:expr) => {
        clone!(@gen ($($n),+) (move) || $body)
    };

    // clone!((a, b), async move { ... })
    (($($n:ident),+), async move $body:block) => {
        clone!(@gen ($($n),+) (async move) $body)
    };

    (@gen ($($n:ident),+) ($($prefix:tt)*) $($rest:tt)*) => {
        {
            $( let $n = $n.clone(); )+
            $($prefix)* $($rest)*
        }
    };
}
