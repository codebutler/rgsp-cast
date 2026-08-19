//! The capture object: open the VE, encode one framebuffer frame per call,
//! tear down in the documented order.
//!
//! Transcribed from `src/rgsp-cast.c` — `rgsp_capture_open_scaled_ex`
//! (622-830), `rgsp_capture_next` (847-1042), `rgsp_capture_request_idr` /
//! `rgsp_capture_param_sets` / `rgsp_capture_stats` (1045-1064) and
//! `rgsp_capture_close` (1066-1088), which becomes `Drop`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Result};
use tracing::{debug, info, warn};

use crate::bitstream;
use crate::convert;
use crate::framebuffer::{validate_geometry, Framebuffer};
use crate::geometry::Pillarbox;
use crate::vendor_abi::{
    index, ScMemOpsS, VencAllocateBufferParam, VencBaseConfig, VencCodecType, VencHeaderData,
    VencInputBuffer, VencOutputBuffer, VencPixelFmt,
};
use crate::vendor_lib::{VendorLibs, VideoEncoder};

/// The Annex-B bitstream of one encoded frame. The slice is owned by the
/// capture and is invalidated by the next call.
pub struct Frame<'a> {
    pub data: &'a [u8],
    pub is_keyframe: bool,
}

/// Host-side timing and health counters, as `rgsp_capture_stats` reported them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub convert_ns: i64,
    pub encode_ns: i64,
    pub short_reads: u32,
}

/// At most one Capture may exist per process.
///
/// The vendor libraries are process-global (dlopen'd once, one Cedar video
/// engine, one framebuffer), so a second capture was never meaningful. The C
/// went further and kept a static error buffer that two captures would race
/// on; this port replaces that with a per-capture error, and the guard is what
/// makes that replacement safe rather than merely tidy.
static CAPTURE_OPEN: AtomicBool = AtomicBool::new(false);

/// The largest parameter-set blob we will keep, matching the C's
/// `unsigned char sps_pps[512]`. A longer one is treated as unavailable.
const SPS_PPS_CAP: usize = 512;

/// The sentinel the vendor guard is filled with. NOT zero: most of what the
/// vendor libraries write past `used` is zeroes, so a zero-filled guard
/// reproduces the exact blind spot that hid the corruption in the C for
/// months. `tests/vendor_overspill.rs` measures the spill against this value.
const GUARD_FILL: u8 = 0xAA;

/// `#[repr(C)]` is not cosmetic here — see the comment on `_vendor_guard`.
#[repr(C)]
pub struct Capture {
    /// framebuffer
    fb: Option<Framebuffer>,
    w: u32,
    h: u32,
    bpp: u32,
    pitch: u32,
    frame_bytes: usize,
    fb_buf: Vec<u8>,

    /// encoder
    libs: &'static VendorLibs,
    enc: *mut VideoEncoder,
    memops: *mut ScMemOpsS,
    mem_open: bool,
    buffers_alloced: bool,
    enc_inited: bool,
    /// True while an input buffer is checked out of the vendor pool.
    held: bool,
    in_fmt: VencPixelFmt,
    rgb_in: bool,

    /// Pillarbox geometry: the panel image is copied into a padded input
    /// buffer whose aspect ratio matches the destination, so the VE's scale to
    /// the negotiated size preserves proportions instead of stretching. When
    /// no padding is needed these are the panel's own dimensions.
    pad_w: u32,
    pad_h: u32,
    pad_x: u32,
    pad_y: u32,
    /// True when the black bars actually exist.
    padded: bool,
    /// The black bars are painted once.
    bars_cleared: bool,

    /// Annex-B output, reused across frames; grows on demand.
    out: Vec<u8>,

    sps_pps: Vec<u8>,
    sps_pps_fetched: bool,

    /// pacing and counters
    fps: i32,
    frame_ns: i64,
    deadline: i64,
    frames: i32,
    force_idr: bool,
    short_reads: u32,
    convert_ns: i64,
    encode_ns: i64,

    /// Sticky death flag. A failed `next()` can leave the vendor input buffer
    /// un-acquired or already submitted, so the object is not safe to drive
    /// again; the stored error preserves the original diagnosis across later
    /// calls.
    failed: Option<anyhow::Error>,

