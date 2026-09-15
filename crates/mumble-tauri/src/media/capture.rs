//! What to capture, and the filter chain that turns it into encoder input.
//!
//! Stage 2 of the plan in `TODO.md`.  Both capture backends are `FFmpeg`
//! filters that emit `AV_PIX_FMT_D3D11` BGRA frames, so a single chain shape
//! serves both: the source filter differs, and everything after it depends
//! only on what the chosen encoder will accept.
//!
//! Deliberately free of `FFmpeg` types.  A chain is a list of
//! [`FilterStep`]s - a filter name, its options, and which hardware device
//! it needs - which makes the interesting decisions (does this need a CPU
//! round trip?  which pixel format?) testable without a GPU.  The `ffmpeg`
//! submodule turns the plan into a real graph.

use serde::{Deserialize, Serialize};

use super::settings::{CaptureBackend, OutputSize};

/// What the user picked to share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum CaptureSource {
    /// A whole display.  Both backends can do this, addressed differently:
    /// `ddagrab` takes a DXGI output index, `gfxcapture` an `HMONITOR`.  A
    /// monitor descriptor carries both so either backend can be used
    /// without re-enumerating.
    #[serde(rename_all = "camelCase")]
    Monitor {
        /// Index of the output on its adapter, as `IDXGIAdapter::EnumOutputs`
        /// orders them - which is exactly what `ddagrab`'s `output_idx` means.
        output_index: u32,
        /// The `HMONITOR` for the same display, for `gfxcapture`.
        #[serde(deserialize_with = "deserialize_handle")]
        hmonitor: u64,
    },
    /// A single window, by handle.  `gfxcapture` only.
    #[serde(rename_all = "camelCase")]
    Window {
        /// The window's `HWND`.
        ///
        /// Always an explicit handle, never a title or executable pattern:
        /// `gfxcapture`'s own matching takes the first visible top-level
        /// window it finds, which for a multi-window process like a browser
        /// is often a hidden one that never repaints, and the capture then
        /// simply never produces a frame (`TODO.md` 0.7).
        #[serde(deserialize_with = "deserialize_handle")]
        hwnd: u64,
    },
}

fn deserialize_handle<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Handle {
        Text(String),
        Number(u64),
    }
    match Handle::deserialize(deserializer)? {
        Handle::Text(value) => value.parse().map_err(serde::de::Error::custom),
        Handle::Number(value) => Ok(value),
    }
}

impl CaptureSource {
    /// Whether this source can be captured with `backend`.
    fn is_supported_by(self, backend: CaptureBackend) -> bool {
        match self {
            Self::Monitor { .. } => true,
            // DXGI Desktop Duplication addresses whole outputs only.
            Self::Window { .. } => backend == CaptureBackend::Gfxcapture,
        }
    }
}

/// The pixel format family an encoder takes frames in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncoderInput {
    /// Takes the capture filter's D3D11 BGRA frames as they are.  No
    /// conversion filter at all, which is what makes the hardware path
    /// zero-copy (`TODO.md` 0.7 measured NVENC and AMF accepting `bgra` and
    /// `d3d11` directly).
    HardwareD3d11,
    /// Takes D3D12 hardware frames, which means a round trip: download the
    /// D3D11 frame, convert to NV12, upload to a D3D12 device.
    HardwareD3d12,
    /// Takes system-memory NV12.  QSV and Media Foundation; the latter
    /// advertises `d3d11` but rejects it at open time with `E_NOTIMPL`.
    SysMemNv12,
    /// Takes system-memory `yuv420p`.  `libopenh264`, which does not accept
    /// NV12 at all.
    SysMemYuv420p,
}

