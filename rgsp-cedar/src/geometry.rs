//! Pillarbox/letterbox geometry for the VE's scaler.

/// The padded input surface the panel image is centred in, so the VE's scale
/// to the negotiated size preserves proportions instead of stretching. When no
/// padding is needed these are the panel's own dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pillarbox {
    /// Padded input surface, in pixels.
    pub pad_w: u32,
    pub pad_h: u32,
    /// Where the panel image sits in it.
    pub pad_x: u32,
    pub pad_y: u32,
    /// True when the black bars actually exist.
    pub padded: bool,
}

impl Pillarbox {
    /// Pillarbox/letterbox: the VE stretches input geometry to destination
    /// geometry with no regard for aspect ratio, and GameStream requires the
    /// stream to be exactly the size the client negotiated - so the padding
    /// has to happen on the input side. Grow the input surface to the
    /// destination's aspect ratio, centre the panel image in it, and leave the
    /// remainder black. 720x480 (3:2) into 1280x720 (16:9) gives 848x480 with
    /// 64px bars either side, which the VE then scales evenly.
    ///
    /// Only widening or heightening, never cropping: the whole panel is
    /// always visible. 16-alignment keeps the VE happy.
    ///
    /// `dst_w`/`dst_h` of 0 mean "no scaling": the source dimensions are
    /// returned unpadded.
    pub fn for_target(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Self {
        let mut p = Pillarbox { pad_w: src_w, pad_h: src_h, pad_x: 0, pad_y: 0, padded: false };
        if dst_w == 0
            || dst_h == 0
            || (dst_w as u64) * (src_h as u64) == (dst_h as u64) * (src_w as u64)
        {
            return p;
        }
        if (dst_w as u64) * (src_h as u64) > (dst_h as u64) * (src_w as u64) {
            let want = ((src_h as u64) * (dst_w as u64) / (dst_h as u64)) as u32;
            // Nearest 16, not next-16: the alignment error becomes residual
            // aspect distortion, and rounding to nearest halves it.
            p.pad_w = (want + 8) & !15u32;
            if p.pad_w < src_w {
                p.pad_w = (src_w + 15) & !15u32;
            }
        } else {
            let want = ((src_w as u64) * (dst_h as u64) / (dst_w as u64)) as u32;
            p.pad_h = (want + 8) & !15u32;
            if p.pad_h < src_h {
                p.pad_h = (src_h + 15) & !15u32;
            }
        }
        p.pad_x = (p.pad_w - src_w) / 2;
        p.pad_y = (p.pad_h - src_h) / 2;
        p.padded = true;
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_case_720x480_panel_into_a_720p_stream() {
        // The VE stretches input geometry to destination geometry with no
        // regard for aspect ratio, and GameStream requires the stream to be
        // exactly the size the client negotiated - so the padding happens on
        // the input side. 720x480 (3:2) into 1280x720 (16:9) gives 848x480
        // with 64px bars either side, which the VE then scales evenly.
        let p = Pillarbox::for_target(720, 480, 1280, 720);
        assert_eq!((p.pad_w, p.pad_h), (848, 480));
        assert_eq!((p.pad_x, p.pad_y), (64, 0));
        assert!(p.padded);
    }

    #[test]
    fn a_matching_aspect_ratio_needs_no_bars() {
        let p = Pillarbox::for_target(720, 480, 1440, 960);
        assert_eq!((p.pad_w, p.pad_h), (720, 480));
        assert!(!p.padded);
    }

    #[test]
    fn zero_destination_means_no_scaling() {
        let p = Pillarbox::for_target(720, 480, 0, 0);
        assert_eq!((p.pad_w, p.pad_h), (720, 480));
        assert_eq!((p.pad_x, p.pad_y), (0, 0));
        assert!(!p.padded);
    }

    #[test]
    fn a_taller_destination_letterboxes_instead() {
        // 720x480 into 480x800: the destination is narrower than the source,
        // so the input grows vertically and the bars are top and bottom.
        let p = Pillarbox::for_target(720, 480, 480, 800);
        assert_eq!((p.pad_w, p.pad_h), (720, 1200));
        assert_eq!((p.pad_x, p.pad_y), (0, 360));
        assert!(p.padded);
    }

    #[test]
    fn padded_dimensions_are_always_16_aligned() {
        // The VE wants 16-aligned dimensions.
        for dst_w in [640u32, 1280, 1920, 854] {
            for dst_h in [360u32, 720, 1080, 480] {
                let p = Pillarbox::for_target(720, 480, dst_w, dst_h);
                assert_eq!(p.pad_w % 16, 0, "{dst_w}x{dst_h} pad_w={}", p.pad_w);
                assert_eq!(p.pad_h % 16, 0, "{dst_w}x{dst_h} pad_h={}", p.pad_h);
            }
        }
    }

    #[test]
    fn the_panel_image_is_never_cropped() {
        // Only widening or heightening, never cropping: the whole panel is
        // always visible.
        for dst_w in [320u32, 640, 1280, 1920] {
            for dst_h in [240u32, 480, 720, 1080] {
                let p = Pillarbox::for_target(720, 480, dst_w, dst_h);
                assert!(p.pad_w >= 720, "{dst_w}x{dst_h}");
                assert!(p.pad_h >= 480, "{dst_w}x{dst_h}");
                assert!(p.pad_x + 720 <= p.pad_w);
                assert!(p.pad_y + 480 <= p.pad_h);
            }
        }
    }

    #[test]
    fn rounding_is_to_nearest_16_not_next_16() {
        // The alignment error becomes residual aspect distortion, and rounding
        // to nearest halves it. src 480x720 into dst 1280x720: want = h *
        // dst_w / dst_h = 720 * 1280 / 720 = 1280, already 16-aligned, so this
        // exercises the branch where rounding is a no-op.
        let p = Pillarbox::for_target(480, 720, 1280, 720);
        assert_eq!(p.pad_w, 1280);
        assert_eq!(p.pad_x, 400);
    }
}
