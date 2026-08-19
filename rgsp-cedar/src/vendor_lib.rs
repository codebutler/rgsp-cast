//! Loads the CedarC vendor libraries at runtime. Nothing proprietary is
//! linked; launch.sh sets LD_LIBRARY_PATH to the pak's lib/h700.
//!
//! Transcribed from `src/rgsp-cast.c:176-278` (the typedefs, the `p_*`
//! statics, `LOADSYM`, and `load_libs()`).

use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use libloading::{Library, Symbol};

use crate::vendor_abi::*;

/// Opaque vendor handle. The C only ever forward-declares
/// `typedef struct VideoEncoder VideoEncoder;` — nothing in this tree, or in
/// the vendor library itself as far as the C ever needed, sees its layout.
/// The pointer is created by `VideoEncCreate` and passed back into every
/// other entry point unexamined.
#[repr(C)]
pub struct VideoEncoder {
    _opaque: [u8; 0],
}

pub struct VendorLibs {
    // Kept alive for the process lifetime. dlclose is deliberately never
    // called: the C dropped it and made loading idempotent so that a capture
    // reopen works at all, and the daemon does one open/close cycle per
    // Moonlight session.
    _libve: Library,
    _libmem: Library,
    _libvenc: Library,

    pub video_enc_create: unsafe extern "C" fn(VencCodecType) -> *mut VideoEncoder,
    pub video_enc_init: unsafe extern "C" fn(*mut VideoEncoder, *mut VencBaseConfig) -> i32,
    pub video_enc_uninit: unsafe extern "C" fn(*mut VideoEncoder),
    pub video_enc_destroy: unsafe extern "C" fn(*mut VideoEncoder),
    pub alloc_input_buffer:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencAllocateBufferParam) -> i32,
    pub get_one_alloc_input_buffer:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencInputBuffer) -> i32,
    pub flush_cache_alloc_input_buffer:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencInputBuffer) -> i32,
    pub return_one_alloc_input_buffer:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencInputBuffer) -> i32,
    pub release_alloc_input_buffer: unsafe extern "C" fn(*mut VideoEncoder) -> i32,
    pub add_one_input_buffer: unsafe extern "C" fn(*mut VideoEncoder, *mut VencInputBuffer) -> i32,
    pub video_encode_one_frame: unsafe extern "C" fn(*mut VideoEncoder) -> i32,
    pub valid_bitstream_frame_num: unsafe extern "C" fn(*mut VideoEncoder) -> i32,
    pub get_one_bitstream_frame:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencOutputBuffer) -> i32,
    pub free_one_bitstream_frame:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencOutputBuffer) -> i32,
    pub already_used_input_buffer:
        unsafe extern "C" fn(*mut VideoEncoder, *mut VencInputBuffer) -> i32,

    /// Optional in the C; if absent there is no bitrate control and no
    /// forced IDR, which means Moonlight cannot recover from packet loss.
    pub video_enc_get_parameter:
        Option<unsafe extern "C" fn(*mut VideoEncoder, i32, *mut std::ffi::c_void) -> i32>,
    pub video_enc_set_parameter:
        Option<unsafe extern "C" fn(*mut VideoEncoder, i32, *mut std::ffi::c_void) -> i32>,

    pub get_ve_ops_s: unsafe extern "C" fn(i32) -> *mut std::ffi::c_void,
    pub mem_adapter_get_ops_s: unsafe extern "C" fn() -> *mut ScMemOpsS,
}

// The vendor entry points are called from one thread at a time (Capture is
// single-instance and owns exclusive access to the encoder handle); the
// Library handles themselves are immutable after load.
unsafe impl Send for VendorLibs {}
unsafe impl Sync for VendorLibs {}

static LIBS: OnceLock<VendorLibs> = OnceLock::new();