    /// Vendor-written structs go LAST, and nothing may be added after them.
    ///
    /// The vendor libraries write past the end of VencInputBuffer, beyond even
    /// its _tail[256] padding. Measured with a 0xAA sentinel guard on-device:
    /// the AlreadyUsedInputBuffer/ReturnOneAllocInputBuffer pair modifies up to
    /// **+24 bytes past the end of `used`**, every frame. Most of that write is
    /// zeroes, which is why it is invisible to a scan for non-zero bytes.
    ///
    /// As stack locals in the old C main() the spill landed on adjacent scratch
    /// and was harmless, which is why it went unnoticed for so long. As struct
    /// members it lands on live fields: out_buf sat at +16..+23 past `used` and
    /// was nulled every frame, segfaulting on the first one. Keeping the pair
    /// adjacent and in this order reproduces the layout the vendor libs have
    /// always been fed, and _vendor_guard (4096, vs the 24 observed) absorbs
    /// the spill. Nothing may be added after them.
    ///
    /// `#[repr(C)]` on this struct is not cosmetic: Rust's default layout
    /// reorders fields freely, which would put live data where the guard is
    /// supposed to be. This is the whole reason the C bug happened.
    inbuf: VencInputBuffer,
    used: VencInputBuffer,
    /// Filled with 0xAA, never zeroed: most of what the vendor writes past
    /// `used` is zeroes, so a zero-filled guard reproduces the exact blind
    /// spot that hid the corruption in the C for months.
    _vendor_guard: [u8; 4096],
}

/// `Capture` can be sent to other threads: the single-instance guard
/// (`CAPTURE_OPEN`) means there is never a second one to race with over the
/// process-global vendor state the raw pointers refer to.
unsafe impl Send for Capture {}

/// `GetOneBitstreamFrame()` fills a `VencOutputBuffer`, so it is vendor-written
/// on every frame and in principle carries the same overspill risk that `used`
/// turned out to have — and in the C it moved from the old main()'s large stack
/// frame, which had scratch after it, into next()'s smaller one where the
/// neighbours are live locals and the return address.
///
/// Measured with a 0xAA sentinel on-device, the spill is **+0 bytes**: unlike
/// VencInputBuffer, this struct is written strictly within its declared extent
/// (its own _tail[256] included). The guard is therefore precaution, not a fix
/// for a live bug — it is kept because it costs 256 bytes of stack and makes
/// the struct safe wherever it is declared, in line with the standing rule that
/// every vendor struct carries trailing slack. VencBaseConfig and
/// VencAllocateBufferParam measured +0 as well and are left as plain locals.
#[repr(C)]
struct GuardedOutputBuffer {
    ob: VencOutputBuffer,
    guard: [u8; 256],
}

fn now_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: writes a timespec we own.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

impl Capture {
    /// Hand the framebuffer to the VE untouched. Allwinner names the formats by
    /// 32-bit word order, so `VencPixelFmt::Argb` (12) is the one whose byte
    /// layout is B,G,R,A — exactly /dev/fb0. Verified against the CPU
    /// conversion path at 42.2 dB PSNR on identical screen content.
    pub fn open(width: u32, height: u32, fps: i32, bitrate: i32) -> Result<Box<Capture>> {
        Self::open_scaled(width, height, 0, 0, fps, bitrate)
    }

    /// Capture at `width`x`height` but encode at `dst_w`x`dst_h`, scaled by the
    /// VE. A GameStream client rejects any stream whose resolution is not the
    /// one it negotiated - it never assembles a frame, sits in "Waiting for IDR
    /// frame" and eventually drops the connection - so the host must encode at
    /// the negotiated size even though the panel is 720x480. Pass 0 for no
    /// scaling.
    pub fn open_scaled(
        width: u32,
        height: u32,
        dst_w: u32,
        dst_h: u32,
        fps: i32,
        bitrate: i32,
    ) -> Result<Box<Capture>> {
        Self::open_scaled_ex(width, height, dst_w, dst_h, fps, bitrate, VencPixelFmt::Argb, false)
    }

    /// `dst_w`/`dst_h` of 0 mean "same as the source", i.e. no scaling. The VE
    /// keeps input and destination geometry as separate fields, so a non-zero
    /// dst asks the hardware to scale during encode - which is what GameStream
    /// needs: the client negotiates a resolution and rejects a stream that is
    /// not exactly it.
    ///
    /// `stride_bytes` selects the framebuffer's own pitch as the VE's stride
    /// rather than its width; it has no effect once padding exists.
    #[allow(clippy::too_many_arguments)]
    pub fn open_scaled_ex(
        width: u32,
        height: u32,
        dst_w: u32,
        dst_h: u32,
        fps: i32,
        bitrate: i32,
        in_fmt: VencPixelFmt,
        stride_bytes: bool,
    ) -> Result<Box<Capture>> {
        let fps = if fps <= 0 { 30 } else { fps };

        let libs = VendorLibs::load()?;

        // Claim the single capture slot before any state exists, so both the
        // failure path and the normal drop release it exactly once.
        if CAPTURE_OPEN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            bail!("a capture is already open");
        }