impl EncoderInput {
    /// What the named `FFmpeg` encoder wants, keyed on its vendor suffix.
    ///
    /// Suffix rather than full name so the HEVC and AV1 variants of each
    /// vendor's encoder are covered by the same rule as the H.264 one.
    /// Measured per encoder with `avcodec_get_supported_config`; see the
    /// input-format table in `TODO.md` 0.7.
    pub(crate) fn for_encoder(id: &str) -> Self {
        if id.ends_with("_nvenc") || id.ends_with("_amf") {
            return Self::HardwareD3d11;
        }
        if id.ends_with("_d3d12va") {
            return Self::HardwareD3d12;
        }
        if id == "libopenh264" {
            return Self::SysMemYuv420p;
        }
        // QSV (`nv12` or `qsv` only) and Media Foundation.  Anything we do
        // not recognise gets the most widely accepted format.
        Self::SysMemNv12
    }

    /// The `FFmpeg` pixel format name a `format` filter should select
    /// before the encoder, or `None` when the frames stay BGRA.
    fn target_format(self) -> Option<&'static str> {
        match self {
            Self::HardwareD3d11 => None,
            Self::HardwareD3d12 | Self::SysMemNv12 => Some("nv12"),
            Self::SysMemYuv420p => Some("yuv420p"),
        }
    }

    /// Whether frames must end up on a hardware device, and which one.
    fn upload_target(self) -> Option<DeviceSlot> {
        match self {
            Self::HardwareD3d11 => Some(DeviceSlot::D3d11),
            Self::HardwareD3d12 => Some(DeviceSlot::D3d12),
            Self::SysMemNv12 | Self::SysMemYuv420p => None,
        }
    }
}

/// Which hardware device a filter should be given before initialisation.
///
/// Every filter in the graph gets one where it makes sense, which is what
/// `hwupload` requires (it reads `hw_device_ctx` in its `init`, before the
/// graph is configured) and what makes the capture sources use *our* device
/// instead of creating their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceSlot {
    /// The shared D3D11VA device: capture, scaling and the D3D11 encoders.
    D3d11,
    /// A separate D3D12VA device, only for uploading to a D3D12 encoder.
    D3d12,
}

/// One filter in a linear chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilterStep {
    /// `FFmpeg` filter name, e.g. `ddagrab`.
    pub(crate) name: &'static str,
    /// Options, applied through an `AVDictionary`.
    ///
    /// Never formatted into a filtergraph string: option values can contain
    /// characters the filtergraph parser treats as syntax, and building the
    /// graph filter by filter is also what lets `hwupload` see its device
    /// before it initialises (`TODO.md` 0.7).
    pub(crate) options: Vec<(&'static str, String)>,
    /// Which device to attach, if any.
    pub(crate) device: Option<DeviceSlot>,
}

impl FilterStep {
    /// A step with a D3D11 device attached.
    fn d3d11(name: &'static str, options: Vec<(&'static str, String)>) -> Self {
        Self { name, options, device: Some(DeviceSlot::D3d11) }
    }

    /// A step needing no device: `hwdownload` takes the frames context from
    /// its input, and `format` / `scale` work in system memory.
    fn plain(name: &'static str, options: Vec<(&'static str, String)>) -> Self {
        Self { name, options, device: None }
    }
}

/// Everything needed to plan a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChainRequest {
    /// What to capture.
    pub(crate) source: CaptureSource,
    /// Which capture filter to use.
    pub(crate) backend: CaptureBackend,
    /// The source's real size, from DXGI for a monitor or from a probe graph
    /// for a window (see [`probe_plan`]).
    pub(crate) source_size: OutputSize,
    /// The size the encoder will be configured for.
    pub(crate) target_size: OutputSize,
    /// Capture frame rate.  A ceiling for `gfxcapture`, a pace for
    /// `ddagrab`; the real cadence comes from the pipeline's own clock.
    pub(crate) fps: u32,
    /// What the chosen encoder accepts.
    pub(crate) input: EncoderInput,
    /// Whether to composite the mouse cursor into the frames.
    pub(crate) draw_cursor: bool,
}

/// The source filter alone, at the source's native size.
///
/// Used to find out how big a window actually is.  Window capture size is
/// not the window rectangle - `gfxcapture` derives it from what Windows
/// Graphics Capture hands over, after border and client-area adjustments -
/// and the size is only settled once the graph is configured.  Configuring
/// this one-filter graph and reading the sink's dimensions answers that
/// without pulling a frame.  Monitors skip this: DXGI already knows.
pub(crate) fn probe_plan(source: CaptureSource, backend: CaptureBackend, fps: u32) -> Vec<FilterStep> {
    vec![source_step(source, backend, fps, true, None)]
}

