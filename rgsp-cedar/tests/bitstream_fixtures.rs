//! Characterization tests against real vendor bytes captured off the device.
//! See rgsp-cedar/tests/fixtures/README.md.

use rgsp_cedar::bitstream::*;

fn fixture(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    std::fs::read(format!("{path}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn converts_a_real_avcc_frame_to_the_bytes_the_c_produced() {
    // Delta frame: raw == expected byte length, so this is a clean
    // conversion test with no parameter-set prepend to reason about.
    let raw = fixture("frame_delta_raw.bin");
    let expected = fixture("frame_delta_expected.bin");

    let mut out = Vec::new();
    let written = append_avcc_as_annexb(&mut out, &raw);

    assert_ne!(written, 0, "the fixture is AVCC; a 0 return means the parser rejected it");
    assert_eq!(out, expected, "byte-for-byte match with the pre-port C output");
}

#[test]
fn parses_the_real_avcc_record_into_annexb_parameter_sets() {
    let record = fixture("avcc_record.bin");
    assert_eq!(record[0], 0x01, "fixture must be an AVCDecoderConfigurationRecord");

    let sets = avcc_record_to_annexb(&record);

    assert!(!sets.is_empty());
    assert_eq!(&sets[..4], &[0, 0, 0, 1], "starts with an Annex-B start code");
    // One 10-byte SPS then one 4-byte PPS, each start-code prefixed:
    // 4+10+4+4 = 22 bytes, matching the device's logged "SPS/PPS: 22 bytes".
    assert_eq!(sets.len(), 22);
    assert_eq!(sets[4] & 0x1f, 7, "first NAL is the SPS");
    let second_start = sets[4 + 10..]
        .windows(4)
        .position(|w| w == [0, 0, 0, 1])
        .map(|p| p + 4 + 10)
        .unwrap();
    assert_eq!(second_start, 4 + 10, "second start code follows immediately after the SPS");
    assert_eq!(sets[second_start + 4] & 0x1f, 8, "second NAL is the PPS");
    assert!(starts_with_parameter_sets(&sets));
}

#[test]
fn the_first_frame_is_a_keyframe_carrying_its_parameter_sets() {
    let expected = fixture("frame_key_expected.bin");
    assert!(first_slice_is_idr(&expected));
    assert!(starts_with_parameter_sets(&expected));
}

#[test]
fn the_first_frame_conversion_matches_raw_plus_prepended_parameter_sets() {
    let raw = fixture("frame_key_raw.bin");
    let expected = fixture("frame_key_expected.bin");
    let sets = avcc_record_to_annexb(&fixture("avcc_record.bin"));

    let mut converted = Vec::new();
    append_avcc_as_annexb(&mut converted, &raw);

    let mut with_sets = sets.clone();
    with_sets.extend_from_slice(&converted);

    assert_eq!(with_sets, expected);
}

#[test]
fn a_forced_idr_gets_its_parameter_sets_prepended() {
    // The VideoToolbox case: the VE emits a client-requested IDR as a bare
    // type-5 NAL with no SPS/PPS. A software decoder reuses its cached sets
    // and does not care; VideoToolbox never starts decoding and sits in
    // "Waiting for IDR frame". So the expected output is longer than the raw
    // by exactly the parameter-set length.
    let raw = fixture("frame_forced_idr_raw.bin");
    let expected = fixture("frame_forced_idr_expected.bin");
    let sets = avcc_record_to_annexb(&fixture("avcc_record.bin"));

    let mut converted = Vec::new();
    append_avcc_as_annexb(&mut converted, &raw);

    assert!(first_slice_is_idr(&converted));
    assert!(!starts_with_parameter_sets(&converted), "the VE emits a bare type-5 NAL");
    assert_eq!(expected.len(), converted.len() + sets.len());
    assert_eq!(&expected[..sets.len()], &sets[..]);
    assert_eq!(&expected[sets.len()..], &converted[..]);
}

#[test]
fn a_second_forced_idr_gets_its_parameter_sets_prepended() {
    let raw = fixture("frame_forced_idr2_raw.bin");
    let expected = fixture("frame_forced_idr2_expected.bin");
    let sets = avcc_record_to_annexb(&fixture("avcc_record.bin"));

    let mut converted = Vec::new();
    append_avcc_as_annexb(&mut converted, &raw);

    assert!(first_slice_is_idr(&converted));
    assert!(!starts_with_parameter_sets(&converted), "the VE emits a bare type-5 NAL");
    assert_eq!(expected.len(), converted.len() + sets.len());
    assert_eq!(&expected[..sets.len()], &sets[..]);
    assert_eq!(&expected[sets.len()..], &converted[..]);
}
