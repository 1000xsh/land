//! core unroll! macro implementation with recursive bisection
//!
//! implements compile-time loop unrolling with two strategies:
//! - inline expansion for n ≤ 16 (manually coded base cases)
//! - recursive bisection for n > 16 (manually coded common sizes)

/// unroll a loop n times with compile-time expansion
///
/// generates straight-line code with no loop overhead. each iteration gets
/// a `const i: usize` variable for llvm constant folding.
///
/// # recursion limit
///
/// for n > 128, add `#![recursion_limit = "512"]` to your crate root
#[macro_export]
macro_rules! unroll {
    // main entry: unroll!(n, |i| body) - dispatch based on literal value
    (0, |$i:ident| $body:expr) => {};
    (1, |$i:ident| $body:expr) => { $crate::unroll!(@inline 1, 0, |$i| $body) };
    (2, |$i:ident| $body:expr) => { $crate::unroll!(@inline 2, 0, |$i| $body) };
    (3, |$i:ident| $body:expr) => { $crate::unroll!(@inline 3, 0, |$i| $body) };
    (4, |$i:ident| $body:expr) => { $crate::unroll!(@inline 4, 0, |$i| $body) };
    (5, |$i:ident| $body:expr) => { $crate::unroll!(@inline 5, 0, |$i| $body) };
    (6, |$i:ident| $body:expr) => { $crate::unroll!(@inline 6, 0, |$i| $body) };
    (7, |$i:ident| $body:expr) => { $crate::unroll!(@inline 7, 0, |$i| $body) };
    (8, |$i:ident| $body:expr) => { $crate::unroll!(@inline 8, 0, |$i| $body) };
    (9, |$i:ident| $body:expr) => { $crate::unroll!(@inline 9, 0, |$i| $body) };
    (10, |$i:ident| $body:expr) => { $crate::unroll!(@inline 10, 0, |$i| $body) };
    (11, |$i:ident| $body:expr) => { $crate::unroll!(@inline 11, 0, |$i| $body) };
    (12, |$i:ident| $body:expr) => { $crate::unroll!(@inline 12, 0, |$i| $body) };
    (13, |$i:ident| $body:expr) => { $crate::unroll!(@inline 13, 0, |$i| $body) };
    (14, |$i:ident| $body:expr) => { $crate::unroll!(@inline 14, 0, |$i| $body) };
    (15, |$i:ident| $body:expr) => { $crate::unroll!(@inline 15, 0, |$i| $body) };
    (16, |$i:ident| $body:expr) => { $crate::unroll!(@inline 16, 0, |$i| $body) };

    // manually coded bisection for common sizes 17-1024
    (17, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 17, 0, |$i| $body) };
    (18, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 18, 0, |$i| $body) };
    (19, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 19, 0, |$i| $body) };
    (20, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 20, 0, |$i| $body) };
    (21, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 21, 0, |$i| $body) };
    (22, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 22, 0, |$i| $body) };
    (23, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 23, 0, |$i| $body) };
    (24, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 24, 0, |$i| $body) };
    (25, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 25, 0, |$i| $body) };
    (26, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 26, 0, |$i| $body) };
    (27, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 27, 0, |$i| $body) };
    (28, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 28, 0, |$i| $body) };
    (29, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 29, 0, |$i| $body) };
    (30, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 30, 0, |$i| $body) };
    (31, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 31, 0, |$i| $body) };
    (32, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 32, 0, |$i| $body) };
    (33, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 33, 0, |$i| $body) };
    (34, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 34, 0, |$i| $body) };
    (35, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 35, 0, |$i| $body) };
    (36, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 36, 0, |$i| $body) };
    (37, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 37, 0, |$i| $body) };
    (38, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 38, 0, |$i| $body) };
    (39, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 39, 0, |$i| $body) };
    (40, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 40, 0, |$i| $body) };
    (41, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 41, 0, |$i| $body) };
    (42, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 42, 0, |$i| $body) };
    (43, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 43, 0, |$i| $body) };
    (44, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 44, 0, |$i| $body) };
    (45, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 45, 0, |$i| $body) };
    (46, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 46, 0, |$i| $body) };
    (47, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 47, 0, |$i| $body) };
    (48, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 48, 0, |$i| $body) };
    (49, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 49, 0, |$i| $body) };
    (50, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 50, 0, |$i| $body) };
    (51, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 51, 0, |$i| $body) };
    (52, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 52, 0, |$i| $body) };
    (53, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 53, 0, |$i| $body) };
    (54, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 54, 0, |$i| $body) };
    (55, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 55, 0, |$i| $body) };
    (56, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 56, 0, |$i| $body) };
    (57, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 57, 0, |$i| $body) };
    (58, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 58, 0, |$i| $body) };
    (59, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 59, 0, |$i| $body) };
    (60, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 60, 0, |$i| $body) };
    (61, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 61, 0, |$i| $body) };
    (62, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 62, 0, |$i| $body) };
    (63, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 63, 0, |$i| $body) };
    (64, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 64, 0, |$i| $body) };
    (128, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 128, 0, |$i| $body) };
    (256, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 256, 0, |$i| $body) };
    (512, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 512, 0, |$i| $body) };
    (1024, |$i:ident| $body:expr) => { $crate::unroll!(@bisect 1024, 0, |$i| $body) };

    // catch-all for other sizes (will fail with error message)
    ($n:literal, |$i:ident| $body:expr) => {
        compile_error!(concat!("unroll! only supports sizes 0-64, 128, 256, 512, 1024. requested: ", stringify!($n)))
    };

    // inline expansion: manually unroll each case
    (@inline 1, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
    }};
    (@inline 2, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
    }};
    (@inline 3, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
    }};
    (@inline 4, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
    }};
    (@inline 5, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
    }};
    (@inline 6, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
    }};
    (@inline 7, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
    }};
    (@inline 8, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
    }};
    (@inline 9, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
    }};
    (@inline 10, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
    }};
    (@inline 11, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
    }};
    (@inline 12, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
        { const $i: usize = $offset + 11; $body }
    }};
    (@inline 13, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
        { const $i: usize = $offset + 11; $body }
        { const $i: usize = $offset + 12; $body }
    }};
    (@inline 14, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
        { const $i: usize = $offset + 11; $body }
        { const $i: usize = $offset + 12; $body }
        { const $i: usize = $offset + 13; $body }
    }};
    (@inline 15, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
        { const $i: usize = $offset + 11; $body }
        { const $i: usize = $offset + 12; $body }
        { const $i: usize = $offset + 13; $body }
        { const $i: usize = $offset + 14; $body }
    }};
    (@inline 16, $offset:expr, |$i:ident| $body:expr) => {{
        { const $i: usize = $offset + 0; $body }
        { const $i: usize = $offset + 1; $body }
        { const $i: usize = $offset + 2; $body }
        { const $i: usize = $offset + 3; $body }
        { const $i: usize = $offset + 4; $body }
        { const $i: usize = $offset + 5; $body }
        { const $i: usize = $offset + 6; $body }
        { const $i: usize = $offset + 7; $body }
        { const $i: usize = $offset + 8; $body }
        { const $i: usize = $offset + 9; $body }
        { const $i: usize = $offset + 10; $body }
        { const $i: usize = $offset + 11; $body }
        { const $i: usize = $offset + 12; $body }
        { const $i: usize = $offset + 13; $body }
        { const $i: usize = $offset + 14; $body }
        { const $i: usize = $offset + 15; $body }
    }};

    // recursive bisection: manually coded for each literal count
    // 17-32
    (@bisect 17, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 1, $offset + 16, |$i| $body); }};
    (@bisect 18, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 2, $offset + 16, |$i| $body); }};
    (@bisect 19, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 3, $offset + 16, |$i| $body); }};
    (@bisect 20, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 4, $offset + 16, |$i| $body); }};
    (@bisect 21, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 5, $offset + 16, |$i| $body); }};
    (@bisect 22, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 6, $offset + 16, |$i| $body); }};
    (@bisect 23, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 7, $offset + 16, |$i| $body); }};
    (@bisect 24, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 8, $offset + 16, |$i| $body); }};
    (@bisect 25, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 9, $offset + 16, |$i| $body); }};
    (@bisect 26, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 10, $offset + 16, |$i| $body); }};
    (@bisect 27, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 11, $offset + 16, |$i| $body); }};
    (@bisect 28, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 12, $offset + 16, |$i| $body); }};
    (@bisect 29, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 13, $offset + 16, |$i| $body); }};
    (@bisect 30, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 14, $offset + 16, |$i| $body); }};
    (@bisect 31, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 15, $offset + 16, |$i| $body); }};
    (@bisect 32, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@inline 16, $offset, |$i| $body); $crate::unroll!(@inline 16, $offset + 16, |$i| $body); }};

    // 33-63 = 32 + remainder
    (@bisect 33, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 1, $offset + 32, |$i| $body); }};
    (@bisect 34, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 2, $offset + 32, |$i| $body); }};
    (@bisect 35, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 3, $offset + 32, |$i| $body); }};
    (@bisect 36, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 4, $offset + 32, |$i| $body); }};
    (@bisect 37, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 5, $offset + 32, |$i| $body); }};
    (@bisect 38, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 6, $offset + 32, |$i| $body); }};
    (@bisect 39, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 7, $offset + 32, |$i| $body); }};
    (@bisect 40, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 8, $offset + 32, |$i| $body); }};
    (@bisect 41, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 9, $offset + 32, |$i| $body); }};
    (@bisect 42, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 10, $offset + 32, |$i| $body); }};
    (@bisect 43, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 11, $offset + 32, |$i| $body); }};
    (@bisect 44, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 12, $offset + 32, |$i| $body); }};
    (@bisect 45, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 13, $offset + 32, |$i| $body); }};
    (@bisect 46, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 14, $offset + 32, |$i| $body); }};
    (@bisect 47, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 15, $offset + 32, |$i| $body); }};
    (@bisect 48, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@inline 16, $offset + 32, |$i| $body); }};
    (@bisect 49, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 17, $offset + 32, |$i| $body); }};
    (@bisect 50, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 18, $offset + 32, |$i| $body); }};
    (@bisect 51, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 19, $offset + 32, |$i| $body); }};
    (@bisect 52, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 20, $offset + 32, |$i| $body); }};
    (@bisect 53, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 21, $offset + 32, |$i| $body); }};
    (@bisect 54, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 22, $offset + 32, |$i| $body); }};
    (@bisect 55, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 23, $offset + 32, |$i| $body); }};
    (@bisect 56, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 24, $offset + 32, |$i| $body); }};
    (@bisect 57, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 25, $offset + 32, |$i| $body); }};
    (@bisect 58, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 26, $offset + 32, |$i| $body); }};
    (@bisect 59, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 27, $offset + 32, |$i| $body); }};
    (@bisect 60, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 28, $offset + 32, |$i| $body); }};
    (@bisect 61, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 29, $offset + 32, |$i| $body); }};
    (@bisect 62, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 30, $offset + 32, |$i| $body); }};
    (@bisect 63, $offset:expr, |$i:ident| $body:expr) => {{ $crate::unroll!(@bisect 32, $offset, |$i| $body); $crate::unroll!(@bisect 31, $offset + 32, |$i| $body); }};

    // 64 = 32 + 32
    (@bisect 64, $offset:expr, |$i:ident| $body:expr) => {{
        $crate::unroll!(@bisect 32, $offset, |$i| $body);
        $crate::unroll!(@bisect 32, $offset + 32, |$i| $body);
    }};

    // 128 = 64 + 64
    (@bisect 128, $offset:expr, |$i:ident| $body:expr) => {{
        $crate::unroll!(@bisect 64, $offset, |$i| $body);
        $crate::unroll!(@bisect 64, $offset + 64, |$i| $body);
    }};

    // 256 = 128 + 128
    (@bisect 256, $offset:expr, |$i:ident| $body:expr) => {{
        $crate::unroll!(@bisect 128, $offset, |$i| $body);
        $crate::unroll!(@bisect 128, $offset + 128, |$i| $body);
    }};

    // 512 = 256 + 256
    (@bisect 512, $offset:expr, |$i:ident| $body:expr) => {{
        $crate::unroll!(@bisect 256, $offset, |$i| $body);
        $crate::unroll!(@bisect 256, $offset + 256, |$i| $body);
    }};

    // 1024 = 512 + 512
    (@bisect 1024, $offset:expr, |$i:ident| $body:expr) => {{
        $crate::unroll!(@bisect 512, $offset, |$i| $body);
        $crate::unroll!(@bisect 512, $offset + 512, |$i| $body);
    }};
}