        // Boxed before any vendor call: the vendor libraries are handed
        // `&mut c.inbuf` and a moved struct would relocate it mid-session.
        // Every acquisition sets its teardown flag immediately, so returning
        // an error here drops the box and runs the C's `goto fail` teardown in
        // the documented order.
        let mut c = Box::new(Capture {
            fb: None,
            w: 0,
            h: 0,
            bpp: 0,
            pitch: 0,
            frame_bytes: 0,
            fb_buf: Vec::new(),
            libs,
            enc: std::ptr::null_mut(),
            memops: std::ptr::null_mut(),
            mem_open: false,
            buffers_alloced: false,
            enc_inited: false,
            held: false,
            in_fmt,
            rgb_in: false,
            pad_w: 0,
            pad_h: 0,
            pad_x: 0,
            pad_y: 0,
            padded: false,
            bars_cleared: false,
            out: Vec::new(),
            sps_pps: Vec::new(),
            sps_pps_fetched: false,
            fps,
            frame_ns: 0,
            deadline: 0,
            frames: 0,
            force_idr: false,
            short_reads: 0,
            convert_ns: 0,
            encode_ns: 0,
            failed: None,
            inbuf: VencInputBuffer::default(),
            used: VencInputBuffer::default(),
            _vendor_guard: [GUARD_FILL; 4096],
        });

