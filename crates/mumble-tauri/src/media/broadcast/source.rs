//! Capture adapter; the transport remains buildable without native libraries.

use super::{frame::FrameSink, StartRequest};
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
use crate::media::audio::capture::Handle as AudioCapture;
use crate::media::audio::Packet;
use crate::media::settings::ResolvedEncoding;
use std::sync::Arc;
use tokio::sync::broadcast;

/// The operations needed by the broadcast owner, independent of the backend.
pub(super) trait Capture: Send {
    /// Encoder selected after probing the machine.
    fn encoder_id(&self) -> &str;
    /// Actual output dimensions and timing.
    fn encoding(&self) -> ResolvedEncoding;
    /// Ask the encoder to produce a decodable entry point.
    fn request_key_frame(&self);
    /// A terminal reason, or `None` while capture is running.
    fn stopped(&self) -> Option<Result<(), String>>;
}

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
struct NativeCapture {
    video: crate::media::pipeline::PipelineHandle,
    audio: Option<AudioCapture>,
}

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
impl Capture for NativeCapture {
    fn encoder_id(&self) -> &str {
        self.video.encoder_id()
    }
    fn encoding(&self) -> ResolvedEncoding {
        self.video.encoding()
    }
    fn request_key_frame(&self) {
        self.video.request_key_frame();
    }
    fn stopped(&self) -> Option<Result<(), String>> {
        use crate::media::pipeline::StopReason;
        if let Some(error) = self.audio.as_ref().and_then(AudioCapture::error) {
            return Some(Err(error));
        }
        self.video.stop_reason().map(|reason| match reason {
            StopReason::Requested | StopReason::SourceEnded => Ok(()),
            StopReason::Failed { message } => Err(message),
        })
    }
}

/// Open the selected source on a blocking thread.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(super) fn start(
    request: StartRequest,
    data_dir: &std::path::Path,
    sink: FrameSink,
    audio: Option<broadcast::Sender<Arc<Packet>>>,
) -> Result<Box<dyn Capture>, String> {
    use crate::media::{encoder, pipeline};
    request.settings.validate()?;
    let source = serde_json::from_value(request.source)
        .map_err(|e| format!("invalid capture source: {e}"))?;
    let report = encoder::report(data_dir, false);
    let selection = report.resolve_choice(&request.settings.encoder)?;
    let config = pipeline::PipelineConfig {
        source,
        settings: request.settings,
        encoder_id: selection.id,
        encoder_input: selection.input,
        draw_cursor: request.draw_cursor,
    };
    let video = pipeline::start(&config, sink)?;
    let audio = audio
        .map(|packets| AudioCapture::start(source, packets))
        .transpose()?;
    Ok(Box::new(NativeCapture { video, audio }))
}

/// Builds without capture support retain an explicit error at the command boundary.
#[cfg(not(all(target_os = "windows", feature = "native-screenshare")))]
pub(super) fn start(
    request: StartRequest,
    _data_dir: &std::path::Path,
    _sink: FrameSink,
    _audio: Option<broadcast::Sender<Arc<Packet>>>,
) -> Result<Box<dyn Capture>, String> {
    request.settings.validate()?;
    let _ = (request.source, request.draw_cursor, request.share_audio);
    Err("native screen sharing requires Windows and the native-screenshare feature".to_owned())
}
