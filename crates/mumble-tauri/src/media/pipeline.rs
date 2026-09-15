//! The capture-and-encode pipeline: configuration, statistics, and handle.
//!
//! Stage 2 of `TODO.md`.  This module owns the vocabulary; the machinery is in
//! `ffmpeg::pipeline`, behind the `native-screenshare` feature.
//!
//! Two threads, connected by a one-slot mailbox rather than a queue:
//!
//! - the **capture thread** pulls frames from the filter graph and drops each
//!   one into the slot, replacing whatever was there.  Latest-wins, so a slow
//!   encoder falls behind by dropping stale frames rather than by building a
//!   backlog of them;
//! - the **encode thread** waits for a fresh frame, but no longer than one
//!   frame interval.  When the wait times out it re-sends the previous frame.
//!
//! That timer is not optional.  `gfxcapture` only produces a frame when the
//! content changes - an idle or occluded window can go tens of seconds without
//! one (`TODO.md` 0.7) - and a receiver that gets no packets cannot tell a
//! still picture from a broken stream.  `ddagrab` paces itself, so its slot is
//! normally refilled before the timer fires and the repeat path stays unused.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::capture::{CaptureSource, EncoderInput};
use super::settings::{ResolvedEncoding, ScreenShareSettings};

pub(crate) use super::frame::{EncodedFrame, FrameSink};

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) use super::ffmpeg::pipeline::{PreviewHandle, PreviewSnapshot};

/// Everything the pipeline needs to start.
///
/// The settings are the *unresolved* ones on purpose.  Resolving them needs
/// the capture source's real size, and for a window that is only known once a
/// graph has been configured, so the pipeline measures first and resolves
/// afterwards (see `ffmpeg::pipeline`).
#[derive(Debug, Clone)]
pub(crate) struct PipelineConfig {
    /// What to capture.
    pub(crate) source: CaptureSource,
    /// The user's encoding settings, including which capture backend to use.
    pub(crate) settings: ScreenShareSettings,
    /// The `FFmpeg` encoder to open, as resolved from the user's choice
    /// against the probe results.
    pub(crate) encoder_id: String,
    /// The input path that the encoder probe successfully opened and encoded.
    pub(crate) encoder_input: EncoderInput,
    /// Whether to composite the mouse cursor into the frames.
    pub(crate) draw_cursor: bool,
}

/// Why the pipeline stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum StopReason {
    /// [`PipelineHandle::stop`] was called.
    Requested,
    /// The capture source went away: the window was closed, or the display
    /// was detached.  `gfxcapture` reports this as end-of-stream, `ddagrab`
    /// as a format change.
    SourceEnded,
    /// Capture or encoding failed.
    #[serde(rename_all = "camelCase")]
    Failed {
        /// What went wrong, including `FFmpeg`'s own error text.
        message: String,
    },
}

/// Counters the diagnostics UI and logs read while a broadcast runs.
///
/// Plain atomics: every field is written by exactly one thread and read by
/// any, so relaxed ordering is enough - a stats display that is a frame
/// behind is not a problem worth a lock for.
#[derive(Debug, Default)]
pub(crate) struct Stats {
    /// Frames the capture graph produced.
    captured: AtomicU64,
    /// Frames handed to the encoder, including repeats.
    encoded: AtomicU64,
    /// Frames sent again because no new one arrived in time.
    repeated: AtomicU64,
    /// Captured frames replaced in the slot before the encoder took them.
    dropped: AtomicU64,
    /// Encoded packets emitted.
    packets: AtomicU64,
    /// Total encoded bytes.
    bytes: AtomicU64,
    /// Key frames emitted.
    key_frames: AtomicU64,
    /// Total time spent inside the encoder, in microseconds.
    encode_micros: AtomicU64,
}