/// Plan the full capture-to-encoder chain.
///
/// Returns the steps in order, source first.  The shape depends on three
/// things: which backend (only `gfxcapture` can scale), whether scaling is
/// needed at all, and what the encoder accepts.
pub(crate) fn plan(request: &ChainRequest) -> Result<Vec<FilterStep>, String> {
    if !request.source.is_supported_by(request.backend) {
        return Err(
            "capturing a single window needs the gfxcapture backend; \
             ddagrab can only capture a whole display"
                .to_owned(),
        );
    }

    let scaling = request.target_size != request.source_size;

    // `gfxcapture` scales on the GPU inside the source filter, so it never
    // needs a conversion stage for size.  `ddagrab` cannot scale at all -
    // only crop - and the D3D11 scaling filter is unusable on this hardware
    // (`TODO.md` 0.7), so its only option is a CPU round trip.
    let (source_canvas, cpu_scale) = match (request.backend, scaling) {
        (CaptureBackend::Gfxcapture, true) => (Some(request.target_size), None),
        (CaptureBackend::Ddagrab, true) => (None, Some(request.target_size)),
        (_, false) => (None, None),
    };

    let mut steps = vec![source_step(
        request.source,
        request.backend,
        request.fps,
        request.draw_cursor,
        source_canvas,
    )];
    steps.extend(conversion_steps(request.input, cpu_scale));
    Ok(steps)
}

/// Build the capture filter and its options.
///
/// `canvas` asks `gfxcapture` to scale on the GPU; it is ignored for
/// `ddagrab`, which cannot.
fn source_step(
    source: CaptureSource,
    backend: CaptureBackend,
    fps: u32,
    draw_cursor: bool,
    canvas: Option<OutputSize>,
) -> FilterStep {
    let cursor = if draw_cursor { "1" } else { "0" };
    match backend {
        CaptureBackend::Ddagrab => {
            let output_index = match source {
                CaptureSource::Monitor { output_index, .. } => output_index,
                // Rejected before we get here; index 0 keeps this total.
                CaptureSource::Window { .. } => 0,
            };
            FilterStep::d3d11(
                "ddagrab",
                vec![
                    ("output_idx", output_index.to_string()),
                    ("framerate", fps.to_string()),
                    ("draw_mouse", cursor.to_owned()),
                    // Repeat the last frame while the desktop is idle, which
                    // gives this backend a real frame pace of its own.
                    ("dup_frames", "1".to_owned()),
                ],
            )
        }
        CaptureBackend::Gfxcapture => {
            let mut options = match source {
                CaptureSource::Monitor { hmonitor, .. } => {
                    vec![("hmonitor", hmonitor.to_string())]
                }
                CaptureSource::Window { hwnd } => vec![("hwnd", hwnd.to_string())],
            };
            options.push(("max_framerate", fps.to_string()));
            options.push(("capture_cursor", cursor.to_owned()));
            // Windows Graphics Capture draws a highlight border around the
            // captured surface by default; the filter's default is off, but
            // being explicit means a future default change cannot put a
            // yellow rectangle into everyone's stream.
            options.push(("display_border", "0".to_owned()));
            if let Some(size) = canvas {
                options.push(("width", size.width.to_string()));
                options.push(("height", size.height.to_string()));
                // `scale`, not `scale_aspect`: the target size already has
                // the source's aspect ratio (it was derived from it), and
                // `scale_aspect` pads to the canvas with black bars, which
                // would letterbox away the pixels we just asked for.
                options.push(("resize_mode", "scale".to_owned()));
                options.push(("scale_mode", "bilinear".to_owned()));
            }
            FilterStep::d3d11("gfxcapture", options)
        }
    }
}

