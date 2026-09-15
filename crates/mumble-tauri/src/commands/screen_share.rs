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
use crate::media::transport::{Allocator, SfuReason};

/// Negotiate or close the local-only preview of an existing broadcast.
#[tauri::command]
pub(crate) fn native_screen_share_preview(
    state: tauri::State<'_, crate::state::AppState>,
    server_id: String,
    broadcast_id: String,
    preview_id: String,
    action: String,
    payload: Option<String>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        use crate::media::broadcast::PreviewAction;
        let _ = uuid::Uuid::parse_str(&preview_id).map_err(|_| "Invalid preview id")?;
        let payload = payload.unwrap_or_default();
        if payload.len() > 128 * 1024 {
            return Err("Preview signal too large".to_owned());
        }
        let action = match action.as_str() {
            "start" => PreviewAction::Start(channel),
            "answer" => PreviewAction::Answer(payload),
            "ice" => PreviewAction::Ice(payload),
            "stop" => PreviewAction::Stop,
            _ => return Err("Invalid preview action".to_owned()),
        };
        state.native_screen_share_preview(&server_id, &broadcast_id, preview_id, action)
    }
    #[cfg(target_os = "android")]
    {
        let _ = (
            state,
            server_id,
            broadcast_id,
            preview_id,
            action,
            payload,
            channel,
        );
        Err("Native preview is unavailable on Android".to_owned())
    }
}

/// Cheap capability check without running encoder probes.
#[tauri::command]
pub(crate) fn native_screen_share_available() -> bool {
    cfg!(all(target_os = "windows", feature = "native-screenshare"))
}

/// Validate persisted defaults through the same model used when starting capture.
#[tauri::command]
pub(crate) fn validate_screen_share_settings(
    settings: ScreenShareSettings,
) -> Result<ScreenShareSettings, String> {
    settings.validate()?;
    Ok(settings)
}

/// Enumerate native sources without starting capture.
#[tauri::command]
pub(crate) async fn list_screen_share_sources() -> Result<serde_json::Value, String> {
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    {
        tokio::task::spawn_blocking(|| {
            serde_json::to_value(crate::media::sources::enumerate()?).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("source enumeration failed: {e}"))?
    }
    #[cfg(not(all(target_os = "windows", feature = "native-screenshare")))]
    {
        Err("native screen sharing is unavailable in this build".to_owned())
    }
}

/// Start native capture and WebRTC transport on the selected server.
#[tauri::command]
pub(crate) async fn start_native_screen_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::state::AppState>,
    request: serde_json::Value,
    server_id: Option<String>,
) -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "android"))]
    {
        let request = serde_json::from_value(request)
            .map_err(|e| format!("invalid screen-share request: {e}"))?;
        let status = state
            .start_native_screen_share(app, request, server_id)
            .await?;
        serde_json::to_value(status).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "android")]
    {
        let _ = (app, state, request, server_id);
        Err("native screen sharing is unavailable on Android".to_owned())
    }
}

/// Stop native capture and wait for its resources to be released.
#[tauri::command]
pub(crate) async fn stop_native_screen_share(
    state: tauri::State<'_, crate::state::AppState>,
    server_id: Option<String>,
) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        state.stop_native_screen_share(server_id).await
    }
    #[cfg(target_os = "android")]
    {
        let _ = (state, server_id);
        Ok(())
    }
}

/// Read native broadcast status without depending on the current UI tab.
#[tauri::command]
pub(crate) fn native_screen_share_status(
    state: tauri::State<'_, crate::state::AppState>,
    server_id: Option<String>,
) -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "android"))]
    {
        serde_json::to_value(state.native_screen_share_status(server_id)?)
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "android")]
    {
        let _ = (state, server_id);
        Ok(serde_json::Value::Null)
    }
}

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
pub(crate) fn suggested_screen_share_bitrate(width: u32, height: u32, fps: u32) -> u32 {
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
    /// How viewers will be served.
    pub(crate) transport: TransportPlan,
}

/// How this broadcast will reach its viewers.
///
/// Resolved up front so the UI can be honest before anything starts: telling
/// a user "direct connections for the first 2 viewers" and then silently
/// serving everyone through the SFU - because their server does not relay for
/// it - would be worse than saying so.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransportPlan {
    /// How many viewers will be offered a direct connection.  Zero means
    /// every viewer goes through the SFU.
    pub(crate) direct_slots: u32,
    /// Why no viewer will be direct, when `direct_slots` is zero.
    pub(crate) sfu_only_reason: Option<SfuReason>,
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
    state: tauri::State<'_, crate::state::AppState>,
    settings: ScreenShareSettings,
    source_width: u32,
    source_height: u32,
) -> Result<ResolvedScreenShare, String> {
    let encoding = settings.resolve(OutputSize {
        width: source_width,
        height: source_height,
    })?;

    let transport = plan_transport(&settings, &state);

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

    Ok(ResolvedScreenShare {
        encoding,
        encoder,
        transport,
    })
}

/// Work out how viewers will be served, given the settings and the server.
///
/// A server that does not advertise `webrtc_p2p_relay_available` cannot carry
/// the P2P signal types at all, so its answer overrides whatever the user
/// asked for (see `TODO.md` 3.0).
fn plan_transport(settings: &ScreenShareSettings, state: &crate::state::AppState) -> TransportPlan {
    let allocator = Allocator::new(settings, state.server_relays_p2p());
    TransportPlan {
        direct_slots: allocator.free_slots(),
        sfu_only_reason: allocator.sfu_only_reason(),
    }
}
