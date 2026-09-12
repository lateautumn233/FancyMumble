//! Bridge `FFmpeg`'s logging into `tracing`, and capture error text.
//!
//! `FFmpeg` reports *why* an encoder refused to open only through its log
//! callback - `avcodec_open2` just returns a generic error code.  The
//! difference between "no AMD driver installed" and "out of memory" is
//! exactly what the settings page needs to show, so the callback keeps the
//! most recent error lines in a buffer that [`take_errors`] drains.

#![allow(
    unsafe_code,
    reason = "FFmpeg log callback is an extern \"C\" fn; confined to this module"
)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ffmpeg_sys_next as ff;

/// Error lines captured since the last [`take_errors`] call.
static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether the callback should record error lines at all.
static CAPTURING: AtomicBool = AtomicBool::new(false);

/// Largest single formatted log line we keep, in bytes.
const LINE_CAP: usize = 1024;

/// Most lines retained; a failing encoder rarely logs more than a handful,
/// and this stops a chatty driver from growing the buffer without bound.
const MAX_LINES: usize = 32;

/// Install the `tracing` bridge as `FFmpeg`'s log callback.
///
/// Idempotent, and safe to call from any thread.  `FFmpeg`'s default level
/// still applies; we set it to `ERROR` so vendor libraries do not spam the
/// log with per-frame chatter.
pub(crate) fn install() {
    // SAFETY: `log_callback` matches the signature FFmpeg expects, and is
    // valid for the remainder of the process.
    unsafe {
        ff::av_log_set_level(ff::AV_LOG_ERROR);
        ff::av_log_set_callback(Some(log_callback));
    }
}

/// Start recording error lines, discarding anything recorded earlier.
pub(crate) fn begin_capture() {
    if let Ok(mut buf) = CAPTURED.lock() {
        buf.clear();
    }
    CAPTURING.store(true, Ordering::Release);
}

/// Stop recording and return the lines captured, oldest first.
pub(crate) fn take_errors() -> Vec<String> {
    CAPTURING.store(false, Ordering::Release);
    CAPTURED.lock().map(|mut buf| std::mem::take(&mut *buf)).unwrap_or_default()
}

/// `FFmpeg` log callback: formats the line, forwards it to `tracing`, and
/// records it when capturing.
///
/// # Safety
///
/// Called by `FFmpeg` with a valid `fmt` and matching `args`; `avcl` is either
/// null or a pointer to a struct beginning with an `AVClass` pointer.
unsafe extern "C" fn log_callback(
    avcl: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    args: ff::va_list,
) {
    if level > ff::AV_LOG_ERROR {
        return;
    }

    let mut line = [0_i8; LINE_CAP];
    let mut print_prefix: c_int = 1;
    // SAFETY: `av_log_format_line2` consumes `args` exactly once and writes
    // at most `line.len()` bytes, NUL-terminating the result.
    let written = unsafe {
        ff::av_log_format_line2(
            avcl,
            level,
            fmt,
            args,
            line.as_mut_ptr(),
            c_int::try_from(line.len()).unwrap_or(c_int::MAX),
            &mut print_prefix,
        )
    };
    if written <= 0 {
        return;
    }

    // SAFETY: the buffer was just NUL-terminated by FFmpeg.
    let text = unsafe { super::cstr(line.as_ptr()) };
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }

    tracing::warn!(target: "ffmpeg", "{text}");

    if CAPTURING.load(Ordering::Acquire) {
        if let Ok(mut buf) = CAPTURED.lock() {
            if buf.len() < MAX_LINES {
                buf.push(text.to_owned());
            }
        }
    }
}
