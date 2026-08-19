//! `rgsp-cedar` — the Rust replacement for `librgspcast`: hardware H.264
//! capture of the RG SP framebuffer via the Allwinner Cedar VE.
//!
//! This crate holds everything that touches the vendor CedarC libraries
//! (dlopen'd at runtime, never linked) plus the bitstream logic layered on
//! top of them.

pub mod bitstream;
pub mod vendor_abi;
