//! Raw NextUI bindings, plus the macros bindgen cannot carry.
#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]

include!(concat!(env!("OUT_DIR"), "/nextui.rs"));

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
