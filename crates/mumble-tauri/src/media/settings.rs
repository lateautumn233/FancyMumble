//! User-facing screen-share encoding settings.
//!
//! This is the shared model referenced by `TODO.md` stage 1.0: the
//! frontend persists these values through `tauri-plugin-store`, and the
//! Rust side owns validation and normalisation so both ends agree on what
//! a given combination actually means.
//!
//! Settings are read once when a broadcast starts.  Changing them mid-way
//! requires restarting the broadcast, because a resolution change forces a
//! new SDP negotiation.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Lowest frame rate the encoder pipeline accepts.
pub(crate) const MIN_FPS: u32 = 5;
/// Highest frame rate the encoder pipeline accepts.
pub(crate) const MAX_FPS: u32 = 120;
/// Lowest bit rate, in kbps.  Below this H.264 screen content falls apart.
pub(crate) const MIN_BITRATE_KBPS: u32 = 500;
/// Highest bit rate, in kbps.
pub(crate) const MAX_BITRATE_KBPS: u32 = 50_000;
/// Smallest custom output edge, in pixels.
pub(crate) const MIN_EDGE: u32 = 16;
/// Largest custom output edge, in pixels.  Matches the practical limit of
/// the Windows hardware encoders we target.
pub(crate) const MAX_EDGE: u32 = 8192;

/// Bits per pixel used to estimate a bit rate for screen content.
///
/// Desktop content is mostly static and compresses far better than camera
/// video, so this sits well below the ~0.1 bpp rule of thumb for live
/// action.  At 1080p30 it suggests roughly 4.3 Mbps.
const SCREEN_BITS_PER_PIXEL: f64 = 0.07;

/// Which native capture backend feeds the encoder.
///
/// Both are `FFmpeg` filters that emit `AV_PIX_FMT_D3D11` frames on the
/// device we hand them, so the encoder stage is identical for both.  No
/// pixel-format conversion happens in between: `nvenc` and `amf` take the
/// BGRA hardware frames directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CaptureBackend {
    /// `FFmpeg`'s `ddagrab` filter (DXGI Desktop Duplication).  Whole
    /// monitors only and it cannot scale, but it paces frames itself and
    /// repeats the last one while the desktop is idle.
    #[default]
    Ddagrab,
    /// `FFmpeg`'s `gfxcapture` filter (Windows Graphics Capture, new in
    /// `FFmpeg` 8.1).  Can target a single window and scales on the GPU,
    /// but only emits a frame when the content changes, so the pipeline
    /// supplies its own frame clock.
    Gfxcapture,
}


/// Which encoder to use.
///
/// Serialises as a plain string - `"auto"` or an `FFmpeg` encoder name such
/// as `"h264_nvenc"` - so the frontend can bind it straight to a `<select>`
/// value without unwrapping a tagged union.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum EncoderChoice {
    /// Pick the first hardware encoder that passes probing.  The default.
    #[default]
    Auto,
    /// Use this specific `FFmpeg` encoder, falling back to [`Self::Auto`]
    /// when it is not available on this machine.
    Id(String),
}

/// The string the wire format uses for [`EncoderChoice::Auto`].
const AUTO: &str = "auto";

impl EncoderChoice {
    /// The `FFmpeg` encoder name, or `None` when set to automatic.
    pub(crate) fn id(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Id(id) => Some(id),
        }
    }
}

impl Serialize for EncoderChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id().unwrap_or(AUTO))
    }
}

impl<'de> Deserialize<'de> for EncoderChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw.is_empty() || raw.eq_ignore_ascii_case(AUTO) {
            return Ok(Self::Auto);
        }
        Ok(Self::Id(raw))
    }
}

/// A validated, encoder-safe output size in pixels.
///
/// Both edges are even, which 4:2:0 chroma subsampling requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutputSize {
    /// Output width in pixels; always even.
    pub(crate) width: u32,
    /// Output height in pixels; always even.
    pub(crate) height: u32,
}

