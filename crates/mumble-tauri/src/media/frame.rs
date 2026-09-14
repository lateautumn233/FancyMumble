//! An encoded frame, and where encoded frames go.
//!
//! Separate from the capture pipeline because the two halves of the screen-share
//! path have different platform requirements: capture and encoding need `FFmpeg`
//! and Windows, while the WebRTC transport is portable Rust.  The frame is what
//! they agree on, so it lives where both can reach it without dragging the
//! feature gate across the transport.

/// One encoded frame, on its way to the transport stage.
#[derive(Debug, Clone)]
pub(crate) struct EncodedFrame {
    /// The bitstream: Annex-B for H.264 and HEVC, OBU for AV1.
    pub(crate) bytes: Vec<u8>,
    /// Presentation timestamp in milliseconds from the start of the stream.
    pub(crate) pts_ms: i64,
    /// Whether this is a key frame, i.e. a decodable entry point.
    pub(crate) is_key: bool,
}

/// Where encoded frames go.
///
/// Called on the encode thread, once per packet, so an implementation must
/// not block: anything slower than the frame interval shows up as a dropped
/// frame rather than as latency.
pub(crate) type FrameSink = Box<dyn FnMut(EncodedFrame) + Send>;
