//! `/dev/fb0`, read-only.
//!
//! Two optimisations were measured here and both are worse — do not retry
//! them without new information:
//!
//!  - Zero-copy (encode straight from fb physical memory, `smem_start`):
//!    produces a corrupt bitstream. The VE reaches memory through an IOMMU
//!    that only maps ION allocations, so a raw framebuffer address is
//!    meaningless to it. Would need dmabuf export, which this fbdev driver
//!    does not provide.
//!  - mmap the framebuffer and copy from it, saving one copy: 19.90 ms per
//!    frame versus 1.44 ms for pread. Framebuffer mappings are uncached, so
//!    CPU reads go to DRAM one access at a time; pread's kernel-side bulk
//!    copy is dramatically faster.
//!
//! pread into a heap buffer, then memcpy into ION, is the fast path.
//!
//! Transcribed from `src/rgsp-cast.c:640-660` (open + ioctls) and
//! `src/rgsp-cast.c:869-882` (per-frame yoffset + pread).

use std::fs::{File, OpenOptions};
use std::mem::MaybeUninit;
use std::os::unix::io::AsRawFd;

use anyhow::{anyhow, bail, Result};

/// `struct fb_bitfield` from `<linux/fb.h>` — a channel's bit position within
/// a pixel. We never read these fields, but they sit between others the
/// kernel does fill by offset, so the layout must still be exact.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// `struct fb_var_screeninfo` from `<linux/fb.h>`, kernel UAPI (GPL-with-
/// syscall-exception), transcribed by hand — not bindgen'd, not copied from
/// the header. Field order and widths must match exactly: the kernel fills
/// this by offset, and a misplaced field silently yields garbage geometry.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,

    bits_per_pixel: u32,
    grayscale: u32,

    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,

    nonstd: u32,

    activate: u32,

    height: u32,
    width: u32,

    accel_flags: u32,

    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

/// `struct fb_fix_screeninfo` from `<linux/fb.h>`. `smem_start`/`mmio_start`
/// are `unsigned long` (pointer-width) in the kernel header; on the arm64
/// target that's 64 bits, so `u64` here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: u64,
    smem_len: u32,
    fb_type: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: u64,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

/// The geometry `Framebuffer::open` read from the panel, exposed for callers
/// that need it (allocation sizing, logging, matching against a requested
/// destination size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FbGeometry {
    pub w: u32,
    pub h: u32,
    pub bpp: u32,
    pub pitch: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub smem_start: u64,
    pub smem_len: u32,
}

/// The pure half of the open path: rejects an unsupported pixel depth or a
/// requested size that disagrees with the panel. Split out from `open()` so
/// it can be unit-tested without a device.
///
/// `req_w`/`req_h` of 0 mean "whatever the panel is" — no size was requested,
/// so there is nothing to disagree with.
pub fn validate_geometry(fb_w: u32, fb_h: u32, bpp: u32, req_w: u32, req_h: u32) -> Result<()> {
    if bpp != 32 && bpp != 16 {
        bail!("unsupported bpp {bpp}");
    }
    // The encoder path has only ever run at the panel's native geometry, and
    // asking the VE to scale the *input* is an untested path. Callers that
    // pass a size say what they expect; disagreeing with the panel is an
    // error, not a resize.
    if (req_w != 0 && req_w != fb_w) || (req_h != 0 && req_h != fb_h) {
        bail!(
            "requested {req_w}x{req_h} but the framebuffer is {fb_w}x{fb_h}; \
             scaling is not supported"
        );
    }
    Ok(())
}

/// An open `/dev/fb0`, read-only.
pub struct Framebuffer {
    file: File,
    geometry: FbGeometry,
}