/// Round down to an even number, never below 2.
fn even_floor(value: u32) -> u32 {
    value.max(2) & !1
}

impl OutputSize {
    /// Scale `source` down to fit inside a `max_width` x `max_height` box,
    /// preserving aspect ratio, then round both edges to even numbers.
    ///
    /// Never upscales: a source already inside the box keeps its size.
    ///
    /// Deliberately integer arithmetic.  The float version of this rounds
    /// an exact fit - 3840x2160 into a 1920x1080 box - down to 1918x1078
    /// whenever the division lands a hair below the true ratio.
    fn fit(source: Self, max_width: u32, max_height: u32) -> Self {
        let src_w = u64::from(source.width.max(1));
        let src_h = u64::from(source.height.max(1));
        let box_w = u64::from(max_width);
        let box_h = u64::from(max_height);

        // Already inside the box: keep it, only evening the edges.
        if src_w <= box_w && src_h <= box_h {
            return Self {
                width: even_floor(source.width),
                height: even_floor(source.height),
            };
        }

        // Whichever edge is proportionally tighter decides the scale; the
        // other is derived from it so the aspect ratio is preserved.
        let width_binds = box_w * src_h <= box_h * src_w;
        let (width, height) = if width_binds {
            (box_w, box_w * src_h / src_w)
        } else {
            (box_h * src_w / src_h, box_h)
        };

        Self {
            width: even_floor(u32::try_from(width).unwrap_or(u32::MAX)),
            height: even_floor(u32::try_from(height).unwrap_or(u32::MAX)),
        }
    }
}

/// Target output resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub(crate) enum Resolution {
    /// Encode at the capture source's own resolution.  The default.
    #[default]
    Native,
    /// Fit inside one of the standard 16:9 boxes.
    Preset {
        /// The preset's short-edge budget: 2160, 1440, 1080 or 720.
        lines: u32,
    },
    /// Fit inside a user-supplied box.
    #[serde(rename_all = "camelCase")]
    Custom {
        /// Bounding-box width in pixels.
        width: u32,
        /// Bounding-box height in pixels.
        height: u32,
    },
}

/// The preset line counts the settings UI offers.
pub(crate) const PRESET_LINES: [u32; 4] = [2160, 1440, 1080, 720];

impl Resolution {
    /// Resolve to a concrete output size for a given capture source size.
    ///
    /// Presets are treated as 16:9 bounding boxes, so the label stays
    /// honest on non-16:9 sources: `Preset { lines: 1080 }` on a 1920x1200
    /// monitor yields 1728x1080, not 1920x1200 scaled by pixel count.
    pub(crate) fn resolve(self, source: OutputSize) -> OutputSize {
        match self {
            Self::Native => OutputSize {
                width: even_floor(source.width),
                height: even_floor(source.height),
            },
            Self::Preset { lines } => {
                OutputSize::fit(source, lines.saturating_mul(16) / 9, lines)
            }
            Self::Custom { width, height } => OutputSize::fit(source, width, height),
        }
    }

    /// Reject presets outside [`PRESET_LINES`] and out-of-range custom sizes.
    fn validate(self) -> Result<(), String> {
        match self {
            Self::Native => Ok(()),
            Self::Preset { lines } if PRESET_LINES.contains(&lines) => Ok(()),
            Self::Preset { lines } => {
                Err(format!("unsupported resolution preset {lines}p"))
            }
            Self::Custom { width, height } => validate_custom_size(width, height),
        }
    }
}

/// Check a custom width/height pair against the supported edge range.
fn validate_custom_size(width: u32, height: u32) -> Result<(), String> {
    for (label, edge) in [("width", width), ("height", height)] {
        if !(MIN_EDGE..=MAX_EDGE).contains(&edge) {
            return Err(format!(
                "custom {label} {edge} is outside {MIN_EDGE}..={MAX_EDGE}"
            ));
        }
    }
    Ok(())
}

