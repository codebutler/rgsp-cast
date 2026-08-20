//! Raw NextUI bindings, plus the macros bindgen cannot carry.
//!
//! # The `platform.h` trap
//!
//! Three h700 constants look bindable from `vendor/nextui/h700/platform.h`
//! and are not: that header redefines each of them as a runtime ternary
//! rather than a plain constant, so bindgen silently emits nothing for them
//! (no error, no warning -- they are just absent from the generated
//! bindings). Each was found the hard way, separately, during this project:
//!
//! - `FIXED_WIDTH`/`FIXED_HEIGHT` -- `hdmi_active?...:is_cube?...`
//! - `PADDING` -- `(hdmi_active||is_cube)?5:10`
//! - `MAIN_ROW_COUNT` -- `(hdmi_active||is_cube)?10:6`
//!
//! `PADDING` and `MAIN_ROW_COUNT` are hand-carried as plain Rust constants
//! in `ui.rs` instead, each justified by a proof comment there that this
//! device's fixed 720x480 surface always takes the same branch of the
//! ternary (search `ui.rs` for `PADDING` and `MAIN_ROW_COUNT`). No
//! `FIXED_WIDTH`/`FIXED_HEIGHT` equivalent exists yet -- if something needs
//! them, they need the same treatment, not a bindgen allowlist entry.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]

// `dead_code` scoped to just this generated module rather than the whole
// file: bindgen's output allowlists whole families of functions/constants
// (see `build.rs`) and not everything in a family is used yet, but the
// blanket file-level `allow` this used to be would also hide genuinely
// dead hand-written code below (`scale1`..`scale4`).
mod generated {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/nextui.rs"));
}
pub use generated::*;

// FIXED_SCALE is a compile-time 2 on h700 and a runtime ternary on tg5040.
// The scale helpers below are only correct for the former, so refuse to
// compile against headers that say otherwise. This fires from the vendored
// headers themselves: no environment variable to set, nothing to forget, and
// it fails the build rather than a user's device. (On tg5040 headers bindgen
// would not emit FIXED_SCALE at all, which fails the build even earlier.)
const _: () = assert!(FIXED_SCALE == 2);

// SCALE1..SCALE4 are function-like macros, so they do not survive
// preprocessing and bindgen cannot emit them. FIXED_SCALE is an object-like
// macro with a constant value on h700, so it does come across — which leaves
// nothing hand-copied that can drift.
pub const fn scale1(a: i32) -> i32 {
    a * FIXED_SCALE as i32
}
pub const fn scale2(a: i32, b: i32) -> (i32, i32) {
    (scale1(a), scale1(b))
}
pub const fn scale3(a: i32, b: i32, c: i32) -> (i32, i32, i32) {
    (scale1(a), scale1(b), scale1(c))
}
pub const fn scale4(a: i32, b: i32, c: i32, d: i32) -> (i32, i32, i32, i32) {
    (scale1(a), scale1(b), scale1(c), scale1(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_scale_is_two_on_h700() {
        assert_eq!(FIXED_SCALE, 2, "h700 scales by 2; see platform.h:159");
    }

    #[test]
    fn scale_helpers_match_the_c_macros() {
        assert_eq!(scale1(30), 60);
        assert_eq!(scale2(10, 16), (20, 32));
    }
}
