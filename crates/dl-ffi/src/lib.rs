#![doc = include_str!("../README.md")]
// The only crate in the workspace that needs `unsafe`: every exported function reads
// caller-owned buffers. Each block states its precondition in a `SAFETY:` comment.
#![allow(unsafe_code)]
// A shared library needs a global allocator and a panic handler, which `std` provides.
// Panics cannot cross the boundary: `extern "C"` functions abort on panic, and the
// libraries called here do not panic on input.

extern crate alloc;

mod abi;
mod vn;

pub use abi::*;
pub use vn::*;

/// Version of the C ABI described in `include/dl_ffi.h`. It changes only when an
/// existing function changes its signature or meaning; new functions do not change it.
pub const DL_FFI_ABI_VERSION: u32 = 1;

/// Returns [`DL_FFI_ABI_VERSION`]. Hosts check it once at startup and refuse to run
/// against a library built for another ABI.
#[unsafe(no_mangle)]
pub extern "C" fn dl_ffi_abi_version() -> u32 {
    DL_FFI_ABI_VERSION
}
