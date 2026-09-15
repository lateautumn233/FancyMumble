//! Start the capture and encode threads.
//!
//! Assembly happens on the calling thread - device, filter graph, encoder -
//! so a configuration that cannot work fails before anything is spawned and
//! the caller gets a real error instead of a pipeline that dies immediately.
//! Only then do the two threads start, each owning one `FFmpeg` object.
//!
//! The thread model, and why the encode side has its own clock, is described
//! in [`crate::media::pipeline`].

#![allow(
    unsafe_code,
    reason = "FFmpeg pipeline assembly; confined to media::ffmpeg"
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::device::{self, Device};
use super::frame::Frame;
use super::graph::{self, CaptureGraph, Pulled};
use super::session::Encoder;
use crate::media::capture::{self, ChainRequest, EncoderInput};
use crate::media::pipeline::{
    EncodedFrame, FrameSink, PipelineConfig, PipelineHandle, Shared, StopReason,
};
use crate::media::settings::{OutputSize, Resolution, ResolvedEncoding};

/// How much longer than one frame interval the encode thread waits for a
/// fresh frame before repeating the previous one.
///
/// A little slack keeps a self-pacing source like `ddagrab` off the repeat
/// path: its frames arrive one interval apart, and a deadline of exactly one
/// interval would race with them every time.
const FRAME_WAIT_SLACK: f64 = 1.5;

/// The frame the encode thread should work on next.
#[derive(Default)]
struct Slot {
    /// The most recent captured frame, if any.
    frame: Option<Frame>,
    /// Set when the capture side is finished, for whatever reason.
    ended: bool,
}

#[derive(Default, Debug)]
struct PreviewState {
    frame: Mutex<Option<Frame>>,
    sequence: AtomicU64,
    encoded: Mutex<Option<PreviewSnapshot>>,
}

/// Handle used by the Tauri command to request a point-in-time JPEG preview.
#[derive(Clone, Debug)]
pub(crate) struct PreviewHandle(Arc<PreviewState>);

impl PreviewHandle {
    /// Publish a newly captured frame and invalidate the encoded preview.
    fn publish_frame(&self, frame: &Frame) -> Result<(), String> {
        let copy = frame.new_ref()?;
        let mut slot = self
            .0
            .frame
            .lock()
            .map_err(|_| "preview frame lock poisoned".to_owned())?;
        *slot = Some(copy);
        let _ = self.0.sequence.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut encoded) = self.0.encoded.lock() {
            *encoded = None;
        }
        Ok(())
    }

    /// Publish another tick of the current frame without re-encoding it.
    ///
    /// The encoder deliberately repeats the last captured frame when the
    /// source is idle.  Advancing the preview sequence here keeps the local
    /// viewer on the configured clock while retaining the cached JPEG bytes.
    fn republish(&self) {
        let has_frame = self.0.frame.lock().map(|slot| slot.is_some()).unwrap_or(false);
        if !has_frame {
            return;
        }
        let sequence = self.0.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut encoded) = self.0.encoded.lock() {
            if let Some(snapshot) = encoded.as_mut() {
                snapshot.sequence = sequence;
            }
        }
    }

    /// Return the latest frame and its monotonically increasing sequence.
    pub(crate) fn snapshot(&self, max_width: u32) -> Result<Option<PreviewSnapshot>, String> {
        let frame = self
            .0
            .frame
            .lock()
            .map_err(|_| "preview frame lock poisoned".to_owned())?
            .as_ref()
            .map(Frame::new_ref)
            .transpose()?;
        let Some(frame) = frame else { return Ok(None); };
        let sequence = self.0.sequence.load(Ordering::Relaxed);
        if let Ok(encoded) = self.0.encoded.lock() {
            if let Some(snapshot) = encoded.as_ref().filter(|snapshot| snapshot.sequence == sequence && snapshot.width <= max_width) {
                return Ok(Some(PreviewSnapshot {
                    sequence: snapshot.sequence,
                    width: snapshot.width,
                    height: snapshot.height,
                    bytes: snapshot.bytes.clone(),
                }));
            }
        }
        let (width, height, bytes) = frame.to_jpeg(max_width)?;
        let snapshot = PreviewSnapshot { sequence, width, height, bytes };
        if let Ok(mut encoded) = self.0.encoded.lock() {
            *encoded = Some(PreviewSnapshot {
                sequence: snapshot.sequence,
                width: snapshot.width,
                height: snapshot.height,
                bytes: snapshot.bytes.clone(),
            });
        }
        Ok(Some(snapshot))
    }
}