        c.init(width, height, dst_w, dst_h, bitrate, stride_bytes)?;
        Ok(c)
    }

    fn init(
        &mut self,
        width: u32,
        height: u32,
        dst_w: u32,
        dst_h: u32,
        bitrate: i32,
        stride_bytes: bool,
    ) -> Result<()> {
        // ── framebuffer ─────────────────────────────────────────────────
        let fb = Framebuffer::open()?;
        let geo = fb.geometry();
        self.fb = Some(fb);

        let (w, h, bpp, pitch) = (geo.w, geo.h, geo.bpp, geo.pitch);
        // The VE wants 16-aligned dimensions; 720x480 already satisfies this.
        if w % 16 != 0 || h % 16 != 0 {
            warn!("{w}x{h} is not 16-aligned, VE may reject it");
        }
        // Framebuffer::open() only validates the bit depth. The caller's
        // requested size still has to be checked against the panel, or the
        // C's "scaling is not supported" error silently disappears.
        validate_geometry(w, h, bpp, width, height)?;

        self.w = w;
        self.h = h;
        self.bpp = bpp;
        self.pitch = pitch;

        let p = Pillarbox::for_target(w, h, dst_w, dst_h);
        self.pad_w = p.pad_w;
        self.pad_h = p.pad_h;
        self.pad_x = p.pad_x;
        self.pad_y = p.pad_y;
        self.padded = p.padded;
        if p.padded {
            info!(
                "pillarbox: {w}x{h} panel centred in a {}x{} surface for a {dst_w}x{dst_h} target",
                p.pad_w, p.pad_h
            );
        }

        self.frame_bytes = pitch as usize * h as usize;
        self.fb_buf = vec![0u8; self.frame_bytes];

        info!(
            "framebuffer {w}x{h} {bpp}bpp pitch={pitch} virtual={}x{}",
            geo.xres_virtual, geo.yres_virtual
        );
        // smem_start is the framebuffer's physical address. If it is exposed,
        // the VE may be able to DMA straight out of it and skip the copy into
        // ION.
        info!(
            "fb physical: smem_start=0x{:x} smem_len={}",
            geo.smem_start, geo.smem_len
        );

        // ── encoder ─────────────────────────────────────────────────────
        // SAFETY: both entry points were resolved at load time and take the
        // arguments their C prototypes declare.
        let (veops, memops) = unsafe {
            ((self.libs.get_ve_ops_s)(0), (self.libs.mem_adapter_get_ops_s)())
        };
        self.memops = memops;
        if veops.is_null() || memops.is_null() {
            bail!("ops NULL");
        }
        // SAFETY: memops points at the vendor's own static op table.
        let ops = unsafe { &*memops };
        let open = ops.open.ok_or_else(|| anyhow!("CdcMemOpen missing"))?;
        // SAFETY: vendor entry point, no arguments.
        if unsafe { open() } < 0 {
            bail!("CdcMemOpen failed");
        }
        self.mem_open = true;

        if let Some(off) = ops.get_ve_addr_offset {
            // SAFETY: vendor entry point, no arguments.
            info!("ve_addr_offset=0x{:x}", unsafe { off() });
        }

        // SAFETY: vendor entry point; the returned handle is opaque and only
        // ever passed back in.
        self.enc = unsafe { (self.libs.video_enc_create)(VencCodecType::H264) };
        if self.enc.is_null() {
            bail!("VideoEncCreate failed");
        }

        // Bitrate is a generic parameter, applied by VideoEncInit. 0 leaves
        // the encoder default alone, which is what the CLI has always used.
        if bitrate > 0 {
            let set = self
                .libs
                .video_enc_set_parameter
                .ok_or_else(|| anyhow!("VideoEncSetParameter missing; cannot set bitrate"))?;
            let mut br = bitrate;
            // SAFETY: the vendor reads an int through the pointer for this index.
            let rc = unsafe { set(self.enc, index::BITRATE, &mut br as *mut i32 as *mut c_void) };
            if rc != 0 {
                bail!("VideoEncSetParameter(bitrate={bitrate}) failed");
            }
        }

        let mut bcfg = VencBaseConfig::default();
        // Ask the encoder for NALU output, which emits SPS/PPS in-band ahead of
        // the IDR. Without it the parameter sets live in VE SRAM that the CPU
        // cannot read (VideoEncGetParameter hands back a VE bus address), and
        // the stream is undecodable without hardcoding them per resolution.
        bcfg.b_enc_h264_nalu = 1;
        bcfg.n_input_width = self.pad_w;
        bcfg.n_input_height = self.pad_h;
        bcfg.n_dst_width = if dst_w > 0 { dst_w } else { self.pad_w };
        bcfg.n_dst_height = if dst_h > 0 { dst_h } else { self.pad_h };
        // Stride describes the padded surface we hand the VE, not /dev/fb0's
        // pitch: once padding exists the two differ and the rows would shear.
        bcfg.n_stride = if self.padded {
            self.pad_w
        } else if stride_bytes {
            pitch
        } else {
            w
        };
        bcfg.e_input_format = self.in_fmt;
        bcfg.memops = memops as *mut c_void;
        bcfg.ve_ops_s = veops;
        bcfg.p_ve_ops_self = std::ptr::null_mut();

        // SAFETY: bcfg outlives the call and matches the vendor layout.
        if unsafe { (self.libs.video_enc_init)(self.enc, &mut bcfg) } != 0 {
            bail!("VideoEncInit failed");
        }
        self.enc_inited = true;

        let mut bp = VencAllocateBufferParam::default();
        self.rgb_in = matches!(
            self.in_fmt,
            VencPixelFmt::Argb | VencPixelFmt::Rgba | VencPixelFmt::Abgr | VencPixelFmt::Bgra
        );
        bp.n_buffer_num = 1;
        bp.n_size_y = if self.rgb_in {
            self.pad_w * self.pad_h * 4
        } else {
            self.pad_w * self.pad_h
        };
        bp.n_size_c = if self.rgb_in { 0 } else { self.pad_w * self.pad_h / 2 };
        // SAFETY: bp outlives the call and matches the vendor layout.
        if unsafe { (self.libs.alloc_input_buffer)(self.enc, &mut bp) } != 0 {
            bail!("AllocInputBuffer failed");
        }
        self.buffers_alloced = true;

        // The VE defaults to an IDR every 25 frames. At 60 fps that is a ~55 KB
        // burst every 0.4 s, which is the single largest thing this stream asks
        // of the handheld's WiFi and the reason the picture stalls: lose one
        // packet of a keyframe and the client discards the whole frame.
        //
        // GameStream does not need periodic keyframes - the client asks for one
        // (RequestIdrFrame) whenever it needs to recover, which is already wired
        // to VENC_IndexParamForceKeyFrame. So push the automatic interval far
        // out and let the client drive it.
        //
        // Index 2 is reconstructed from the same enum as ForceKeyFrame (6) and
        // Bitrate (0), both confirmed by behaviour; this one is confirmed the
        // same way, by watching the keyframe cadence change. A failure here is
        // not fatal - it just means the default cadence stays.
        {
            let mut interval: i32 = if self.fps > 0 { self.fps * 60 } else { 1800 };
            // The C calls p_VideoEncSetParameter unconditionally here; absent,
            // there is no interval to set and the default cadence stays.
            let rc = match self.libs.video_enc_set_parameter {
                // SAFETY: the vendor reads an int through the pointer for this index.
                Some(set) => unsafe {
                    set(
                        self.enc,
                        index::MAX_KEY_INTERVAL,
                        &mut interval as *mut i32 as *mut c_void,
                    )
                },
                None => -1,
            };
            if rc != 0 {
                warn!("could not set key-frame interval (rc={rc}); keeping the encoder default");
            } else {
                info!("key-frame interval set to {interval} frames (client-driven IDR)");
            }
        }

        info!(
            "encoder ready: {w}x{h} -> {}x{} fmt={:?} ({}) stride={} -> H.264 @ {} fps",
            bcfg.n_dst_width,
            bcfg.n_dst_height,
            self.in_fmt,
            if self.rgb_in { "RGB passthrough" } else { "NV12 via CPU convert" },
            bcfg.n_stride,
            self.fps
        );

        // SPS/PPS is fetched after the first frame is encoded — see
        // fetch_sps_pps() below. The parameter set does not exist until then:
        // querying beforehand returns a pointer with nLength=0.

        self.inbuf = VencInputBuffer::default();
        // SAFETY: the vendor fills inbuf in place; its address is stable for
        // the session because the struct is boxed.
        if unsafe { (self.libs.get_one_alloc_input_buffer)(self.enc, &mut self.inbuf) } != 0 {
            bail!("GetOneAllocInputBuffer failed");
        }
        self.held = true;

        self.frame_ns = 1_000_000_000 / self.fps as i64;
        self.deadline = now_ns();
        Ok(())
    }

    /// Marks the capture dead and returns the failure.
    ///
    /// A failed frame can leave the vendor input buffer either already
    /// submitted (AddOneInputBuffer succeeded, encode did not) or not acquired
    /// at all (GetOneAllocInputBuffer failed, leaving inbuf zeroed and _virY
    /// NULL). Neither state is safe to drive again — the old C main()
    /// sidestepped this by breaking out of the loop and exiting, but a library
    /// that returns an error invites a retry that would encode from a NULL
    /// pointer or double-submit.
    fn fail(&mut self, e: anyhow::Error) -> anyhow::Error {
        let out = anyhow!("{e:#}");
        self.failed = Some(e);
        out
    }

    /// Blocks until the next frame is due, then returns its Annex-B bitstream.
    /// The slice is owned by the capture and is invalidated by the next call.
    ///
    /// A failure from this function is terminal: the capture is dead and every
    /// later call returns the original error.
    // Not `Iterator::next`: the returned `Frame` borrows `self` for as long as
    // it lives, which `Iterator` cannot express.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Frame<'_>> {
        if let Some(e) = &self.failed {
            return Err(anyhow!("{e:#}"));
        }

        // Pace to the frame deadline before capturing, so each frame samples
        // the screen one frame interval after the last.
        let now = now_ns();
        let slack = self.deadline - now;
        if slack > 0 {
            let ts = libc::timespec {
                tv_sec: (slack / 1_000_000_000) as libc::time_t,
                tv_nsec: (slack % 1_000_000_000) as _,
            };
            // SAFETY: sleeps for a duration we own; no remainder is wanted.
            unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
        }
        self.deadline += self.frame_ns;
        // After a stall the deadline can fall arbitrarily far behind. Without
        // this clamp the next calls all return instantly, replaying the backlog
        // as a burst of frames with stale timestamps; drop the missed frames
        // instead.
        if self.deadline < now {
            self.deadline = now + self.frame_ns;
        }

        self.out.clear();

        let read = self
            .fb
            .as_mut()
            .expect("framebuffer open")
            .read_visible(&mut self.fb_buf);
        match read {
            Ok(n) if n != self.frame_bytes => self.short_reads += 1,
            Ok(_) => {}
            // `read_visible` errors only where the C's `n <= 0` did; a short
            // read comes back as Ok and is counted, not fatal.
            Err(e) => {
                self.short_reads += 1;
                return Err(self.fail(e));
            }
        }

        let t0 = now_ns();
        self.convert_into_input_buffer();
        let t1 = now_ns();
        self.convert_ns += t1 - t0;

        self.inbuf.n_pts = self.frames as i64 * (1_000_000 / self.fps as i64);
        self.inbuf.b_is_first_frame = i32::from(self.frames == 0);

        // Moonlight asks for an IDR after packet loss; the vendor parameter is
        // one-shot and applies to the frame encoded next.
        if self.force_idr {
            let mut one: i32 = 1;
            let rc = match self.libs.video_enc_set_parameter {
                // SAFETY: the vendor reads an int through the pointer for this index.
                Some(set) => unsafe {
                    set(
                        self.enc,
                        index::FORCE_KEY_FRAME,
                        &mut one as *mut i32 as *mut c_void,
                    )
                },
                None => -1,
            };
            if rc != 0 {
                warn!("force-IDR request ignored (rc={rc}); the next frame may not be a keyframe");
            }
            self.force_idr = false;
        }

        // SAFETY: inbuf was filled by GetOneAllocInputBuffer and its address is
        // stable; both entry points take it by pointer.
        unsafe { (self.libs.flush_cache_alloc_input_buffer)(self.enc, &mut self.inbuf) };
        // SAFETY: as above.
        if unsafe { (self.libs.add_one_input_buffer)(self.enc, &mut self.inbuf) } != 0 {
            let frames = self.frames;
            return Err(self.fail(anyhow!("AddOneInputBuffer failed at frame {frames}")));
        }
        // SAFETY: vendor entry point taking only the encoder handle.
        if unsafe { (self.libs.video_encode_one_frame)(self.enc) } != 0 {
            let frames = self.frames;
            return Err(self.fail(anyhow!("VideoEncodeOneFrame failed at frame {frames}")));
        }
        self.encode_ns += now_ns() - t1;

        // Parameter sets exist only once a frame has been encoded, so grab
        // them after the first one and emit them ahead of any frame data.
        if !self.sps_pps_fetched {
            self.sps_pps = self.fetch_sps_pps();
            if self.sps_pps.is_empty() {
                warn!("no SPS/PPS - the stream will not decode standalone");
            } else {
                info!("SPS/PPS: {} bytes", self.sps_pps.len());
            }
            self.sps_pps_fetched = true;
        }
        // Frame 0 gets its parameter sets up front; every later keyframe gets
        // them too, but only once the drain below has told us it IS a keyframe -
        // see the prepend after the loop.
        if self.frames == 0 && !self.sps_pps.is_empty() {
            self.out.extend_from_slice(&self.sps_pps);
        }

        // Bytes present before the bitstream drain, so a failure to pull the
        // very first segment can be told apart from the end of a frame's
        // segments.
        let before_drain = self.out.len();

        // SAFETY: vendor entry point taking only the encoder handle.
        while unsafe { (self.libs.valid_bitstream_frame_num)(self.enc) } > 0 {
            let mut g = GuardedOutputBuffer {
                ob: VencOutputBuffer::default(),
                guard: [0; 256],
            };
            // SAFETY: the vendor fills the output buffer in place.
            if unsafe { (self.libs.get_one_bitstream_frame)(self.enc, &mut g.ob) } != 0 {
                // Nothing retrieved at all means there is no frame to hand
                // back; that is a failure, not an early end to the segment
                // list.
                //
                // Caveat for the next reader: a preceding segment that
                // succeeded but carried nTotalSize == 0 would leave out
                // untouched and be indistinguishable from "the loop has not
                // appended yet", so a genuine failure after one could be
                // misreported as end-of-list. Not seen in practice on this
                // encoder, and not worth a speculative fix — but that is the
                // hole if this ever misbehaves.
                if self.out.len() == before_drain {
                    let frames = self.frames;
                    return Err(
                        self.fail(anyhow!("GetOneBitstreamFrame failed at frame {frames}"))
                    );
                }
                break;
            }

            let o = &g.ob;
            // The vendor exposes the frame as up to two segments of a ring
            // buffer (pData0/nSize0 + pData1/nSize1), with nTotalSize the sum.
            // Only trust the split when it adds up; otherwise treat pData0 as
            // one contiguous run of nTotalSize bytes, which is what
            // cedar-probe does and what this build appears to produce.
            if !o.p_data0.is_null() && o.n_size0 != 0 && o.n_size0 + o.n_size1 == o.n_total_size {
                // SAFETY: the vendor reports nSize0 valid bytes at pData0.
                let s0 = unsafe { std::slice::from_raw_parts(o.p_data0, o.n_size0 as usize) };
                bitstream::append_avcc_as_annexb(&mut self.out, s0);
                if !o.p_data1.is_null() && o.n_size1 != 0 {
                    // SAFETY: as above, for the second ring segment.
                    let s1 = unsafe { std::slice::from_raw_parts(o.p_data1, o.n_size1 as usize) };
                    bitstream::append_avcc_as_annexb(&mut self.out, s1);
                }
            } else if !o.p_data0.is_null() && o.n_total_size != 0 {
                // SAFETY: the vendor reports nTotalSize valid bytes at pData0.
                let s = unsafe { std::slice::from_raw_parts(o.p_data0, o.n_total_size as usize) };
                if bitstream::append_avcc_as_annexb(&mut self.out, s) == 0 {
                    // not AVCC — emit verbatim
                    self.out.extend_from_slice(s);
                }
            }

            debug!(
                "frame {}: total={} size0={} size1={}{}",
                self.frames,
                o.n_total_size,
                o.n_size0,
                o.n_size1,
                if o.b_is_key_frame != 0 { " (key)" } else { "" }
            );
            // SAFETY: hands the same buffer back to the vendor exactly once.
            unsafe { (self.libs.free_one_bitstream_frame)(self.enc, &mut g.ob) };
        }

        // Recycle the input buffer for the next frame.
        self.used = VencInputBuffer::default();
        // SAFETY: both entry points fill/consume `used` in place; its address
        // is stable and the spill past it lands in _vendor_guard.
        unsafe {
            if (self.libs.already_used_input_buffer)(self.enc, &mut self.used) == 0 {
                (self.libs.return_one_alloc_input_buffer)(self.enc, &mut self.used);
            }
        }
        self.inbuf = VencInputBuffer::default();
        // SAFETY: the vendor fills inbuf in place.
        if unsafe { (self.libs.get_one_alloc_input_buffer)(self.enc, &mut self.inbuf) } != 0 {
            self.held = false;
            let frames = self.frames;
            return Err(self.fail(anyhow!("GetOneAllocInputBuffer failed at frame {frames}")));
        }

        self.frames += 1;

        // A client-requested IDR arrives from the VE as a bare type-5 NAL with
        // no SPS/PPS ahead of it - only the very first frame carries them. A
        // software decoder reuses the parameter sets it already cached and does
        // not care, which is why this went unnoticed against FFmpeg. A hardware
        // decoder builds its format description from the parameter sets carried
        // with the keyframe: VideoToolbox (every Apple client, including
        // Moonlight on the Apple TV) never starts decoding, sits in "Waiting
        // for IDR frame" and drops the connection. Repeat the sets ahead of
        // every keyframe.
        let keyframe = bitstream::first_slice_is_idr(&self.out);
        if keyframe
            && !self.sps_pps.is_empty()
            && !bitstream::starts_with_parameter_sets(&self.out)
        {
            self.out.splice(0..0, self.sps_pps.iter().copied());
        }

        Ok(Frame {
            data: &self.out,
            is_keyframe: keyframe,
        })
    }

    /// The framebuffer copy into the vendor's ION input buffer, in whichever of
    /// the four shapes the configured input format asks for.
    fn convert_into_input_buffer(&mut self) {
        let vir_y = self.inbuf._vir_y;
        let vir_uv = self.inbuf._vir_uv;
        let (w, h, pitch) = (self.w as usize, self.h as usize, self.pitch as usize);
        let (pad_w, pad_h) = (self.pad_w as usize, self.pad_h as usize);

        if self.rgb_in && self.padded {
            // Row-by-row into the centre of the padded surface. The bars are
            // blacked once per buffer below, not per frame - with nBufferNum = 1
            // the VE hands back the same allocation every time, so anything
            // outside the image area stays as we left it.
            let row_bytes = w * 4;
            let dst_pitch = pad_w * 4;
            if !self.bars_cleared {
                // SAFETY: the vendor allocated pad_w*pad_h*4 bytes at _vir_y.
                unsafe { std::ptr::write_bytes(vir_y, 0, dst_pitch * pad_h) };
                self.bars_cleared = true;
            }
            let dst = unsafe {
                // SAFETY: the image area sits wholly inside the allocation.
                vir_y.add(self.pad_y as usize * dst_pitch + self.pad_x as usize * 4)
            };
            for y in 0..h {
                // SAFETY: source row is inside fb_buf, destination row inside
                // the padded allocation.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.fb_buf.as_ptr().add(y * pitch),
                        dst.add(y * dst_pitch),
                        row_bytes,
                    );
                }
            }
        } else if self.rgb_in {
            // No conversion: the VE ingests the framebuffer format as-is.
            // Still one copy, because the encoder reads from ION memory.
            // SAFETY: the allocation is pad_w*pad_h*4 >= frame_bytes here,
            // since padding is off and the surface is the panel itself.
            unsafe {
                std::ptr::copy_nonoverlapping(self.fb_buf.as_ptr(), vir_y, self.frame_bytes)
            };
        } else {
            // SAFETY: AllocInputBuffer reserved pad_w*pad_h luma and
            // pad_w*pad_h/2 chroma bytes at these pointers.
            let (dy, duv) = unsafe {
                (
                    std::slice::from_raw_parts_mut(vir_y, pad_w * pad_h),
                    std::slice::from_raw_parts_mut(vir_uv, pad_w * pad_h / 2),
                )
            };
            if self.bpp == 32 {
                convert::bgra_to_nv12(&self.fb_buf, pitch, w, h, dy, duv);
            } else {
                convert::rgb565_to_nv12(&self.fb_buf, pitch, w, h, dy, duv);
            }
        }
    }

    /// Fetch the H.264 parameter sets.
    ///
    /// The index is 0x101: vencoder.h puts the H.264 parameters in their own
    /// block at 0x100, so VENC_IndexParamH264SPSPPS = 0x100 + 1. cedar-probe
    /// used 16, which is an unrelated parameter — that is why it read back a
    /// frame-sized nLength and concluded the data lived in unreachable VE SRAM.
    ///
    /// Must be called *after* the first frame is encoded; before that the
    /// library returns a pointer with nLength=0. It also writes more than the
    /// two documented fields, hence the padding in VencHeaderData.
    ///
    /// Returns the Annex-B parameter sets, or empty if unavailable.
    fn fetch_sps_pps(&self) -> Vec<u8> {
        let Some(get) = self.libs.video_enc_get_parameter else {
            return Vec::new();
        };

        let mut hdr = VencHeaderData::default();
        // SAFETY: the vendor writes a VencHeaderData (plus slack, absorbed by
        // its _tail) through the pointer.
        let r = unsafe {
            get(
                self.enc,
                index::H264_SPS_PPS,
                &mut hdr as *mut VencHeaderData as *mut c_void,
            )
        };
        debug!(
            "VideoEncGetParameter(0x101) rc={r} pBuffer={:p} nLength={}",
            hdr.p_buffer, hdr.n_length
        );
        if r != 0
            || hdr.p_buffer.is_null()
            || hdr.n_length == 0
            || hdr.n_length as usize > SPS_PPS_CAP
        {
            info!("SPS/PPS unavailable (rc={r} len={})", hdr.n_length);
            return Vec::new();
        }

        // pBuffer may be a VE bus address rather than a CPU pointer.
        let mut p = hdr.p_buffer;
        // SAFETY: memops is the vendor's op table, live while mem_open.
        if let Some(vir) = unsafe { (*self.memops).ve_get_viraddr } {
            // SAFETY: vendor translation of its own address.
            let m = unsafe { vir(hdr.p_buffer as *mut c_void) } as *mut u8;
            if !m.is_null() {
                p = m;
            }
        }
        if p.is_null() {
            return Vec::new();
        }

        // SAFETY: the vendor reports nLength readable bytes at this address.
        let raw = unsafe { std::slice::from_raw_parts(p, hdr.n_length as usize) };

        // The library hands back an AVCDecoderConfigurationRecord (avcC), not
        // Annex-B; convert each parameter set into a start-code-prefixed NAL.
        // Anything else is passed through verbatim, as the C did.
        if raw.len() > 7 && raw[0] == 0x01 {
            bitstream::avcc_record_to_annexb(raw)
        } else {
            raw.to_vec()
        }
    }

    /// Ask the VE for an IDR on the next encoded frame. Moonlight sends this
    /// after packet loss.
    pub fn request_idr(&mut self) {
        self.force_idr = true;
    }

    /// The Annex-B SPS/PPS, empty until the first frame has been encoded.
    pub fn param_sets(&self) -> &[u8] {
        &self.sps_pps
    }

    pub fn stats(&self) -> Stats {
        Stats {
            convert_ns: self.convert_ns,
            encode_ns: self.encode_ns,
            short_reads: self.short_reads,
        }
    }

    /// The 0xAA sentinel region after the vendor-written structs. Exposed for
    /// tests/vendor_overspill.rs only.
    #[doc(hidden)]
    pub fn vendor_guard(&self) -> &[u8; 4096] {
        &self._vendor_guard
    }
}

