//! Screen-share encoder enumeration and settings validation.
//!
//! Stage 1 of the native screen-share work (see `TODO.md`): tell the
//! frontend which encoders actually work on this machine, and turn a set of
//! user settings into the concrete parameters the capture and encode
//! pipeline will use.
//!
//! These commands exist on every platform.  Where native encoding is not
//! available they report it rather than failing, so the settings page needs
//! no platform branches.

use crate::media::encoder::{self, EncoderReport, Selection};
use crate::media::settings::{OutputSize, ResolvedEncoding, ScreenShareSettings};

/// List the video encoders on this machine, with availability.
///
/// Covers H.264, HEVC and AV1.  Only H.264 is auto-selected; the others are
/// listed so a user can opt into them deliberately.
///
/// Probing happens in a subprocess and its results are cached per GPU
/// signature, so the first call after a driver change costs a second or two
/// and later calls are nearly free.  Pass `refresh: true` to force a
/// re-probe (the settings page offers this as a "re-detect" button).
#[tauri::command]
pub(crate) async fn list_video_encoders(
    app: tauri::AppHandle,
    refresh: Option<bool>,
) -> Result<EncoderReport, String> {
    let data_dir = crate::e2e_data_dir(&app)?;
    let refresh = refresh.unwrap_or(false);

    tokio::task::spawn_blocking(move || encoder::report(&data_dir, refresh))
        .await
        .map_err(|e| format!("encoder probe task failed: {e}"))
}

/// The default screen-share settings, so defaults live in one place.
#[tauri::command]
pub(crate) fn default_screen_share_settings() -> ScreenShareSettings {
    ScreenShareSettings::default()
}

/// The automatic bit rate estimate for a given size and frame rate.
///
/// Shown next to the manual bit-rate input as a reference figure.
#[tauri::command]
pub(crate) fn suggested_screen_share_bitrate(
    width: u32,
    height: u32,
    fps: u32,
) -> u32 {
    crate::media::settings::suggested_bitrate_kbps(OutputSize { width, height }, fps)
}

/// What the pipeline will actually do with a set of settings.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedScreenShare {
    /// Size, frame rate and bit rate the encoder will be configured with.
    pub(crate) encoding: ResolvedEncoding,
    /// The encoder that will be opened, and whether the user's explicit
    /// choice had to be dropped to get there.
    pub(crate) encoder: Selection,
}

/// Validate settings and resolve them against a capture source.
///
/// `sourceWidth` / `sourceHeight` are the capture source's native size,
/// which decides what "native" resolution means and how presets scale.
/// Returns an error for settings the pipeline cannot honour - an
/// out-of-range frame rate or bit rate, an unknown preset, or the case where
/// no working encoder exists at all.
#[tauri::command]
pub(crate) async fn resolve_screen_share_encoding(
    app: tauri::AppHandle,
    settings: ScreenShareSettings,
    source_width: u32,
    source_height: u32,
) -> Result<ResolvedScreenShare, String> {
    let encoding = settings.resolve(OutputSize {
        width: source_width,
        height: source_height,
    })?;

    let data_dir = crate::e2e_data_dir(&app)?;
    let report = tokio::task::spawn_blocking(move || encoder::report(&data_dir, false))
        .await
        .map_err(|e| format!("encoder probe task failed: {e}"))?;

    if !report.supported {
        return Err("native screen-share encoding is not available in this build".to_owned());
    }

    let encoder = report.resolve_choice(&settings.encoder)?;
    if encoder.fell_back {
        tracing::info!(
            "screen-share encoder {:?} unavailable, using {}",
            settings.encoder,
            encoder.id
        );
    }

    Ok(ResolvedScreenShare { encoding, encoder })
}
