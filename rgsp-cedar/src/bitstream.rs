//! Annex-B / AVCC byte handling. No unsafe, no vendor calls — the only part
//! of the capture path that can be tested without the device.
//!
//! Ported from `src/rgsp-cast.c:454-528` (`append_avcc_as_annexb`,
//! `annexb_first_slice_is_idr`, `annexb_starts_with_parameter_sets`) and the
//! avcC branch of `fetch_sps_pps` (`src/rgsp-cast.c:557-585`).

const START: [u8; 4] = [0, 0, 0, 1];

/// Append AVCC (4-byte length prefixes) to `out` as Annex-B start codes.
///
/// Returns the number of bytes appended, which is 0 when the input does not
/// parse as AVCC and the caller should append it untouched.
///
/// Unlike the C, allocation failure is not a case here (`Vec` panics on OOM),
/// so the C's `-1`/`*written` split collapses to this one return value.
pub fn append_avcc_as_annexb(out: &mut Vec<u8>, d: &[u8]) -> usize {
    // Already Annex-B? Pass through.
    if d.len() >= 4 && d[..4] == START {
        out.extend_from_slice(d);
        return d.len();
    }

    // Parse fully before appending anything: a malformed tail must leave `out`
    // untouched, or a partial conversion is handed on as a whole frame. (The C
    // appended as it went and relied on the caller discarding on a 0 return.)
    let mut written = 0usize;
    let mut off = 0usize;
    let mut nals: Vec<(usize, usize)> = Vec::new();
    while off + 4 <= d.len() {
        let len = u32::from_be_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]]) as usize;
        if len == 0 || off + 4 + len > d.len() {
            return 0; // not AVCC
        }
        nals.push((off + 4, len));
        written += 4 + len;
        off += 4 + len;
    }
    for (start, len) in nals {
        out.extend_from_slice(&START);
        out.extend_from_slice(&d[start..start + len]);
    }
    written
}

/// An IDR frame is one whose first slice NAL has type 5. Parameter sets (7, 8),
/// SEI (6) and access-unit delimiters (9) are skipped.
pub fn first_slice_is_idr(d: &[u8]) -> bool {
    for (_, nal_type) in nal_types(d) {
        if nal_type == 1 || nal_type == 5 {
            return nal_type == 5;
        }
    }
    false
}

/// True when the access unit already opens with a parameter set (7 or 8), so
/// the keyframe prepend does not duplicate sets the encoder itself emitted.
pub fn starts_with_parameter_sets(d: &[u8]) -> bool {
    for (_, nal_type) in nal_types(d) {
        // First NAL that carries picture data decides: a leading 7/8 means the
        // sets are already there, a leading slice means they are not.
        match nal_type {
            7 | 8 => return true,
            1 | 5 => return false,
            _ => {}
        }
    }
    false
}

/// Walk Annex-B start codes, yielding (offset of the NAL header, nal_unit_type).
/// Accepts both 3- and 4-byte start codes, like the C did.
fn nal_types(d: &[u8]) -> impl Iterator<Item = (usize, u8)> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i + 4 <= d.len() {
            let sc = if d[i..i + 4] == START {
                4
            } else if d[i..i + 3] == [0, 0, 1] {
                3
            } else {
                i += 1;
                continue;
            };
            if i + sc >= d.len() {
                i += 1;
                continue;
            }
            let at = i + sc;
            i += sc;
            return Some((at, d[at] & 0x1f));
        }
        None
    })
}

