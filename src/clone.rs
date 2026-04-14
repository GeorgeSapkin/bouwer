// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

// The reason for the strange syntax is for rustfm to see this as Rust code and
// not a custom DSL, so it can format it.
macro_rules! clone {
    // async move closure and block
    (($($n:ident),+), async move $($rest:tt)*) => {
        {
            $( let $n = $n.clone(); )+
            async move $($rest)*
        }
    };

    // move closure
    (($($n:ident),+), move $($rest:tt)*) => {
        {
            $( let $n = $n.clone(); )+
            move $($rest)*
        }
    };
}