/// Estimate a bit rate in kbps for the given output size and frame rate.
///
/// Used both as the automatic default and as the reference figure the
/// settings UI shows next to the manual bit-rate input.  The result is
/// clamped to [`MIN_BITRATE_KBPS`]..=[`MAX_BITRATE_KBPS`].
pub(crate) fn suggested_bitrate_kbps(size: OutputSize, fps: u32) -> u32 {
    let pixels = f64::from(size.width) * f64::from(size.height);
    let bits_per_second = pixels * f64::from(fps) * SCREEN_BITS_PER_PIXEL;
    let kbps = (bits_per_second / 1000.0) as u32;
    kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
}

/// Default frame rate: a compromise between smoothness and bit rate.
const DEFAULT_FPS: u32 = 30;

/// Highest number of direct viewers the settings UI allows.
///
/// Each one costs a full copy of the stream on the broadcaster's uplink, so
/// this is a bandwidth ceiling rather than a technical limit.
pub(crate) const MAX_P2P_VIEWERS: u32 = 8;

/// How many viewers get a direct connection by default.
///
/// Two keeps the uplink cost modest while covering the common case of one or
/// two people watching, where avoiding the server round trip is most
/// noticeable.
const DEFAULT_P2P_MAX_VIEWERS: u32 = 2;

/// How a broadcast reaches its viewers.
///
/// The two transports coexist within one broadcast: the first
/// [`ScreenShareSettings::p2p_max_viewers`] viewers are offered a direct
/// connection and everyone after that goes through the server's SFU.  The
/// encoder runs once either way - the pipeline's frame sink fans out - so the
/// cost of a direct viewer is uplink bandwidth, not CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum P2pMode {
    /// Offer direct connections up to the viewer cap, then fall back to the
    /// SFU.  Also falls back for any viewer whose direct connection cannot be
    /// established - without a TURN server, a symmetric NAT on either side
    /// defeats it.
    #[default]
    Auto,
    /// Never offer a direct connection; every viewer goes through the SFU.
    /// Broadcaster uplink is then O(1) regardless of audience size.
    Disabled,
}

/// The complete set of user-configurable screen-share encoding settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ScreenShareSettings {
    /// Which capture backend to use.
    pub(crate) capture: CaptureBackend,
    /// Which encoder to use, or automatic selection.
    pub(crate) encoder: EncoderChoice,
    /// Target output resolution.
    pub(crate) resolution: Resolution,
    /// Target frame rate in frames per second.
    pub(crate) fps: u32,
    /// Target bit rate in kbps.  `None` means derive it from the resolved
    /// resolution and frame rate via [`suggested_bitrate_kbps`].
    pub(crate) bitrate_kbps: Option<u32>,
    /// How a broadcast reaches its viewers.  See [`P2pMode`].
    pub(crate) p2p: P2pMode,
    /// How many viewers, at most, are offered a direct connection.
    ///
    /// Only consulted when [`Self::p2p`] is [`P2pMode::Auto`].  Capped at
    /// [`MAX_P2P_VIEWERS`].  A value of 0 is equivalent to
    /// [`P2pMode::Disabled`].
    pub(crate) p2p_max_viewers: u32,
}

impl Default for ScreenShareSettings {
    fn default() -> Self {
        Self {
            capture: CaptureBackend::default(),
            encoder: EncoderChoice::default(),
            resolution: Resolution::default(),
            fps: DEFAULT_FPS,
            bitrate_kbps: None,
            p2p: P2pMode::default(),
            p2p_max_viewers: DEFAULT_P2P_MAX_VIEWERS,
        }
    }
}

/// The concrete encoding parameters handed to the pipeline, after
/// validating the settings and resolving them against a capture source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedEncoding {
    /// Encoder input size after scaling; both edges even.
    pub(crate) size: OutputSize,
    /// Frame rate handed to the encoder and the capture pacer.
    pub(crate) fps: u32,
    /// Bit rate handed to the encoder, in kbps.
    pub(crate) bitrate_kbps: u32,
}

