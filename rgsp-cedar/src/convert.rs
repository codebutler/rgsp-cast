//! CPU colour conversion — the reference path behind `in_fmt = Yuv420Sp`.
//!
//! The VE ingests the framebuffer's pixel format directly and does RGB->YUV in
//! its ISP block, so the production path does no CPU colour conversion:
//! 1.55 ms/frame of memcpy instead of 18.28 ms/frame of conversion. This
//! module is kept as a reference for comparing output.
//!
//! Transcribed from `src/rgsp-cast.c:283-356`.

#[inline]
fn clamp8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// BT.601 limited range, matching what the VE expects for NV12 input.
#[inline]
pub fn rgb_y(r: i32, g: i32, b: i32) -> u8 {
    clamp8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16)
}

#[inline]
pub fn rgb_u(r: i32, g: i32, b: i32) -> u8 {
    clamp8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128)
}

#[inline]
pub fn rgb_v(r: i32, g: i32, b: i32) -> u8 {
    clamp8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128)
}

/// Chroma is subsampled by averaging each 2x2 block, which is visibly cleaner
/// than point-sampling on the dithered gradients NextUI and GBA games produce.
pub fn bgra_to_nv12(src: &[u8], pitch: usize, w: usize, h: usize, dy: &mut [u8], duv: &mut [u8]) {
    for y in 0..h {
        let row = &src[y * pitch..];
        let outy = &mut dy[y * w..];
        for x in 0..w {
            let (b, g, r) = (
                row[x * 4] as i32,
                row[x * 4 + 1] as i32,
                row[x * 4 + 2] as i32,
            );
            outy[x] = rgb_y(r, g, b);
        }
    }
    let mut y = 0;
    while y < h {
        let r0 = &src[y * pitch..];
        let r1 = &src[if y + 1 < h { y + 1 } else { y } * pitch..];
        let outuv = &mut duv[(y / 2) * w..];
        let mut x = 0;
        while x < w {
            let x1 = if x + 1 < w { x + 1 } else { x };
            let b = (r0[x * 4] as i32
                + r0[x1 * 4] as i32
                + r1[x * 4] as i32
                + r1[x1 * 4] as i32)
                >> 2;
            let g = (r0[x * 4 + 1] as i32
                + r0[x1 * 4 + 1] as i32
                + r1[x * 4 + 1] as i32
                + r1[x1 * 4 + 1] as i32)
                >> 2;
            let r = (r0[x * 4 + 2] as i32
                + r0[x1 * 4 + 2] as i32
                + r1[x * 4 + 2] as i32
                + r1[x1 * 4 + 2] as i32)
                >> 2;
            outuv[x] = rgb_u(r, g, b);
            outuv[x + 1] = rgb_v(r, g, b);
            x += 2;
        }
        y += 2;
    }
}

#[inline]
fn r565(p: u16) -> i32 {
    (((p >> 11) & 0x1f) << 3) as i32
}

#[inline]
fn g565(p: u16) -> i32 {
    (((p >> 5) & 0x3f) << 2) as i32
}

#[inline]
fn b565(p: u16) -> i32 {
    ((p & 0x1f) << 3) as i32
}