impl Drop for Capture {
    /// Documented teardown order. Reached on every exit path, so the VE and its
    /// ION allocations are always released.
    fn drop(&mut self) {
        if !self.enc.is_null() {
            // SAFETY: every call below takes the encoder handle this object
            // created and has not yet destroyed.
            unsafe {
                if self.held {
                    self.used = VencInputBuffer::default();
                    if (self.libs.already_used_input_buffer)(self.enc, &mut self.used) == 0 {
                        (self.libs.return_one_alloc_input_buffer)(self.enc, &mut self.used);
                    }
                }
                if self.buffers_alloced {
                    (self.libs.release_alloc_input_buffer)(self.enc);
                }
                if self.enc_inited {
                    (self.libs.video_enc_uninit)(self.enc);
                }
                (self.libs.video_enc_destroy)(self.enc);
            }
        }
        if self.mem_open && !self.memops.is_null() {
            // SAFETY: the op table is the vendor's own static, live until close.
            if let Some(close) = unsafe { (*self.memops).close } {
                // SAFETY: vendor entry point, no arguments.
                unsafe { close() };
            }
        }
        // Release the single capture slot after teardown completes.
        CAPTURE_OPEN.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The single highest-risk property in this port, checked off-device.
    ///
    /// `inbuf` and `used` must be adjacent, in that order, last, with only
    /// `_vendor_guard` after them - the vendor libraries write up to +24 bytes
    /// past the end of `used` every frame, and in the C the field that sat
    /// there was nulled and segfaulted the first frame. Rust's default layout
    /// reorders fields freely, so this is what `#[repr(C)]` is buying.
    #[test]
    fn the_vendor_written_structs_are_adjacent_and_last() {
        assert_eq!(
            offset_of!(Capture, used),
            offset_of!(Capture, inbuf) + size_of::<VencInputBuffer>(),
            "inbuf and used must be adjacent, in that order"
        );
        assert_eq!(
            offset_of!(Capture, _vendor_guard),
            offset_of!(Capture, used) + size_of::<VencInputBuffer>(),
            "the guard must start exactly where `used` ends"
        );
        assert_eq!(
            size_of::<Capture>(),
            offset_of!(Capture, _vendor_guard) + 4096,
            "nothing may sit after the guard"
        );
    }

    /// A zero-filled guard cannot detect the spill: most of what the vendor
    /// writes past `used` is zeroes, which is exactly the blind spot that hid
    /// the corruption in the C for months.
    #[test]
    fn the_guard_fill_is_not_zero() {
        assert_ne!(GUARD_FILL, 0);
        assert_eq!(GUARD_FILL, 0xAA, "the overspill test measures against 0xAA");
    }
}