/// Convert an AVCDecoderConfigurationRecord into start-code-prefixed NALs.
///
/// The library hands back avcC, not Annex-B:
///   01 <profile> <compat> <level> <ff|lengthSizeMinusOne>
///   <e0|numSPS> [u16 len + SPS]...  <numPPS> [u16 len + PPS]...
///
/// Returns empty on anything that does not parse.
pub fn avcc_record_to_annexb(p: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if p.len() <= 7 || p[0] != 0x01 {
        // Not an avcC record — the caller passes the bytes through verbatim.
        return out;
    }
    let mut in_ = 5usize;
    let nsps = p[in_] & 0x1f;
    in_ += 1;
    for _ in 0..nsps {
        if in_ + 2 > p.len() {
            return out;
        }
        let len = u16::from_be_bytes([p[in_], p[in_ + 1]]) as usize;
        in_ += 2;
        if in_ + len > p.len() {
            return out;
        }
        out.extend_from_slice(&START);
        out.extend_from_slice(&p[in_..in_ + len]);
        in_ += len;
    }
    if in_ >= p.len() {
        return out;
    }
    let npps = p[in_];
    in_ += 1;
    for _ in 0..npps {
        if in_ + 2 > p.len() {
            return out;
        }
        let len = u16::from_be_bytes([p[in_], p[in_ + 1]]) as usize;
        in_ += 2;
        if in_ + len > p.len() {
            return out;
        }
        out.extend_from_slice(&START);
        out.extend_from_slice(&p[in_..in_ + len]);
        in_ += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_data_that_is_already_annexb() {
        let d = [0, 0, 0, 1, 0x65, 0xaa, 0xbb];
        let mut out = Vec::new();
        assert_eq!(append_avcc_as_annexb(&mut out, &d), d.len());
        assert_eq!(out, d);
    }

    #[test]
    fn rejects_a_length_prefix_that_overruns_the_buffer() {
        let d = [0, 0, 0, 99, 0x65];
        let mut out = Vec::new();
        assert_eq!(append_avcc_as_annexb(&mut out, &d), 0);
        assert!(out.is_empty(), "a rejected buffer must not leave partial output");
    }

    #[test]
    fn rejects_a_zero_length_prefix() {
        let d = [0, 0, 0, 0, 0x65];
        let mut out = Vec::new();
        assert_eq!(append_avcc_as_annexb(&mut out, &d), 0);
    }

    #[test]
    fn converts_two_concatenated_nals() {
        let d = [0, 0, 0, 2, 0x67, 0x01, 0, 0, 0, 1, 0x68];
        let mut out = Vec::new();
        assert_eq!(append_avcc_as_annexb(&mut out, &d), 11);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x01, 0, 0, 0, 1, 0x68]);
    }

    #[test]
    fn recognizes_a_three_byte_start_code() {
        // 0x65 = type 5, IDR slice.
        assert!(first_slice_is_idr(&[0, 0, 1, 0x65]));
    }

    #[test]
    fn skips_sei_and_aud_before_the_first_slice() {
        // type 9 (AUD), type 6 (SEI), then type 5 (IDR).
        let d = [0, 0, 0, 1, 0x09, 0, 0, 0, 1, 0x06, 0, 0, 0, 1, 0x65];
        assert!(first_slice_is_idr(&d));
        assert!(!starts_with_parameter_sets(&d));
    }

    #[test]
    fn a_non_idr_slice_is_not_a_keyframe() {
        assert!(!first_slice_is_idr(&[0, 0, 0, 1, 0x41]));
    }

    #[test]
    fn a_leading_parameter_set_is_detected() {
        assert!(starts_with_parameter_sets(&[0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65]));
    }

    #[test]
    fn a_malformed_avcc_record_yields_nothing() {
        assert!(avcc_record_to_annexb(&[0x02, 1, 2, 3, 4, 5, 6, 7, 8]).is_empty());
        assert!(avcc_record_to_annexb(&[0x01, 1, 2]).is_empty());
    }

    #[test]
    fn an_avcc_record_truncated_mid_sps_yields_what_parsed() {
        // version, profile/compat/level, ff, e1 (1 SPS), len=10, but only 2 bytes follow.
        let r = [0x01, 0x64, 0x00, 0x33, 0xff, 0xe1, 0x00, 0x0a, 0x67, 0x4d];
        assert!(avcc_record_to_annexb(&r).is_empty());
    }
}