#[derive(Debug)]
pub(crate) struct PreviewSnapshot {
    pub(crate) sequence: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes: Vec<u8>,
}

/// The mailbox between the capture and encode threads.
#[derive(Default)]
struct Mailbox {
    /// The slot itself.
    slot: Mutex<Slot>,
    /// Signalled when a frame is stored, or when capture ends.
    ready: Condvar,
}

/// Everything assembled and ready to be handed to the threads.
struct Assembled {
    /// The configured capture graph.
    graph: CaptureGraph,
    /// The opened encoder.
    encoder: Encoder,
    /// What the encoder was actually opened with, after the source was
    /// measured and the capture filter rounded its canvas.
    encoding: ResolvedEncoding,
    /// Devices, kept alive for as long as the graph and encoder use them.
    devices: Vec<Arc<Device>>,
}

/// Build the pipeline and start its threads.
///
/// `adapter_index` selects the DXGI adapter the capture device is created on;
/// it must be the adapter that drives the captured display, because `ddagrab`
/// looks its output index up on the device's own adapter.  `source_size` is
/// the source's known size, from DXGI for a monitor; for a window it is only a
/// starting guess and gets measured.
pub(crate) fn start(
    config: &PipelineConfig,
    adapter_index: u32,
    source_size: OutputSize,
    mut sink: FrameSink,
) -> Result<PipelineHandle, String> {
    let assembled = assemble(config, adapter_index, source_size)?;
    let Assembled { mut graph, mut encoder, encoding, devices } = assembled;
    let encode_span = tracing::info_span!("screen_share_encode",
        encoder = %config.encoder_id, capture = ?config.settings.capture,
        adapter_index, width = encoding.size.width, height = encoding.size.height,
        fps = encoding.fps, bitrate_kbps = encoding.bitrate_kbps);

    let shared = Arc::new(Shared::default());
    let preview = PreviewHandle(Arc::new(PreviewState::default()));
    let mailbox = Arc::new(Mailbox::default());
    let fps = encoding.fps.max(1);

    let capture_shared = Arc::clone(&shared);
    let capture_mailbox = Arc::clone(&mailbox);
    let capture_devices = devices.clone();
    let capture = std::thread::Builder::new()
        .name("screenshare-capture".to_owned())
        .spawn(move || {
            capture_loop(&mut graph, &capture_shared, &capture_mailbox);
            // Devices outlive the graph, which holds references into them.
            drop(capture_devices);
        })
        .map_err(|e| format!("could not start capture thread: {e}"))?;

    let encode_shared = Arc::clone(&shared);
    let encode_mailbox = Arc::clone(&mailbox);
    let encode_preview = preview.clone();
    let encode = std::thread::Builder::new()
        .name("screenshare-encode".to_owned())
        .spawn(move || {
            let _entered = encode_span.enter();
            encode_loop(
                &mut encoder,
                &encode_shared,
                &encode_mailbox,
                fps,
                &mut sink,
                &encode_preview,
            );
            drop(devices);
        })
        .map_err(|e| format!("could not start encode thread: {e}"))?;

    Ok(PipelineHandle::new(
        shared,
        vec![capture, encode],
        config.encoder_id.clone(),
        encoding,
        preview,
    ))
}

/// Create the devices, build the graph, and open the encoder.
fn assemble(
    config: &PipelineConfig,
    adapter_index: u32,
    source_size: OutputSize,
) -> Result<Assembled, String> {
    super::log::install();

    let d3d11 = Arc::new(device::d3d11(adapter_index)?);
    let input = config.encoder_input;

    // Resolve the settings only once the source's real size is known: "native"
    // and the presets both mean something different for a 2560x1440 monitor
    // than for a 1280x720 window.
    let source_size = resolve_source_size(config, &d3d11, source_size);
    let requested = config.settings.resolve(source_size)?;

    let steps = capture::plan(&ChainRequest {
        source: config.source,
        backend: config.settings.capture,
        source_size,
        target_size: requested.size,
        fps: requested.fps,
        input,
        draw_cursor: config.draw_cursor,
    })?;

    tracing::info!(encoder = %config.encoder_id, adapter_index, source_size = ?source_size,
        encoding = ?requested, input = ?input, filters = ?steps,
        "screen-share capture chain configured");

    // Only the D3D12 encoders need a second device, and it is a plain upload
    // target, so it is created lazily.
    let d3d12 = if input == EncoderInput::HardwareD3d12 {
        Some(Arc::new(device::d3d12()?))
    } else {
        None
    };

    let graph = graph::build(&steps, &d3d11, d3d12.as_deref())?;
    let format = graph.format();
    let actual_size = format.size();

    if actual_size != requested.size {
        // Not fatal, and the frames win: an encoder context smaller than its
        // input crops rather than scales (`TODO.md` 0.7), so it has to be
        // opened for what the graph actually produces.
        tracing::info!(
            "screen-share capture produced {}x{}, not the requested {}x{}",
            actual_size.width,
            actual_size.height,
            requested.size.width,
            requested.size.height
        );
    }

    let encoding = ResolvedEncoding { size: actual_size, ..requested };
    let encoder = Encoder::open(
        &config.encoder_id,
        format,
        i64::from(encoding.bitrate_kbps) * 1000,
        encoding.fps,
        graph.hw_frames_ctx(),
    )?;

    let mut devices = vec![d3d11];
    devices.extend(d3d12);
    Ok(Assembled { graph, encoder, encoding, devices })
}

