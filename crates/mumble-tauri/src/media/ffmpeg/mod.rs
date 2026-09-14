//! `FFmpeg` FFI, confined to this subtree.
//!
//! The workspace denies `unsafe_code`; each module here re-allows it
//! locally with a reason, and exposes only safe APIs upwards.  Nothing
//! outside `media::ffmpeg` touches an `AV*` type.
//!
//! We bind `ffmpeg-sys-next` directly rather than the safe `ffmpeg-next`
//! wrapper because that wrapper has no hardware device or hardware frame
//! API at all, and the D3D11 zero-copy path is the entire point of this
//! work (see `TODO.md` 0.3).
//!
//! - [`log`] - routes `FFmpeg`'s log output into `tracing`, and captures
//!   error text so probe failures can be explained.
//! - [`params`] - encoder configuration, shared by the probe and the pipeline
//!   so the two cannot drift apart.
//! - [`probe`] - opens each candidate encoder and encodes one frame.
//! - `device` / `frame` - RAII wrappers for hardware devices, frames and
//!   packets.
//! - `graph` - turns a [`crate::media::capture`] plan into a filter graph.
//! - `session` - the opened encoder.
//! - [`pipeline`] - assembles all of it and runs the two worker threads.

#![allow(
    unsafe_code,
    reason = "FFmpeg is a C library; unsafe is confined to media::ffmpeg"
)]

mod device;
mod frame;
mod graph;
pub(crate) mod log;
pub(crate) mod params;
pub(crate) mod pipeline;
pub(crate) mod probe;
mod session;

use std::ffi::CStr;

/// Render an `FFmpeg` error code as text, e.g. `-12` -> `Cannot allocate memory`.
pub(crate) fn errstr(code: i32) -> String {
    let mut buf = [0_i8; 128];
    // SAFETY: `av_strerror` writes at most `buf.len()` bytes including the
    // NUL terminator, and always leaves the buffer NUL-terminated.
    let written = unsafe {
        let _ = ffmpeg_sys_next::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    if written.is_empty() {
        return format!("error {code}");
    }
    written
}

/// Copy a NUL-terminated C string, or `"<null>"` for a null pointer.
///
/// # Safety
///
/// `ptr` must be null or point to a NUL-terminated string that stays valid
/// for the duration of the call.
pub(crate) unsafe fn cstr(ptr: *const std::os::raw::c_char) -> String {
    if ptr.is_null() {
        return "<null>".to_owned();
    }
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}
