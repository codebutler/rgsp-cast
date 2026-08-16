use anyhow::{anyhow, Result};
use std::ffi::{c_char, c_int, CStr};
use std::sync::atomic::{AtomicBool, Ordering};

#[repr(C)]
struct RgspCapture {
    _private: [u8; 0],
}

extern "C" {
    fn rgsp_capture_open(width: c_int, height: c_int, fps: c_int, bitrate: c_int)
        -> *mut RgspCapture;
    fn rgsp_capture_next(
        c: *mut RgspCapture,
        data: *mut *const u8,
        len: *mut usize,
        is_keyframe: *mut c_int,
    ) -> c_int;
    fn rgsp_capture_request_idr(c: *mut RgspCapture);
    fn rgsp_capture_close(c: *mut RgspCapture);
    fn rgsp_capture_last_error() -> *const c_char;
}

fn last_error() -> String {
    unsafe {
        let p = rgsp_capture_last_error();
        if p.is_null() {
            "unknown error".into()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

pub struct Frame<'a> {
    pub data: &'a [u8],
    pub is_keyframe: bool,
}

/// At most one Capture may exist per process.
///
/// The C library keeps process-global state: a static error buffer written by
/// every failure path, and dlopen'd vendor handles. Two Captures on different
/// threads would race on the error string. The hardware agrees: there is one
/// Cedar video engine and one framebuffer, so a second capture was never
/// meaningful. Enforcing it here makes `Send` sound by construction rather
/// than by convention: a Capture can be moved to another thread, and there is
/// never a second one to race with.
static CAPTURE_OPEN: AtomicBool = AtomicBool::new(false);

pub struct Capture {
    handle: *mut RgspCapture,
}

/// `Capture` can be sent to other threads because the single-instance guard
/// (`CAPTURE_OPEN`) prevents concurrent access to process-global C state.
unsafe impl Send for Capture {}

impl Capture {
    pub fn open(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Capture> {
        // Claim the single capture slot.
        if CAPTURE_OPEN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(anyhow!("a capture is already open"));
        }

        let handle = unsafe {
            rgsp_capture_open(width as c_int, height as c_int, fps as c_int, bitrate as c_int)
        };
        if handle.is_null() {
            // C open failed; release the slot before returning the error.
            CAPTURE_OPEN.store(false, Ordering::SeqCst);
            return Err(anyhow!("rgsp_capture_open: {}", last_error()));
        }
        Ok(Capture { handle })
    }

    /// Blocks until the next frame is due, then returns its Annex-B bitstream.
    /// The slice is owned by the capture and is invalidated by the next call.
    ///
    /// A failure from this function is terminal. The capture object is dead and
    /// every later call returns an Err with the original error. Do not retry —
    /// the capture can only be safely dropped after a failure. A failed frame can
    /// leave the encoder's input buffer in an inconsistent state.
    // Not `Iterator::next`: the returned `Frame` borrows `self` for as long as
    // it lives, which `Iterator` cannot express.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Frame<'_>> {
        let mut data: *const u8 = std::ptr::null();
        let mut len: usize = 0;
        let mut key: c_int = 0;
        let rc = unsafe { rgsp_capture_next(self.handle, &mut data, &mut len, &mut key) };
        if rc != 0 {
            return Err(anyhow!("rgsp_capture_next: {}", last_error()));
        }
        // Guard against null pointer even though the C API should not return it.
        let data = if data.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data, len) }
        };
        Ok(Frame {
            data,
            is_keyframe: key != 0,
        })
    }

    pub fn request_idr(&self) {
        unsafe { rgsp_capture_request_idr(self.handle) }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { rgsp_capture_close(self.handle) }
        // Release the single capture slot after close completes.
        CAPTURE_OPEN.store(false, Ordering::SeqCst);
    }
}