impl VendorLibs {
    /// Loads the three vendor libraries and resolves every symbol, or
    /// returns the already-loaded singleton. Matches the C's
    /// `if (g_libvenc) return 0;` idempotence: a capture reopen calls this
    /// again and must not dlopen a second time.
    pub fn load() -> Result<&'static VendorLibs> {
        if let Some(l) = LIBS.get() {
            return Ok(l);
        }
        let libs = unsafe { Self::load_uncached() }?;
        Ok(LIBS.get_or_init(|| libs))
    }

    unsafe fn load_uncached() -> Result<VendorLibs> {
        // RTLD_GLOBAL matters: libvencoder resolves symbols out of libVE and
        // libMemAdapter at load time, and libloading's default is
        // RTLD_LOCAL, which would break that resolution.
        let flags = libloading::os::unix::RTLD_LAZY | libloading::os::unix::RTLD_GLOBAL;
        let open = |name: &str| -> Result<Library> {
            libloading::os::unix::Library::open(Some(name), flags)
                .map(Library::from)
                .with_context(|| format!("dlopen({name})"))
        };
        let libve = open("libVE.so")?;
        let libmem = open("libMemAdapter.so")?;
        let libvenc = open("libvencoder.so")?;

        Ok(VendorLibs {
            video_enc_create: required(&libvenc, b"VideoEncCreate\0")?,
            video_enc_init: required(&libvenc, b"VideoEncInit\0")?,
            video_enc_uninit: required(&libvenc, b"VideoEncUnInit\0")?,
            video_enc_destroy: required(&libvenc, b"VideoEncDestroy\0")?,
            alloc_input_buffer: required(&libvenc, b"AllocInputBuffer\0")?,
            get_one_alloc_input_buffer: required(&libvenc, b"GetOneAllocInputBuffer\0")?,
            flush_cache_alloc_input_buffer: required(&libvenc, b"FlushCacheAllocInputBuffer\0")?,
            return_one_alloc_input_buffer: required(&libvenc, b"ReturnOneAllocInputBuffer\0")?,
            release_alloc_input_buffer: required(&libvenc, b"ReleaseAllocInputBuffer\0")?,
            add_one_input_buffer: required(&libvenc, b"AddOneInputBuffer\0")?,
            video_encode_one_frame: required(&libvenc, b"VideoEncodeOneFrame\0")?,
            valid_bitstream_frame_num: required(&libvenc, b"ValidBitstreamFrameNum\0")?,
            get_one_bitstream_frame: required(&libvenc, b"GetOneBitstreamFrame\0")?,
            // Capital S in "Stream", unlike every neighbouring symbol - the
            // real vendor spelling, not a typo to fix.
            free_one_bitstream_frame: required(&libvenc, b"FreeOneBitStreamFrame\0")?,
            already_used_input_buffer: required(&libvenc, b"AlreadyUsedInputBuffer\0")?,

            // Optional in the C, and kept optional here so a vendor lib
            // without them still loads. Absent, there is no bitrate control
            // and no forced IDR - see the field docs.
            video_enc_get_parameter: optional(&libvenc, b"VideoEncGetParameter\0"),
            video_enc_set_parameter: optional(&libvenc, b"VideoEncSetParameter\0"),

            get_ve_ops_s: required(&libve, b"GetVeOpsS\0")?,
            mem_adapter_get_ops_s: required(&libmem, b"MemAdapterGetOpsS\0")?,

            _libve: libve,
            _libmem: libmem,
            _libvenc: libvenc,
        })
    }
}

/// Look up a symbol and copy the raw function pointer out of the `Symbol`
/// borrow, so `VendorLibs` holds plain pointers rather than borrowing its own
/// `Library` fields (which would make it self-referential and
/// unconstructable).
///
/// The pointers stay valid because the `Library` handles live in the same
/// struct and that struct is leaked into a `OnceLock` for the process
/// lifetime — nothing ever calls `dlclose`. Dropping a `VendorLibs` would
/// dangle every pointer in it; there is deliberately no path that does.
unsafe fn required<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
    let sym: Symbol<T> = lib.get(name).map_err(|_| {
        anyhow!(
            "missing symbol {}",
            String::from_utf8_lossy(&name[..name.len() - 1])
        )
    })?;
    Ok(*sym)
}

/// As `required`, but absence is not an error.
unsafe fn optional<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|sym| *sym)
}
