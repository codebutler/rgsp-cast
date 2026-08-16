use anyhow::{anyhow, Result};
use std::ffi::{c_char, c_int, c_void, CStr};

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

pub struct Capture {
    handle: *mut RgspCapture,
}

// The handle is only ever touched from the thread that owns the Capture.
unsafe impl Send for Capture {}

impl Capture {
    pub fn open(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Capture> {
        let handle = unsafe {
            rgsp_capture_open(width as c_int, height as c_int, fps as c_int, bitrate as c_int)
        };
        if handle.is_null() {
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
    pub fn next(&mut self) -> Result<Frame<'_>> {
        let mut data: *const u8 = std::ptr::null();
        let mut len: usize = 0;
        let mut key: c_int = 0;
        let rc = unsafe { rgsp_capture_next(self.handle, &mut data, &mut len, &mut key) };
        if rc != 0 {
            return Err(anyhow!("rgsp_capture_next: {}", last_error()));
        }
        Ok(Frame {
            data: unsafe { std::slice::from_raw_parts(data, len) },
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
    }
}

// Silences an unused-import warning on the c_void import in some toolchains.
const _: Option<*const c_void> = None;
