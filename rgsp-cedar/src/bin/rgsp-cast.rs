//! rgsp-cast — CLI front end for `rgsp_cedar::capture`.
//!
//! Captures the framebuffer to an Annex-B .h264 file and prints a timing
//! summary. All of the Cedar VE work lives in `rgsp_cedar::capture`; this
//! file is the loop, the flags and the file I/O.
//!
//! Transcribed from `src/rgsp-cast-cli.c`, which this replaces.
//!
//! DROPPED: the C's `-a PATH` (audio source: pump socket or tee file) and
//! `-A` (video only) flags. Both audio sources — a Unix socket fed by
//! rgsp-audio-pump, and an ALSA `type file` tee — were deleted when
//! snd-aloop replaced that approach, so neither can exist any more.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use clap::Parser;
use rgsp_cedar::capture::Capture;
use rgsp_cedar::vendor_abi::VencPixelFmt;

macro_rules! log {
    ($($arg:tt)*) => {{
        eprint!("[rgsp-cast] ");
        eprintln!($($arg)*);
    }};
}

/// rgsp-cast — capture the framebuffer to Annex-B H.264.
#[derive(Parser)]
#[command(disable_help_flag = false)]
struct Args {
    /// output Annex-B .h264
    #[arg(short = 'o', default_value = "cast.h264")]
    out: String,

    /// input format: 12=ARGB passthrough (default), 0=NV12
    #[arg(short = 'i', default_value_t = 12)]
    in_fmt: i32,

    /// target bitrate (default: encoder's)
    #[arg(short = 'b', default_value_t = 0)]
    bitrate: i32,

    /// capture duration in seconds
    #[arg(short = 'd', default_value_t = 30)]
    duration: i32,

    /// target frame rate
    #[arg(short = 'f', default_value_t = 30)]
    fps: i32,

    /// stop after N frames (overrides -d)
    #[arg(short = 'n', default_value_t = 0)]
    max_frames: i32,

    /// pass the framebuffer pitch in bytes rather than pixels
    #[arg(short = 'S')]
    stride_bytes: bool,

    /// dump the raw SPS/PPS parameter sets and exit
    #[arg(long = "dump-hdr")]
    dump_hdr: bool,

    /// verbose per-frame logging
    #[arg(short = 'v')]
    verbose: bool,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn hexdump(tag: &str, data: &[u8]) {
    eprint!("[rgsp-cast] {tag}:");
    for b in data {
        eprint!(" {b:02x}");
    }
    eprintln!();
}

fn pixel_fmt(v: i32) -> Option<VencPixelFmt> {
    match v {
        0 => Some(VencPixelFmt::Yuv420Sp),
        12 => Some(VencPixelFmt::Argb),
        13 => Some(VencPixelFmt::Rgba),
        14 => Some(VencPixelFmt::Abgr),
        15 => Some(VencPixelFmt::Bgra),
        _ => None,
    }
}

fn main() -> std::process::ExitCode {
    let mut args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| if args.verbose { "debug".into() } else { "info".into() },
        ))
        .init();

    if args.fps <= 0 {
        args.fps = 30;
    }
    if args.max_frames <= 0 {
        args.max_frames = args.duration * args.fps;
    }

    let Some(in_fmt) = pixel_fmt(args.in_fmt) else {
        log!("unknown input format {} (expected 0 or 12-15)", args.in_fmt);
        return std::process::ExitCode::from(2);
    };
    // Matches the C's `rgb_in = (in_fmt >= 12 && in_fmt <= 15)`: whether the
    // per-frame timing line below says "copy" (RGB passthrough) or "convert"
    // (CPU NV12 conversion).
    let rgb_in = args.in_fmt >= 12 && args.in_fmt <= 15;

    // SAFETY: on_signal only touches an atomic; installing it is safe at any
    // point in the program.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }

    let mut cap = match Capture::open_scaled_ex(0, 0, 0, 0, args.fps, args.bitrate, in_fmt, args.stride_bytes) {
        Ok(c) => c,
        Err(e) => {
            log!("{e:#}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut out = match File::create(&args.out) {
        Ok(f) => f,
        Err(e) => {
            log!("fopen({}): {e}", args.out);
            return std::process::ExitCode::FAILURE;
        }
    };

    // ── capture loop ────────────────────────────────────────────────────
    let t_start = Instant::now();
    let mut bytes_out: u64 = 0;
    let mut frames: u32 = 0;
    let mut keyframes: u32 = 0;

    while !STOP.load(Ordering::SeqCst) && frames < args.max_frames as u32 {
        let frame = match cap.next() {
            Ok(f) => f,
            Err(e) => {
                log!("{e:#}");
                break;
            }
        };

        if !frame.data.is_empty() {
            if let Err(e) = out.write_all(frame.data) {
                log!("write({}): {e}", args.out);
                break;
            }
        }
        bytes_out += frame.data.len() as u64;
        keyframes += u32::from(frame.is_keyframe);

        if args.dump_hdr {
            let hdr = cap.param_sets();
            hexdump("sps/pps", &hdr[..hdr.len().min(32)]);
            return std::process::ExitCode::SUCCESS;
        }

        frames += 1;
    }

    let secs = t_start.elapsed().as_secs_f64();
    let stats = cap.stats();

    log!(
        "captured {frames} frames ({keyframes} keyframes) in {secs:.1} s = {:.1} fps",
        if secs > 0.0 { frames as f64 / secs } else { 0.0 }
    );
    log!(
        "output {bytes_out} bytes = {:.0} kbps average",
        if secs > 0.0 { (bytes_out as f64 * 8.0 / 1000.0) / secs } else { 0.0 }
    );
    if frames > 0 {
        log!(
            "per frame: {} {:.2} ms, encode {:.2} ms",
            if rgb_in { "copy   " } else { "convert" },
            stats.convert_ns as f64 / 1e6 / frames as f64,
            stats.encode_ns as f64 / 1e6 / frames as f64
        );
    }
    if stats.short_reads > 0 {
        log!("warning: {} short framebuffer reads", stats.short_reads);
    }

    std::process::ExitCode::SUCCESS
}
