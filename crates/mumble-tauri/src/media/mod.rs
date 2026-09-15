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
//! - [`capture`]  - what to capture, and the filter chain that encodes it
//!   (platform independent: a chain is data, not `FFmpeg` objects).
//! - [`pipeline`] - the running capture-and-encode pipeline.
//! - `display`    - DXGI display enumeration, for the capture source picker.
//! - `ffmpeg`     - `FFmpeg` FFI, compiled only with `native-screenshare`.
//! - `gpu`        - DXGI adapter signature used as the cache key.
//!
//! Everything that touches `FFmpeg` is behind the `native-screenshare`
//! cargo feature (Windows only).  Without it the commands still exist and
//! report [`encoder::EncoderReport::supported`] as `false`, so the frontend
//! needs no platform branches.
//!
//! Video only, so far.  System audio still comes from the browser's
//! `getDisplayMedia({audio: true})`; replacing it needs a WASAPI loopback
//! capture (`cpal` is already a dependency) and somewhere to put the result,
//! which is the transport stage's business.

#[cfg(not(target_os = "android"))]
pub(crate) mod broadcast;
#[cfg(not(target_os = "android"))]
pub(crate) mod connection;
pub(crate) mod encoder;
#[cfg(not(target_os = "android"))]
pub(crate) mod frame;
pub(crate) mod settings;
pub(crate) mod transport;

/// What to capture, and the filter chain that encodes it.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod capture;
/// The running capture-and-encode pipeline.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod pipeline;
/// Hidden CLI self-test for the pipeline.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod selftest;

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod display;
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod sources;
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod ffmpeg;
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod gpu;
