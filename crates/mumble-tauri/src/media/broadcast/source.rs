//! Capture adapter; the transport remains buildable without native libraries.

use super::{frame::FrameSink, StartRequest};
use crate::media::settings::ResolvedEncoding;

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
impl Capture for crate::media::pipeline::PipelineHandle {
    fn encoder_id(&self) -> &str {
        self.encoder_id()
    }
    fn encoding(&self) -> ResolvedEncoding {
        self.encoding()
    }
    fn request_key_frame(&self) {
        self.request_key_frame();
    }
    fn stopped(&self) -> Option<Result<(), String>> {
        use crate::media::pipeline::StopReason;
        self.stop_reason().map(|reason| match reason {
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
    Ok(Box::new(pipeline::start(&config, sink)?))
}

/// Builds without capture support retain an explicit error at the command boundary.
#[cfg(not(all(target_os = "windows", feature = "native-screenshare")))]
pub(super) fn start(
    request: StartRequest,
    _data_dir: &std::path::Path,
    _sink: FrameSink,
) -> Result<Box<dyn Capture>, String> {
    request.settings.validate()?;
    let _ = (request.source, request.draw_cursor);
    Err("native screen sharing requires Windows and the native-screenshare feature".to_owned())
}