/// The size the capture source will actually produce.
///
/// A monitor's size comes from DXGI and is exact.  A window's is not its
/// window rectangle - `gfxcapture` derives it from what Windows Graphics
/// Capture hands over, after border and client-area adjustments - and is only
/// settled once a graph has been configured.
///
/// Measuring it means configuring a throwaway source-only graph, which for a
/// window is the slowest part of startup, so it is only done when the answer
/// can change the outcome: at native resolution the chain has no scaling stage
/// either way, and reporting the source as its own size is exactly right.
fn resolve_source_size(
    config: &PipelineConfig,
    d3d11: &Device,
    known: OutputSize,
) -> OutputSize {
    let needs_measuring = matches!(config.source, capture::CaptureSource::Window { .. })
        && config.settings.resolution != Resolution::Native;
    if !needs_measuring {
        return known;
    }

    let steps =
        capture::probe_plan(config.source, config.settings.capture, config.settings.fps);
    match graph::build(&steps, d3d11, None) {
        Ok(probe) => probe.format().size(),
        Err(e) => {
            tracing::warn!("could not measure capture source, assuming {known:?}: {e}");
            known
        }
    }
}

/// Pull frames until asked to stop, or until the source ends.
fn capture_loop(graph: &mut CaptureGraph, shared: &Shared, mailbox: &Mailbox) {
    let mut frame = match Frame::empty() {
        Ok(frame) => frame,
        Err(e) => {
            shared.finish(StopReason::Failed { message: e });
            mailbox.close();
            return;
        }
    };

    while !shared.should_stop() {
        match graph.pull(&mut frame) {
            Ok(Pulled::Frame) => {
                if !store(&frame, shared, mailbox) {
                    break;
                }
                frame.unref();
            }
            // The source had nothing to give within its own wait; loop round
            // and check the stop flag again.
            Ok(Pulled::Idle) => {}
            Ok(Pulled::Ended) => {
                shared.finish(StopReason::SourceEnded);
                break;
            }
            Err(message) => {
                shared.finish(StopReason::Failed { message });
                break;
            }
        }
    }

    mailbox.close();
}

/// Put a new reference to `frame` in the slot, replacing anything there.
///
/// Returns `false` when the reference could not be taken, which is fatal.
fn store(frame: &Frame, shared: &Shared, mailbox: &Mailbox) -> bool {
    let reference = match frame.new_ref() {
        Ok(reference) => reference,
        Err(message) => {
            shared.finish(StopReason::Failed { message });
            return false;
        }
    };

    let Ok(mut slot) = mailbox.slot.lock() else {
        shared.finish(StopReason::Failed { message: "frame slot poisoned".to_owned() });
        return false;
    };

    // Latest-wins: an unencoded frame still in the slot is dropped rather
    // than queued, so falling behind costs freshness, not memory.
    let displaced = slot.frame.is_some();
    slot.frame = Some(reference);
    drop(slot);

    shared.stats.note_captured(displaced);
    mailbox.ready.notify_one();
    true
}

impl Mailbox {
    /// Mark capture as finished and wake the encode thread.
    fn close(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.ended = true;
        }
        self.ready.notify_all();
    }
}

/// What the encode thread should do next.
enum Next {
    /// A newly captured frame.
    Fresh(Frame),
    /// Nothing new arrived in time; re-send the previous frame.
    Repeat,
    /// Capture has finished.
    Done,
}