impl ScreenShareSettings {
    /// Reject settings the pipeline cannot honour.
    ///
    /// Encoder availability is *not* checked here - that needs probe
    /// results, which live in [`super::encoder`].
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(MIN_FPS..=MAX_FPS).contains(&self.fps) {
            return Err(format!(
                "fps {} is outside {MIN_FPS}..={MAX_FPS}",
                self.fps
            ));
        }
        if let Some(kbps) = self.bitrate_kbps {
            if !(MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS).contains(&kbps) {
                return Err(format!(
                    "bitrate {kbps} kbps is outside \
                     {MIN_BITRATE_KBPS}..={MAX_BITRATE_KBPS}"
                ));
            }
        }
        if self.p2p_max_viewers > MAX_P2P_VIEWERS {
            return Err(format!(
                "p2p viewer cap {} is above {MAX_P2P_VIEWERS}",
                self.p2p_max_viewers
            ));
        }
        self.resolution.validate()
    }

    /// How many viewers may be given a direct connection.
    ///
    /// Collapses the two ways of saying "none" - [`P2pMode::Disabled`] and a
    /// cap of zero - so callers have a single number to reason about.  The
    /// server's own support is a separate condition and is checked where the
    /// signalling happens.
    pub(crate) fn p2p_slots(&self) -> u32 {
        match self.p2p {
            P2pMode::Disabled => 0,
            P2pMode::Auto => self.p2p_max_viewers.min(MAX_P2P_VIEWERS),
        }
    }

    /// Validate, then resolve against the capture source's own size.
    pub(crate) fn resolve(&self, source: OutputSize) -> Result<ResolvedEncoding, String> {
        self.validate()?;
        let size = self.resolution.resolve(source);
        Ok(ResolvedEncoding {
            size,
            fps: self.fps,
            bitrate_kbps: self
                .bitrate_kbps
                .unwrap_or_else(|| suggested_bitrate_kbps(size, self.fps)),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "acceptable in test code")]

    use super::*;

    const UHD: OutputSize = OutputSize { width: 3840, height: 2160 };
    const WIDE: OutputSize = OutputSize { width: 1920, height: 1200 };

    #[test]
    fn native_keeps_source_size_but_evens_it() {
        let odd = OutputSize { width: 1367, height: 769 };
        assert_eq!(
            Resolution::Native.resolve(odd),
            OutputSize { width: 1366, height: 768 }
        );
    }

    #[test]
    fn preset_fits_inside_sixteen_by_nine_box() {
        assert_eq!(
            Resolution::Preset { lines: 1080 }.resolve(UHD),
            OutputSize { width: 1920, height: 1080 }
        );
        // 16:10 source: height is honoured, width shrinks with it.
        assert_eq!(
            Resolution::Preset { lines: 1080 }.resolve(WIDE),
            OutputSize { width: 1728, height: 1080 }
        );
    }

    #[test]
    fn preset_never_upscales() {
        let small = OutputSize { width: 1280, height: 720 };
        assert_eq!(Resolution::Preset { lines: 2160 }.resolve(small), small);
    }

    #[test]
    fn custom_preserves_aspect_ratio() {
        let res = Resolution::Custom { width: 1000, height: 1000 };
        assert_eq!(res.resolve(UHD), OutputSize { width: 1000, height: 562 });
    }

    #[test]
    fn portrait_source_survives_preset() {
        let portrait = OutputSize { width: 1080, height: 1920 };
        let out = Resolution::Preset { lines: 1080 }.resolve(portrait);
        assert!(out.width <= 1920 && out.height <= 1080);
        assert_eq!(out.width % 2, 0);
        assert_eq!(out.height % 2, 0);
    }

    #[test]
    fn validate_rejects_out_of_range_values() {
        let bad_fps = ScreenShareSettings { fps: 0, ..Default::default() };
        assert!(bad_fps.validate().is_err());

        let bad_rate =
            ScreenShareSettings { bitrate_kbps: Some(10), ..Default::default() };
        assert!(bad_rate.validate().is_err());

        let bad_preset = ScreenShareSettings {
            resolution: Resolution::Preset { lines: 999 },
            ..Default::default()
        };
        assert!(bad_preset.validate().is_err());

        let bad_custom = ScreenShareSettings {
            resolution: Resolution::Custom { width: 4, height: 4 },
            ..Default::default()
        };
        assert!(bad_custom.validate().is_err());
    }

    #[test]
    fn defaults_are_valid_and_resolve() {
        let settings = ScreenShareSettings::default();
        let resolved = settings.resolve(UHD).unwrap();
        assert_eq!(resolved.size, UHD);
        assert_eq!(resolved.fps, 30);
        // Auto bit rate: clamped to the ceiling at 4K30.
        assert_eq!(resolved.bitrate_kbps, MAX_BITRATE_KBPS.min(17418));
    }

    #[test]
    fn explicit_bitrate_wins_over_estimate() {
        let settings =
            ScreenShareSettings { bitrate_kbps: Some(2500), ..Default::default() };
        assert_eq!(settings.resolve(UHD).unwrap().bitrate_kbps, 2500);
    }

    #[test]
    fn suggested_bitrate_is_clamped_both_ways() {
        let tiny = OutputSize { width: 160, height: 120 };
        assert_eq!(suggested_bitrate_kbps(tiny, 5), MIN_BITRATE_KBPS);
        let huge = OutputSize { width: 7680, height: 4320 };
        assert_eq!(suggested_bitrate_kbps(huge, 120), MAX_BITRATE_KBPS);
    }

    #[test]
    fn encoder_choice_round_trips_as_plain_string() {
        assert_eq!(serde_json::to_string(&EncoderChoice::Auto).unwrap(), "\"auto\"");
        let named = EncoderChoice::Id("h264_nvenc".to_owned());
        assert_eq!(serde_json::to_string(&named).unwrap(), "\"h264_nvenc\"");
        assert_eq!(
            serde_json::from_str::<EncoderChoice>("\"AUTO\"").unwrap(),
            EncoderChoice::Auto
        );
        assert_eq!(serde_json::from_str::<EncoderChoice>("\"\"").unwrap(), EncoderChoice::Auto);
        assert_eq!(serde_json::from_str::<EncoderChoice>("\"h264_mf\"").unwrap(), named_mf());
    }

    fn named_mf() -> EncoderChoice {
        EncoderChoice::Id("h264_mf".to_owned())
    }

    #[test]
    fn settings_wire_format_is_camel_case() {
        let json = serde_json::to_string(&ScreenShareSettings::default()).unwrap();
        assert!(json.contains("\"bitrateKbps\""), "{json}");
        assert!(json.contains("\"capture\":\"ddagrab\""), "{json}");
        assert!(json.contains("\"mode\":\"native\""), "{json}");
    }

    #[test]
    fn p2p_slots_collapses_every_way_of_saying_none() {
        let auto = ScreenShareSettings {
            p2p: P2pMode::Auto,
            p2p_max_viewers: 3,
            ..Default::default()
        };
        assert_eq!(auto.p2p_slots(), 3);

        // Disabled wins over whatever the cap says.
        let disabled = ScreenShareSettings { p2p: P2pMode::Disabled, ..auto.clone() };
        assert_eq!(disabled.p2p_slots(), 0);

        // A cap of zero means the same thing as disabling it.
        let zero = ScreenShareSettings { p2p_max_viewers: 0, ..auto.clone() };
        assert_eq!(zero.p2p_slots(), 0);

        // Stored settings from a future build with a higher ceiling must not
        // hand out more slots than this build is prepared to serve.
        let over = ScreenShareSettings { p2p_max_viewers: 999, ..auto };
        assert_eq!(over.p2p_slots(), MAX_P2P_VIEWERS);
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let settings: ScreenShareSettings =
            serde_json::from_str(r#"{"fps":60}"#).unwrap();
        assert_eq!(settings.fps, 60);
        assert_eq!(settings.capture, CaptureBackend::Ddagrab);
        assert_eq!(settings.encoder, EncoderChoice::Auto);
    }
}
