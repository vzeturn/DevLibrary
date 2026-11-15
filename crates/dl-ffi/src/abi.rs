//! Status codes and the buffer conventions shared by every function.
//!
//! - Text in and out is UTF-8, passed as pointer + length (no NUL terminator). A null
//!   pointer is allowed only with length 0.
//! - Output goes into a caller-owned buffer `out[..cap]`. `*out_len` always receives the
//!   number of bytes the complete result needs. If it does not fit, nothing is written and
//!   [`DL_ERR_BUFFER_TOO_SMALL`] is returned: call again with a buffer of `*out_len` bytes.
//! - On [`DL_ERR_INVALID_INPUT`] the output is a UTF-8 message describing the problem,
//!   under the same rules.
//! - No function allocates memory that the caller must free, keeps state between calls,
//!   or panics across the boundary.

/// Success.
pub const DL_OK: i32 = 0;
/// `cap` is smaller than the result; `*out_len` holds the size needed.
pub const DL_ERR_BUFFER_TOO_SMALL: i32 = 1;
/// A required pointer is null, or a null pointer was passed with a non-zero length.
pub const DL_ERR_NULL_POINTER: i32 = 2;
/// An input string is not valid UTF-8.
pub const DL_ERR_INVALID_UTF8: i32 = 3;
/// The input was rejected by the library; the output holds the reason.
pub const DL_ERR_INVALID_INPUT: i32 = 4;

/// Borrows `ptr[..len]` as UTF-8.
///
/// # Safety
/// If `len > 0`, `ptr` must be valid for reads of `len` bytes for the duration of the call.
pub(crate) unsafe fn input<'a>(ptr: *const u8, len: usize) -> Result<&'a str, i32> {
    if len == 0 {
        return Ok("");
    }
    if ptr.is_null() {
        return Err(DL_ERR_NULL_POINTER);
    }
    // SAFETY: the caller guarantees `ptr` is valid for `len` bytes, and it is non-null.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    core::str::from_utf8(bytes).map_err(|_| DL_ERR_INVALID_UTF8)
}

/// Writes `text` to `out[..cap]` following the module conventions and returns `status`
/// (or [`DL_ERR_BUFFER_TOO_SMALL`] / [`DL_ERR_NULL_POINTER`]).
///
/// # Safety
/// `out_len` must be valid for a write. If `cap > 0`, `out` must be valid for writes of
/// `cap` bytes and must not overlap `text`.
pub(crate) unsafe fn output(
    text: &str,
    status: i32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if out_len.is_null() {
        return DL_ERR_NULL_POINTER;
    }
    let bytes = text.as_bytes();
    // SAFETY: `out_len` is non-null and the caller guarantees it is valid for a write.
    unsafe { out_len.write(bytes.len()) };
    if bytes.len() > cap {
        return DL_ERR_BUFFER_TOO_SMALL;
    }
    if bytes.is_empty() {
        return status;
    }
    if out.is_null() {
        return DL_ERR_NULL_POINTER;
    }
    // SAFETY: `out` is non-null and valid for `cap >= bytes.len()` bytes; the library's own
    // string cannot overlap caller memory.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    status
}
