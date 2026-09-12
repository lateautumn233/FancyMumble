//! Native screen-share capture and hardware video encoding.
//!
//! Replaces the browser's WebRTC video path (VP8 software encoding) with
//! native capture plus a hardware H.264 encoder.  See `TODO.md` in the
//! repository root for the full plan; this module covers stage 1:
//! enumerating the encoders that actually work on this machine and
//! validating the user's encoding settings.
//!
//! Layout:
//!
//! - [`settings`] - the user-facing settings model (platform independent).
//! - [`encoder`]  - encoder catalogue, subprocess probing and result cache.
//! - `ffmpeg`     - `FFmpeg` FFI, compiled only with `native-screenshare`.
//! - `gpu`        - DXGI adapter signature used as the cache key.
//!
//! Everything that touches `FFmpeg` is behind the `native-screenshare`
//! cargo feature (Windows only).  Without it the commands still exist and
//! report [`encoder::EncoderReport::supported`] as `false`, so the frontend
//! needs no platform branches.

pub(crate) mod encoder;
pub(crate) mod settings;

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod ffmpeg;
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod gpu;