/// As `bgra_to_nv12`, for a 16bpp framebuffer.
pub fn rgb565_to_nv12(src: &[u8], pitch: usize, w: usize, h: usize, dy: &mut [u8], duv: &mut [u8]) {
    let px = |row: usize, x: usize| -> u16 {
        let off = row * pitch + x * 2;
        u16::from_ne_bytes([src[off], src[off + 1]])
    };

    for y in 0..h {
        let outy = &mut dy[y * w..];
        for x in 0..w {
            let p = px(y, x);
            outy[x] = rgb_y(r565(p), g565(p), b565(p));
        }
    }
    let mut y = 0;
    while y < h {
        let y1 = if y + 1 < h { y + 1 } else { y };
        let outuv = &mut duv[(y / 2) * w..];
        let mut x = 0;
        while x < w {
            let x1 = if x + 1 < w { x + 1 } else { x };
            let (a, b_, c, d) = (px(y, x), px(y, x1), px(y1, x), px(y1, x1));
            let r = (r565(a) + r565(b_) + r565(c) + r565(d)) >> 2;
            let g = (g565(a) + g565(b_) + g565(c) + g565(d)) >> 2;
            let b = (b565(a) + b565(b_) + b565(c) + b565(d)) >> 2;
            outuv[x] = rgb_u(r, g, b);
            outuv[x + 1] = rgb_v(r, g, b);
            x += 2;
        }
        y += 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_and_white_land_on_the_limited_range_endpoints() {
        // BT.601 limited range: black is Y=16, white is Y=235, and neutral
        // colour is U=V=128 at both ends.
        assert_eq!(rgb_y(0, 0, 0), 16);
        assert_eq!(rgb_y(255, 255, 255), 235);
        assert_eq!(rgb_u(0, 0, 0), 128);
        assert_eq!(rgb_v(0, 0, 0), 128);
        assert_eq!(rgb_u(255, 255, 255), 128);
        assert_eq!(rgb_v(255, 255, 255), 128);
    }

    #[test]
    fn primaries_push_chroma_to_the_expected_side() {
        // Blue is the positive end of U (Cb), red the positive end of V (Cr);
        // 128 is the neutral point either way.
        assert!(rgb_u(0, 0, 255) > 200, "{}", rgb_u(0, 0, 255));
        assert!(rgb_v(0, 0, 255) < 128, "{}", rgb_v(0, 0, 255));
        assert!(rgb_v(255, 0, 0) > 200, "{}", rgb_v(255, 0, 0));
        assert!(rgb_u(255, 0, 0) < 128, "{}", rgb_u(255, 0, 0));
        // Green carries most of the luma weight, then red, then blue.
        assert!(rgb_y(0, 255, 0) > rgb_y(255, 0, 0));
        assert!(rgb_y(255, 0, 0) > rgb_y(0, 0, 255));
    }

    #[test]
    fn clamping_holds_at_both_ends() {
        assert_eq!(clamp8(-1), 0);
        assert_eq!(clamp8(256), 255);
        assert_eq!(clamp8(128), 128);
    }

    #[test]
    fn bgra_white_converts_to_flat_luma_and_neutral_chroma() {
        let (w, h) = (4usize, 4usize);
        let pitch = w * 4;
        let src = vec![0xffu8; pitch * h];
        let mut dy = vec![0u8; w * h];
        let mut duv = vec![0u8; w * h / 2];
        bgra_to_nv12(&src, pitch, w, h, &mut dy, &mut duv);
        assert!(dy.iter().all(|&y| y == 235), "{dy:?}");
        assert!(duv.iter().all(|&c| c == 128), "{duv:?}");
    }

    #[test]
    fn bgra_reads_the_channels_in_framebuffer_order() {
        // /dev/fb0 lays a pixel out as B,G,R,A. A pure-red pixel is therefore
        // 00 00 ff ff, and must come back as red, not blue.
        let (w, h) = (2usize, 2usize);
        let pitch = w * 4;
        let mut src = vec![0u8; pitch * h];
        for p in src.chunks_mut(4) {
            p.copy_from_slice(&[0x00, 0x00, 0xff, 0xff]);
        }
        let mut dy = vec![0u8; w * h];
        let mut duv = vec![0u8; w * h / 2];
        bgra_to_nv12(&src, pitch, w, h, &mut dy, &mut duv);
        assert_eq!(dy[0], rgb_y(255, 0, 0));
        assert_eq!(duv[0], rgb_u(255, 0, 0));
        assert_eq!(duv[1], rgb_v(255, 0, 0));
    }

    #[test]
    fn rgb565_white_converts_to_flat_luma_and_neutral_chroma() {
        // 5/6-bit channels are expanded by shifting left, not by replicating
        // the high bits, so 0xffff is (248, 252, 248) rather than white -
        // Y=231, not the 235 an 8-bit white gives, and chroma lands one step
        // off neutral at 127 because green expands further than red and blue.
        // Transcribed from the C as it stands.
        let (w, h) = (4usize, 4usize);
        let pitch = w * 2;
        let mut src = vec![0u8; pitch * h];
        for p in src.chunks_mut(2) {
            p.copy_from_slice(&0xffffu16.to_ne_bytes());
        }
        let mut dy = vec![0u8; w * h];
        let mut duv = vec![0u8; w * h / 2];
        rgb565_to_nv12(&src, pitch, w, h, &mut dy, &mut duv);
        assert!(dy.iter().all(|&y| y == 231), "{dy:?}");
        assert!(duv.iter().all(|&c| c == 127), "{duv:?}");
    }

    #[test]
    fn rgb565_black_is_the_luma_floor() {
        let (w, h) = (2usize, 2usize);
        let pitch = w * 2;
        let src = vec![0u8; pitch * h];
        let mut dy = vec![0u8; w * h];
        let mut duv = vec![0u8; w * h / 2];
        rgb565_to_nv12(&src, pitch, w, h, &mut dy, &mut duv);
        assert!(dy.iter().all(|&y| y == 16));
        assert!(duv.iter().all(|&c| c == 128));
    }

    #[test]
    fn chroma_averages_a_2x2_block_rather_than_point_sampling() {
        // Two white pixels and two black ones in a 2x2 block average to a mid
        // grey; point-sampling the top-left would give white's chroma and, more
        // visibly, the wrong luma weighting on dithered gradients.
        let (w, h) = (2usize, 2usize);
        let pitch = w * 4;
        let mut src = vec![0u8; pitch * h];
        // Top row white, bottom row black (BGRA).
        src[0..8].copy_from_slice(&[0xff; 8]);
        let mut dy = vec![0u8; w * h];
        let mut duv = vec![0u8; w * h / 2];
        bgra_to_nv12(&src, pitch, w, h, &mut dy, &mut duv);
        assert_eq!(dy[0], 235);
        assert_eq!(dy[2], 16);
        // (255 + 255 + 0 + 0) >> 2 == 127 in each channel.
        assert_eq!(duv[0], rgb_u(127, 127, 127));
        assert_eq!(duv[1], rgb_v(127, 127, 127));
    }
}
