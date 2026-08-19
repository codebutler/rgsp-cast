//! Replaces tests/test_idr_cadence.c. Runs only on the device.
//!
//! Does `Capture::request_idr()` actually reach the encoder?
//!
//! `capture_api.rs` asserts that the frame after a request is a keyframe,
//! which passes spuriously if the encoder was about to emit one anyway at a
//! GOP boundary. That matters here because the vendor parameter index used by
//! `request_idr()` (`VENC_IndexParamForceKeyFrame = 0x6`) is reconstructed
//! from the standard CedarX enum ordering rather than read from a vendor
//! header — the same shape of hazard as `0x101` vs `16` (see
//! `vendor_abi.rs`). A silently-wrong index would leave Moonlight unable to
//! recover from packet loss.
//!
//! So: measure the encoder's natural keyframe cadence first, then force an
//! IDR twice at different off-cadence offsets and require both to land. Two
//! forced keyframes away from any natural boundary is convincing; one is not.

mod common;

use common::{on_device, LOCK};
use rgsp_cedar::capture::Capture;

const OBSERVE: usize = 120;
const OFFSETS: [usize; 2] = [17, 29];

#[test]
fn forced_idr_lands_off_cadence_at_two_different_offsets() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");

    // ── phase 1: natural cadence, no IDR requests ──────────────────────
    let mut key = [false; OBSERVE];
    for slot in key.iter_mut() {
        *slot = cap.next().expect("frame").is_keyframe;
    }

    eprintln!("phase 1: natural keyframe pattern over {OBSERVE} frames (K=keyframe)");
    let pattern: String = key.iter().map(|&k| if k { 'K' } else { '.' }).collect();
    eprintln!("  {pattern}");

    let mut kf = 0usize;
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    let mut min_gap = OBSERVE + 1;
    for (i, &k) in key.iter().enumerate() {
        if !k {
            continue;
        }
        kf += 1;
        if first.is_none() {
            first = Some(i);
        }
        if let Some(l) = last {
            min_gap = min_gap.min(i - l);
        }
        last = Some(i);
    }
    let first = first.unwrap_or(0);
    eprintln!("  {kf} keyframe(s); first at frame {first}");

    // A forced IDR only proves anything if the encoder was not about to emit
    // one regardless.
    let gop = if kf < 2 { OBSERVE } else { min_gap };
    assert!(
        gop >= 4,
        "natural keyframe interval is {gop} frames, too short to distinguish \
         a forced IDR from the encoder's own cadence"
    );

    // ── phase 2: force an IDR twice, at two different off-cadence points ──
    //
    // The encoder keeps emitting its own keyframes every `gop` frames
    // throughout, so keyframes appearing in the run-up are expected and
    // prove nothing either way. What makes a trial conclusive is where the
    // *forced* frame sits: if it is several frames away from the nearest
    // natural boundary, the encoder was not about to emit a keyframe there.
    let mut next_index = OBSERVE;
    let mut fails = 0;

    eprintln!("\nphase 2: forcing an IDR at two off-cadence offsets");
    eprintln!("  (natural keyframes predicted at frame {first} + n*{gop})");
    for (t, &offset) in OFFSETS.iter().enumerate() {
        let mut stray = 0;
        let mut len_before = 0;
        for _ in 0..offset {
            let f = cap.next().expect("frame");
            if f.is_keyframe {
                stray += 1;
            }
            len_before = f.data.len();
            next_index += 1;
        }

        cap.request_idr();
        let forced = cap.next().expect("forced frame");
        let len_forced = forced.data.len();
        let forced_at = next_index;
        next_index += 1;

        // Distance from the forced frame to the nearest predicted boundary.
        let off = (forced_at as isize - first as isize).rem_euclid(gop as isize) as usize;
        let dist = off.min(gop - off);

        eprintln!(
            "  trial {}: forced frame is index {forced_at}; nearest natural \
             keyframe {dist} frames away",
            t + 1
        );
        eprintln!(
            "           keyframe={}; size {len_forced} bytes vs {len_before} for the \
             preceding frame ({:.0}x)",
            forced.is_keyframe as u8,
            if len_before > 0 {
                len_forced as f64 / len_before as f64
            } else {
                0.0
            }
        );
        eprintln!(
            "           ({stray} keyframes in the {offset}-frame run-up, as the \
             natural cadence predicts)"
        );

        if dist < 2 {
            eprintln!(
                "           INCONCLUSIVE: forced frame sits on a natural boundary, \
                 so this trial proves nothing"
            );
            fails += 1;
        } else if !forced.is_keyframe {
            eprintln!("           FAIL: forced IDR did not produce a keyframe");
            fails += 1;
        }
    }

    assert_eq!(
        fails, 0,
        "{fails} of 2 forced IDRs did not land - VENC_IndexParamForceKeyFrame \
         is probably the wrong index"
    );
    eprintln!("\nPASS: both forced IDRs landed off-cadence (natural interval >= {gop} frames)");
}
