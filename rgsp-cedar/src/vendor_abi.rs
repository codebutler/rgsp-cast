//! The CedarC vendor ABI, hand-written.
//!
//! There is no header for this: the layouts are reverse-engineered, which is
//! why every struct carries trailing padding a real header would not have.
//! The vendor libraries write past the field sets we know about; the padding
//! absorbs it. Do not generate this with bindgen against libcedarc's
//! vencoder.h - that source is GPLv3 and nothing from it enters this tree.
//!
//! Transcribed from `src/rgsp-cast.c:103-203`, field order and `_tail`
//! padding sizes preserved exactly.

use std::ffi::{c_char, c_int, c_uint, c_void};

/// Generic encoder parameters start at 0; the H.264-specific block starts at
/// 0x100. `H264_SPS_PPS` is 0x100 + 1, NOT 16 — cedar-probe used 16, which is
/// an unrelated parameter, and read back a frame-sized nLength from it. That
/// is how it concluded the parameter sets lived in unreachable VE SRAM and
/// resorted to hardcoding them per resolution.
pub mod index {
    use std::ffi::c_int;
    pub const BITRATE: c_int = 0x0;
    pub const MAX_KEY_INTERVAL: c_int = 0x2;
    pub const FORCE_KEY_FRAME: c_int = 0x6;
    pub const H264_SPS_PPS: c_int = 0x101;
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VencPixelFmt {
    /// NV12 — needs CPU conversion from the framebuffer.
    #[default]
    Yuv420Sp = 0,
    /// Allwinner names formats by 32-bit word order, not byte order. For a
    /// framebuffer whose bytes are B,G,R,A this is the correct constant —
    /// *not* Bgra (15). Verified against the CPU conversion path at 42.2 dB
    /// PSNR on identical screen content.
    Argb = 12,
    Rgba = 13,
    Abgr = 14,
    /// matches /dev/fb0 directly, if supported
    Bgra = 15,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VencCodecType {
    #[default]
    H264 = 0,
}

/// The full 20-entry function-pointer table the vendor libraries hand back
/// from `MemAdapterGetOpsS()`. Every entry is `Option<...>` so the struct can
/// still derive `Default` — a zeroed table is what the vendor libraries
/// themselves return before `open()`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ScMemOpsS {
    pub open: Option<unsafe extern "C" fn() -> c_int>,
    pub open2: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    pub close: Option<unsafe extern "C" fn()>,
    pub total_size: Option<unsafe extern "C" fn() -> c_int>,
    pub palloc: Option<unsafe extern "C" fn(c_int, *mut c_void, *mut c_void) -> *mut c_void>,
    pub palloc_no_cache:
        Option<unsafe extern "C" fn(c_int, *mut c_void, *mut c_void) -> *mut c_void>,
    pub pfree: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
    pub flush_cache: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
    pub ve_get_phyaddr: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    pub ve_get_viraddr: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    pub cpu_get_phyaddr: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    pub cpu_get_viraddr: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    pub mem_set: Option<unsafe extern "C" fn(*mut c_void, c_int, usize) -> c_int>,
    pub mem_cpy: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int>,
    pub mem_read: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int>,
    pub mem_write: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int>,
    pub setup: Option<unsafe extern "C" fn() -> c_int>,
    pub shutdown: Option<unsafe extern "C" fn() -> c_int>,
    pub get_ve_addr_offset: Option<unsafe extern "C" fn() -> c_uint>,
    pub get_debug_info: Option<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VencBaseConfig {
    pub b_enc_h264_nalu: u8,
    pub n_input_width: u32,
    pub n_input_height: u32,
    pub n_dst_width: u32,
    pub n_dst_height: u32,
    pub n_stride: u32,
    pub e_input_format: VencPixelFmt,
    pub memops: *mut c_void,
    pub ve_ops_s: *mut c_void,
    pub p_ve_ops_self: *mut c_void,
    pub b_only_wb_flag: u8,
    pub b_lbc_lossy_com_en_flag2x: u8,
    pub b_lbc_lossy_com_en_flag2_5x: u8,
    pub b_is_vbv_no_cache: u8,
    pub _tail: [u8; 128],
}

// `[u8; N]` only derives `Default` for N <= 32 in stable std; these vendor
// tails are all longer, so the zero bit pattern (a valid default for every
// field here: integers, an enum whose 0 variant is real, and null pointers)
// is filled in by hand instead.
impl Default for VencBaseConfig {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Vendor layout: pAddrPhyC holds the Y physical address, and three extra
/// pointers follow that the open-source CedarC headers do not declare.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VencInputBuffer {
    pub p_addr_vir_y: *mut u8,
    pub p_addr_vir_c: *mut u8,
    pub p_addr_phy_y: *mut u8,
    /// Y physical (VE DMA)
    pub p_addr_phy_c: *mut u8,
    /// UV physical
    pub _phy_uv: *mut u8,
    /// Y  CPU virtual — write NV12 luma here
    pub _vir_y: *mut u8,
    /// UV CPU virtual — write NV12 chroma here
    pub _vir_uv: *mut u8,
    pub n_id: i32,
    pub _pad: i32,
    pub n_pts: i64,
    pub n_duration: i64,
    pub b_is_first_frame: i32,
    pub b_last_frame: i32,
    pub b_enable_corp: i32,
    pub n_share_buf_fd: u32,
    pub _tail: [u8; 256],
}

impl Default for VencInputBuffer {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VencOutputBuffer {
    pub _flags: i32,
    pub _pad0: [i32; 3],
    pub b_is_key_frame: i32,
    pub n_total_size: u32,
    pub n_id: i32,
    pub _align: i32,
    pub p_data0: *mut u8,
    pub p_data1: *mut u8,
    pub n_size0: u32,
    pub n_size1: u32,
    pub n_pts: i64,
    pub _tail: [u8; 256],
}

impl Default for VencOutputBuffer {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VencAllocateBufferParam {
    pub n_buffer_num: u32,
    pub n_size_y: u32,
    pub n_size_c: u32,
    pub _tail: [u8; 64],
}

impl Default for VencAllocateBufferParam {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// The struct that caused the stack smash. Real fields are pBuffer + nLength;
/// the padding absorbs whatever else the vendor writes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VencHeaderData {
    pub p_buffer: *mut u8,
    pub n_length: u32,
    pub _tail: [u8; 496],
}

impl Default for VencHeaderData {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}