/// The steps between capture and the encoder.
///
/// Empty for the fast path - a D3D11 encoder taking the BGRA hardware
/// frames unchanged.  Otherwise the frames come down to system memory,
/// because there is no working GPU scaler or GPU format converter in this
/// build (`TODO.md` 0.7), and go back up only if the encoder needs them to.
fn conversion_steps(input: EncoderInput, scale_to: Option<OutputSize>) -> Vec<FilterStep> {
    if scale_to.is_none() && input == EncoderInput::HardwareD3d11 {
        return Vec::new();
    }

    let mut steps = vec![
        FilterStep::plain("hwdownload", Vec::new()),
        // `hwdownload` emits the frames context's software format, which is
        // BGRA here; naming it explicitly keeps the next filter's input
        // unambiguous.
        FilterStep::plain("format", vec![("pix_fmts", "bgra".to_owned())]),
    ];

    if let Some(size) = scale_to {
        steps.push(FilterStep::plain(
            "scale",
            vec![("w", size.width.to_string()), ("h", size.height.to_string())],
        ));
    }

    if let Some(format) = input.target_format() {
        steps.push(FilterStep::plain("format", vec![("pix_fmts", format.to_owned())]));
    }

    match input.upload_target() {
        Some(DeviceSlot::D3d11) => {
            steps.push(FilterStep::d3d11("hwupload", Vec::new()));
        }
        Some(DeviceSlot::D3d12) => {
            steps.push(FilterStep {
                name: "hwupload",
                options: Vec::new(),
                device: Some(DeviceSlot::D3d12),
            });
        }
        None => {}
    }

    steps
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "acceptable in test code")]

    use super::*;

    const HD: OutputSize = OutputSize { width: 1920, height: 1080 };
    const QHD: OutputSize = OutputSize { width: 2560, height: 1440 };

    const MONITOR: CaptureSource = CaptureSource::Monitor { output_index: 0, hmonitor: 0x1234 };
    #[test]
    fn picker_handles_preserve_all_bits_and_accept_legacy_numbers() -> Result<(), serde_json::Error>
    {
        let source: CaptureSource =
            serde_json::from_str(r#"{"kind":"window","hwnd":"18446744073709551615"}"#)?;
        assert_eq!(source, CaptureSource::Window { hwnd: u64::MAX });
        let legacy: CaptureSource = serde_json::from_str(r#"{"kind":"window","hwnd":1234}"#)?;
        assert_eq!(legacy, CaptureSource::Window { hwnd: 1234 });
        assert!(serde_json::from_str::<CaptureSource>(r#"{"kind":"window","hwnd":"-1"}"#).is_err());
        Ok(())
    }

    const WINDOW: CaptureSource = CaptureSource::Window { hwnd: 0xABCD };

    fn request(backend: CaptureBackend, input: EncoderInput, target: OutputSize) -> ChainRequest {
        ChainRequest {
            source: MONITOR,
            backend,
            source_size: QHD,
            target_size: target,
            fps: 30,
            input,
            draw_cursor: true,
        }
    }

    fn names(steps: &[FilterStep]) -> Vec<&str> {
        steps.iter().map(|s| s.name).collect()
    }

    fn option<'a>(step: &'a FilterStep, key: &str) -> Option<&'a str> {
        step.options.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn hardware_path_has_no_conversion_at_all() {
        let steps =
            plan(&request(CaptureBackend::Ddagrab, EncoderInput::HardwareD3d11, QHD)).unwrap();
        assert_eq!(names(&steps), ["ddagrab"], "BGRA D3D11 frames go straight to the encoder");
    }

    #[test]
    fn gfxcapture_scales_inside_the_source_filter() {
        let steps =
            plan(&request(CaptureBackend::Gfxcapture, EncoderInput::HardwareD3d11, HD)).unwrap();
        assert_eq!(names(&steps), ["gfxcapture"], "GPU scaling needs no extra filter");
        assert_eq!(option(&steps[0], "width"), Some("1920"));
        assert_eq!(option(&steps[0], "height"), Some("1080"));
        assert_eq!(option(&steps[0], "resize_mode"), Some("scale"));
    }

    #[test]
    fn ddagrab_scales_through_a_cpu_round_trip() {
        let steps =
            plan(&request(CaptureBackend::Ddagrab, EncoderInput::HardwareD3d11, HD)).unwrap();
        assert_eq!(
            names(&steps),
            ["ddagrab", "hwdownload", "format", "scale", "hwupload"],
            "ddagrab cannot scale, and no GPU scaler is usable"
        );
        let scale = &steps[3];
        assert_eq!(option(scale, "w"), Some("1920"));
        assert_eq!(option(scale, "h"), Some("1080"));
        assert_eq!(steps[4].device, Some(DeviceSlot::D3d11));
    }

    #[test]
    fn software_encoders_end_in_system_memory() {
        let nv12 =
            plan(&request(CaptureBackend::Ddagrab, EncoderInput::SysMemNv12, QHD)).unwrap();
        assert_eq!(names(&nv12), ["ddagrab", "hwdownload", "format", "format"]);
        assert_eq!(option(&nv12[3], "pix_fmts"), Some("nv12"));

        let yuv =
            plan(&request(CaptureBackend::Ddagrab, EncoderInput::SysMemYuv420p, QHD)).unwrap();
        assert_eq!(option(&yuv[3], "pix_fmts"), Some("yuv420p"));
        assert!(yuv.iter().all(|s| s.name != "hwupload"), "no upload for a software encoder");
    }

    #[test]
    fn d3d12_encoder_uploads_nv12_to_its_own_device() {
        let steps =
            plan(&request(CaptureBackend::Gfxcapture, EncoderInput::HardwareD3d12, QHD)).unwrap();
        assert_eq!(names(&steps), ["gfxcapture", "hwdownload", "format", "format", "hwupload"]);
        assert_eq!(option(&steps[3], "pix_fmts"), Some("nv12"));
        assert_eq!(
            steps[4].device,
            Some(DeviceSlot::D3d12),
            "the upload has to target the D3D12 device, not the capture one"
        );
    }

    #[test]
    fn window_capture_requires_gfxcapture() {
        let mut req = request(CaptureBackend::Ddagrab, EncoderInput::HardwareD3d11, QHD);
        req.source = WINDOW;
        assert!(plan(&req).is_err());

        req.backend = CaptureBackend::Gfxcapture;
        let steps = plan(&req).unwrap();
        assert_eq!(option(&steps[0], "hwnd"), Some("43981"), "0xABCD as decimal");
    }

    #[test]
    fn monitors_are_addressed_per_backend() {
        // The two filters name the same display differently, and getting this
        // wrong captures the wrong screen rather than failing.
        let dda = plan(&request(CaptureBackend::Ddagrab, EncoderInput::HardwareD3d11, QHD)).unwrap();
        assert_eq!(option(&dda[0], "output_idx"), Some("0"));
        assert_eq!(option(&dda[0], "dup_frames"), Some("1"), "self-paced repeat frames");

        let gfx =
            plan(&request(CaptureBackend::Gfxcapture, EncoderInput::HardwareD3d11, QHD)).unwrap();
        assert_eq!(option(&gfx[0], "hmonitor"), Some("4660"), "0x1234 as decimal");

        // Both must use our device; a source that makes its own would put the
        // frames on a different D3D11 device than the encoder.
        assert_eq!(dda[0].device, Some(DeviceSlot::D3d11));
        assert_eq!(gfx[0].device, Some(DeviceSlot::D3d11));
    }

    #[test]
    fn encoder_input_matches_the_measured_table() {
        for (id, expected) in [
            ("h264_nvenc", EncoderInput::HardwareD3d11),
            ("hevc_nvenc", EncoderInput::HardwareD3d11),
            ("av1_amf", EncoderInput::HardwareD3d11),
            ("h264_d3d12va", EncoderInput::HardwareD3d12),
            ("h264_qsv", EncoderInput::SysMemNv12),
            ("h264_mf", EncoderInput::SysMemNv12),
            ("libopenh264", EncoderInput::SysMemYuv420p),
        ] {
            assert_eq!(EncoderInput::for_encoder(id), expected, "{id}");
        }
    }
}