/// Encode on a real clock until asked to stop.
fn encode_loop(
    encoder: &mut Encoder,
    shared: &Shared,
    mailbox: &Mailbox,
    fps: u32,
    sink: &mut FrameSink,
    preview: &PreviewHandle,
) {
    let interval = Duration::from_secs_f64(FRAME_WAIT_SLACK / f64::from(fps));
    let started = Instant::now();
    let mut current: Option<Frame> = None;
    let mut last_pts = i64::MIN;
    let mut packets = Vec::new();
    let mut first_packet_logged = false;

    while !shared.should_stop() {
        let repeated = match wait_for_frame(mailbox, interval) {
            Next::Fresh(frame) => {
                current = Some(frame);
                false
            }
            Next::Repeat => true,
            Next::Done => break,
        };

        let Some(source) = current.as_ref() else {
            // Nothing captured yet, so there is nothing to repeat either.
            continue;
        };

        let outcome = encode_one(
            encoder,
            source,
            shared,
            EncodeClock { started, last_pts: &mut last_pts },
            &mut packets,
            sink,
            repeated,
        );
        if let Err(message) = outcome {
            tracing::error!(error = %message, elapsed_ms = started.elapsed().as_millis() as u64,
                pts_ms = last_pts, repeated, stats = ?shared.stats.snapshot(),
                "screen-share encoding failed");
            shared.finish(StopReason::Failed { message });
            break;
        }
        if repeated {
            preview.republish();
        } else if let Err(error) = preview.publish_frame(source) {
            tracing::debug!(%error, "could not cache native preview frame");
        }
        if !first_packet_logged && shared.stats.snapshot().packets > 0 {
            first_packet_logged = true;
            tracing::info!(elapsed_ms = started.elapsed().as_millis() as u64,
                stats = ?shared.stats.snapshot(), "screen-share first encoded packet produced");
        }
    }
}

/// Wait for a freshly captured frame, up to `timeout`.
fn wait_for_frame(mailbox: &Mailbox, timeout: Duration) -> Next {
    let Ok(mut slot) = mailbox.slot.lock() else {
        return Next::Done;
    };

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = slot.frame.take() {
            return Next::Fresh(frame);
        }
        // Checked after taking any frame that is already there, so the last
        // captured frame is still encoded when capture ends in the same breath.
        if slot.ended {
            return Next::Done;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Next::Repeat;
        }
        let Ok((next, _)) = mailbox.ready.wait_timeout(slot, remaining) else {
            return Next::Done;
        };
        slot = next;
    }
}

/// The encode thread's timestamp source.
struct EncodeClock<'a> {
    /// When the broadcast started.
    started: Instant,
    /// The last timestamp emitted, so the next one can be kept ahead of it.
    last_pts: &'a mut i64,
}

impl EncodeClock<'_> {
    /// The next presentation timestamp, in the encoder's millisecond base.
    ///
    /// Taken from our own wall clock rather than the capture filter's
    /// timeline: a repeated frame has no source timestamp of its own, and
    /// encoders reject a timestamp that does not advance.
    fn next_pts(&mut self) -> i64 {
        let elapsed = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let pts = elapsed.max(self.last_pts.saturating_add(1));
        *self.last_pts = pts;
        pts
    }
}

/// Encode one frame and hand every packet to the sink.
///
/// The frame is a *new reference* to the captured surface, not the surface
/// itself, so setting a timestamp or forcing a key frame here cannot disturb
/// the copy the repeat path keeps hold of.
fn encode_one(
    encoder: &mut Encoder,
    source: &Frame,
    shared: &Shared,
    mut clock: EncodeClock<'_>,
    packets: &mut Vec<super::session::EncodedPacket>,
    sink: &mut FrameSink,
    repeated: bool,
) -> Result<(), String> {
    let mut frame = source.new_ref()?;
    frame.set_pts(clock.next_pts());

    let force_key = shared.take_key_frame_request();
    packets.clear();
    let spent = encoder.encode(&mut frame, force_key, packets)?;
    shared.stats.note_encoded(repeated, u64::try_from(spent.as_micros()).unwrap_or(u64::MAX));

    for packet in packets.drain(..) {
        shared.stats.note_packet(packet.bytes.len(), packet.key);
        sink(EncodedFrame {
            bytes: packet.bytes,
            pts_ms: packet.pts,
            is_key: packet.key,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_wait_leaves_room_for_a_self_paced_source() {
        // At 30 fps a frame arrives every ~33 ms; the encode thread must wait
        // longer than that before deciding to repeat, or it would repeat on
        // every single tick of a perfectly healthy ddagrab source.
        let interval = Duration::from_secs_f64(FRAME_WAIT_SLACK / 30.0);
        assert!(interval > Duration::from_millis(33), "{interval:?}");
        assert!(interval < Duration::from_millis(100), "{interval:?} is too slack");
    }
}