impl Framebuffer {
    pub fn open() -> Result<Framebuffer> {
        let file = OpenOptions::new()
            .read(true)
            .open("/dev/fb0")
            .map_err(|e| anyhow!("open(/dev/fb0): {e}"))?;
        let fd = file.as_raw_fd();

        let mut vinfo = MaybeUninit::<FbVarScreeninfo>::zeroed();
        let mut finfo = MaybeUninit::<FbFixScreeninfo>::zeroed();
        // SAFETY: FBIOGET_VSCREENINFO/FBIOGET_FSCREENINFO fill exactly
        // sizeof(FbVarScreeninfo)/sizeof(FbFixScreeninfo) bytes at the
        // pointer given, and both structs are repr(C) with kernel-matching
        // layout.
        let (rv, rf) = unsafe {
            (
                libc::ioctl(fd, FBIOGET_VSCREENINFO, vinfo.as_mut_ptr()),
                libc::ioctl(fd, FBIOGET_FSCREENINFO, finfo.as_mut_ptr()),
            )
        };
        if rv < 0 || rf < 0 {
            bail!(
                "FBIOGET_*SCREENINFO: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: both ioctls returned 0, so the kernel filled the structs.
        let vinfo = unsafe { vinfo.assume_init() };
        let finfo = unsafe { finfo.assume_init() };

        let w = vinfo.xres;
        let h = vinfo.yres;
        let bpp = vinfo.bits_per_pixel;
        let pitch = finfo.line_length;

        // bpp is a device property, not something a caller asks for, so it's
        // checked here unconditionally; a requested-size mismatch is the
        // caller's concern (validate_geometry with the caller's req_w/req_h).
        validate_geometry(w, h, bpp, 0, 0)?;

        Ok(Framebuffer {
            file,
            geometry: FbGeometry {
                w,
                h,
                bpp,
                pitch,
                xres_virtual: vinfo.xres_virtual,
                yres_virtual: vinfo.yres_virtual,
                smem_start: finfo.smem_start,
                smem_len: finfo.smem_len,
            },
        })
    }

    pub fn geometry(&self) -> FbGeometry {
        self.geometry
    }

    /// Capture the *visible* buffer: with double buffering, yoffset tells us
    /// which half of the virtual framebuffer is currently on screen. Reading
    /// offset 0 unconditionally - as the reference implementation does - can
    /// capture the buffer being drawn into rather than the one displayed.
    ///
    /// Returns the byte count read. A short read is not an error — the
    /// caller counts them and continues — but `n <= 0` is.
    pub fn read_visible(&mut self, buf: &mut [u8]) -> Result<usize> {
        let fd = self.file.as_raw_fd();

        let mut vinfo = MaybeUninit::<FbVarScreeninfo>::zeroed();
        // SAFETY: same as in open() — fills exactly sizeof(FbVarScreeninfo)
        // bytes. A failed re-read falls back to yoffset 0, matching the C:
        // `if (ioctl(...) == 0) yoff = vinfo.yoffset;`.
        let yoff = unsafe {
            if libc::ioctl(fd, FBIOGET_VSCREENINFO, vinfo.as_mut_ptr()) == 0 {
                vinfo.assume_init().yoffset
            } else {
                0
            }
        };
        let fb_off = yoff as i64 * self.geometry.pitch as i64;

        // SAFETY: pread reads at most buf.len() bytes into buf, which is
        // exactly the buffer libc is given.
        let n = unsafe {
            libc::pread(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                fb_off,
            )
        };
        if n <= 0 {
            bail!(
                "pread(/dev/fb0): {}",
                if n < 0 {
                    std::io::Error::last_os_error().to_string()
                } else {
                    "end of file".to_string()
                }
            );
        }
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_unsupported_bit_depth() {
        assert!(validate_geometry(720, 480, 24, 0, 0).is_err());
    }

    #[test]
    fn accepts_32_and_16_bpp() {
        assert!(validate_geometry(720, 480, 32, 0, 0).is_ok());
        assert!(validate_geometry(720, 480, 16, 0, 0).is_ok());
    }

    #[test]
    fn rejects_a_requested_size_that_is_not_the_panel() {
        // The encoder path has only ever run at the panel's native geometry,
        // and asking the VE to scale the *input* is an untested path. Callers
        // that pass a size say what they expect; disagreeing with the panel is
        // an error, not a resize.
        let e = validate_geometry(720, 480, 32, 1280, 720).unwrap_err();
        assert!(e.to_string().contains("scaling is not supported"));
    }

    #[test]
    fn zero_means_whatever_the_panel_is() {
        assert!(validate_geometry(720, 480, 32, 0, 0).is_ok());
        assert!(validate_geometry(720, 480, 32, 720, 480).is_ok());
    }

    #[test]
    fn opens_and_reads_the_real_device_if_present() {
        // Device-gated: the RG SP is the only place /dev/fb0 exists. Skips in
        // the build container and on any dev machine, which is expected.
        if !std::path::Path::new("/dev/fb0").exists() {
            return;
        }
        let mut fb = Framebuffer::open().expect("open /dev/fb0");
        let geo = fb.geometry();
        let mut buf = vec![0u8; (geo.pitch as usize) * (geo.h as usize)];
        let n = fb.read_visible(&mut buf).expect("read_visible");
        assert!(n > 0);
    }
}