impl Stats {
    /// Record a captured frame, and whether it displaced an unencoded one.
    pub(crate) fn note_captured(&self, displaced: bool) {
        let _ = self.captured.fetch_add(1, Ordering::Relaxed);
        if displaced {
            let _ = self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a frame handed to the encoder.
    pub(crate) fn note_encoded(&self, repeated: bool, micros: u64) {
        let _ = self.encoded.fetch_add(1, Ordering::Relaxed);
        let _ = self.encode_micros.fetch_add(micros, Ordering::Relaxed);
        if repeated {
            let _ = self.repeated.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record an emitted packet.
    pub(crate) fn note_packet(&self, bytes: usize, is_key: bool) {
        let _ = self.packets.fetch_add(1, Ordering::Relaxed);
        let _ = self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        if is_key {
            let _ = self.key_frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Read every counter.
    pub(crate) fn snapshot(&self) -> StatsSnapshot {
        let encoded = self.encoded.load(Ordering::Relaxed);
        let encode_micros = self.encode_micros.load(Ordering::Relaxed);
        StatsSnapshot {
            captured: self.captured.load(Ordering::Relaxed),
            encoded,
            repeated: self.repeated.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            packets: self.packets.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            key_frames: self.key_frames.load(Ordering::Relaxed),
            mean_encode_micros: if encoded == 0 { 0 } else { encode_micros / encoded },
        }
    }
}

/// A point-in-time reading of [`Stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatsSnapshot {
    /// Frames the capture graph produced.
    pub(crate) captured: u64,
    /// Frames handed to the encoder, including repeats.
    pub(crate) encoded: u64,
    /// Frames re-sent because the source produced nothing in time.
    pub(crate) repeated: u64,
    /// Captured frames dropped because the encoder was still busy.
    pub(crate) dropped: u64,
    /// Encoded packets emitted.
    pub(crate) packets: u64,
    /// Total encoded bytes.
    pub(crate) bytes: u64,
    /// Key frames emitted.
    pub(crate) key_frames: u64,
    /// Mean time spent inside the encoder per frame, in microseconds.
    pub(crate) mean_encode_micros: u64,
}

/// State shared with the pipeline's threads.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Set to ask both threads to finish.
    stop: AtomicBool,
    /// Set to force the next frame to be coded as a key frame.
    key_frame_wanted: AtomicBool,
    /// Why the pipeline stopped, once it has.  The first writer wins, so a
    /// capture failure is not overwritten by the shutdown that follows it.
    reason: std::sync::Mutex<Option<StopReason>>,
    /// Live counters.
    pub(crate) stats: Stats,
}

impl Shared {
    /// Whether the threads have been asked to stop.
    pub(crate) fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ask the threads to stop.
    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Record why the pipeline is stopping, and ask it to stop.
    ///
    /// Only the first reason is kept: a source that ends causes a shutdown,
    /// and the shutdown must not relabel it as merely requested.
    pub(crate) fn finish(&self, reason: StopReason) {
        if let Ok(mut slot) = self.reason.lock() {
            if slot.is_none() {
                *slot = Some(reason);
            }
        }
        self.request_stop();
    }

    /// The recorded stop reason, if the pipeline has stopped.
    pub(crate) fn stop_reason(&self) -> Option<StopReason> {
        self.reason.lock().ok().and_then(|slot| slot.clone())
    }

    /// Ask for a key frame on the next encoded frame.
    pub(crate) fn request_key_frame(&self) {
        self.key_frame_wanted.store(true, Ordering::Release);
    }

    /// Consume a pending key-frame request, if any.
    pub(crate) fn take_key_frame_request(&self) -> bool {
        self.key_frame_wanted.swap(false, Ordering::AcqRel)
    }
}

/// A running pipeline.
///
/// Dropping the handle stops the pipeline and waits for its threads, so a
/// broadcast cannot outlive the object that owns it.
#[derive(Debug)]
pub(crate) struct PipelineHandle {
    /// Shared flags and counters.
    shared: Arc<Shared>,
    /// The capture thread, and the encode thread.
    threads: Vec<std::thread::JoinHandle<()>>,
    /// What the encoder was actually opened as.
    encoder_id: String,
    /// The parameters the encoder was actually opened with, which can differ
    /// from the request by a pixel or two once the capture filter has rounded
    /// its canvas.  The transport stage negotiates from these, not from what
    /// was asked for.
    encoding: ResolvedEncoding,
    /// Latest captured frame, exposed only to the local preview command.
    preview: PreviewHandle,
}

impl PipelineHandle {
    /// Assemble a handle around already-started threads.
    pub(crate) fn new(
        shared: Arc<Shared>,
        threads: Vec<std::thread::JoinHandle<()>>,
        encoder_id: String,
        encoding: ResolvedEncoding,
        preview: PreviewHandle,
    ) -> Self {
        Self { shared, threads, encoder_id, encoding, preview }
    }

    /// The encoder in use.
    pub(crate) fn encoder_id(&self) -> &str {
        &self.encoder_id
    }

    /// What the encoder was actually opened with.
    pub(crate) fn encoding(&self) -> ResolvedEncoding {
        self.encoding
    }

    /// Handle for requesting a native point-in-time preview image.
    pub(crate) fn preview(&self) -> PreviewHandle {
        self.preview.clone()
    }

    /// Current counters.
    pub(crate) fn stats(&self) -> StatsSnapshot {
        self.shared.stats.snapshot()
    }

    /// Ask for a key frame, as a receiver's PLI or FIR would.
    pub(crate) fn request_key_frame(&self) {
        self.shared.request_key_frame();
    }

    /// Whether the pipeline has stopped on its own, and why.
    ///
    /// `None` while it is still running.  A caller that polls this can tell a
    /// closed window ([`StopReason::SourceEnded`]) from a real failure without
    /// waiting for the threads.
    pub(crate) fn stop_reason(&self) -> Option<StopReason> {
        self.shared.stop_reason()
    }

    /// Stop the pipeline, wait for both threads, and report why it ended.
    ///
    /// Can take up to about a second: the capture thread only notices the
    /// request between pulls, and a pull from an idle `gfxcapture` source
    /// blocks for that long inside the filter.
    pub(crate) fn stop(mut self) -> StopReason {
        self.shutdown();
        self.shared.stop_reason().unwrap_or(StopReason::Requested)
    }

    /// Signal, then join.  Idempotent, so `stop` and `drop` can both call it.
    fn shutdown(&mut self) {
        self.shared.finish(StopReason::Requested);
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                tracing::warn!("screen-share pipeline thread panicked");
            }
        }
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start capturing and encoding.
///
/// Assembly is synchronous: the device, filter graph and encoder are all
/// created before this returns, so an unusable configuration is an error here
/// rather than a pipeline that dies a moment later.  Expect this to take a few
/// hundred milliseconds - a first hardware-encoder open pays driver
/// initialisation, and window capture pays a Windows Graphics Capture session
/// setup on top.
///
/// `sink` is called on the encode thread for every packet.
pub(crate) fn start(
    config: &PipelineConfig,
    sink: FrameSink,
) -> Result<PipelineHandle, String> {
    tracing::info!(encoder = %config.encoder_id, source = ?config.source,
        settings = ?config.settings, "screen-share pipeline starting");
    let result = locate(config.source).and_then(|(adapter_index, source_size)| {
        super::ffmpeg::pipeline::start(config, adapter_index, source_size, sink)
    });
    if let Err(error) = &result {
        tracing::error!(encoder = %config.encoder_id, error = %error,
            "screen-share pipeline startup failed");
    }
    result
}

/// Find the adapter that drives a capture source, and the source's size.
///
/// The adapter matters: `ddagrab` resolves its output index against the
/// adapter of the device it is handed, so a device on the wrong one captures
/// the wrong screen or fails to find the output at all.
fn locate(source: CaptureSource) -> Result<(u32, super::settings::OutputSize), String> {
    use super::display;

    match source {
        CaptureSource::Monitor { .. } => {
            let selected = display::find(source)?;
            tracing::info!(adapter_index = selected.adapter_index,
                adapter_name = %selected.adapter_name, output_index = selected.output_index,
                "screen-share capture adapter selected");
            Ok((selected.adapter_index, selected.size))
        }
        CaptureSource::Window { .. } => {
            // A window has no adapter of its own, and its size is measured
            // rather than looked up (see `ffmpeg::pipeline`).  The primary
            // display's adapter is the right default: it is where an
            // unmoved window lives, and on a single-GPU machine - which is
            // every machine until the multi-GPU work in stage 1 lands - it is
            // the only one.
            let displays = display::enumerate()?;
            let primary = displays
                .iter()
                .find(|d| d.is_primary)
                .or_else(|| displays.first())
                .ok_or_else(|| "no display found".to_owned())?;
            tracing::info!(adapter_index = primary.adapter_index,
                adapter_name = %primary.adapter_name,
                "screen-share window capture adapter selected");
            Ok((primary.adapter_index, primary.size))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_reason_wins() {
        // Every shutdown path ends in `finish(Requested)`, so a real failure
        // would be relabelled as a user request if the first reason did not
        // stick - and the UI would report a crash as a normal stop.
        let shared = Shared::default();
        shared.finish(StopReason::Failed { message: "encoder died".to_owned() });
        shared.finish(StopReason::Requested);
        assert_eq!(
            shared.stop_reason(),
            Some(StopReason::Failed { message: "encoder died".to_owned() })
        );
    }

    #[test]
    fn key_frame_request_is_consumed_once() {
        // A request that repeated would make every subsequent frame an IDR.
        let shared = Shared::default();
        shared.request_key_frame();
        assert!(shared.take_key_frame_request());
        assert!(!shared.take_key_frame_request());
    }
}
