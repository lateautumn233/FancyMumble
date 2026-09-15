//! Native broadcasts pinned to their original server connection and channel.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mumble_protocol::proto::mumble_tcp::{self, web_rtc_signal::SignalType};

use super::{types::ConnectionStatus, AppState, SharedState};
use crate::media::broadcast::{self, Context, Event, StartRequest, Status};

impl AppState {
    pub(crate) fn native_screen_share_preview(
        &self,
        server_id: &str,
        broadcast_id: &str,
        id: String,
        action: broadcast::PreviewAction,
    ) -> Result<(), String> {
        let session = self.screen_share_session(Some(server_id))?;
        let state = session.lock().map_err(|e| e.to_string())?;
        let handle = state
            .native_broadcast
            .as_ref()
            .ok_or("No active screen share")?;
        let status = handle.status.borrow();
        if !status.running || status.broadcast_id != broadcast_id {
            return Err("Screen share has ended or changed".to_owned());
        }
        handle
            .events
            .try_send(Event::Preview { id, action })
            .map_err(|e| format!("Preview signaling unavailable: {e}"))
    }

    fn screen_share_session(
        &self,
        server_id: Option<&str>,
    ) -> Result<Arc<Mutex<SharedState>>, String> {
        match server_id {
            Some(id) => self
                .registry
                .session(id.parse().map_err(|e| format!("invalid server id: {e}"))?)
                .ok_or_else(|| format!("unknown server id: {id}")),
            None => Ok(self.inner.snapshot()),
        }
    }

    /// Reserve and start one native broadcast on a specific server.
    pub(crate) async fn start_native_screen_share(
        &self,
        app: tauri::AppHandle,
        request: StartRequest,
        server_id: Option<String>,
    ) -> Result<Status, String> {
        request.settings.validate()?;
        let session = self.screen_share_session(server_id.as_deref())?;
        let ready = {
            let mut state = session.lock().map_err(|e| e.to_string())?;
            if state
                .native_broadcast
                .as_ref()
                .is_some_and(|h| h.status.borrow().running)
            {
                return Err("this server already has an active screen share".to_owned());
            }
            let context = context(&state, &session, app)?;
            if !context.sfu_available
                && (!context.p2p_available || request.settings.p2p_slots() == 0)
            {
                return Err("this server has no available screen-share transport".to_owned());
            }
            let (handle, ready) = broadcast::spawn(context, request);
            state.native_broadcast = Some(handle);
            ready
        };
        ready
            .await
            .map_err(|_| "screen sharing stopped during startup".to_owned())?
    }

    /// Wait for capture and network resources to close before allowing a restart.
    pub(crate) async fn stop_native_screen_share(
        &self,
        server_id: Option<String>,
    ) -> Result<(), String> {
        let session = self.screen_share_session(server_id.as_deref())?;
        stop_on(&session).await
    }

    /// Return the last broadcast's status, including its terminal error.
    pub(crate) fn native_screen_share_status(
        &self,
        server_id: Option<String>,
    ) -> Result<Option<Status>, String> {
        let session = self.screen_share_session(server_id.as_deref())?;
        let state = session.lock().map_err(|e| e.to_string())?;
        Ok(state
            .native_broadcast
            .as_ref()
            .map(|h| h.status.borrow().clone()))
    }
}

/// Stop before moving or disconnecting so STOP still reaches the old channel.
pub(super) async fn stop_on(session: &Arc<Mutex<SharedState>>) -> Result<(), String> {
    let mut status = {
        let state = session.lock().map_err(|e| e.to_string())?;
        let Some(handle) = &state.native_broadcast else {
            return Ok(());
        };
        handle.cancel.cancel();
        handle.status.clone()
    };
    while status.borrow().running {
        if status.changed().await.is_err() {
            break;
        }
    }
    Ok(())
}

fn context(
    state: &SharedState,
    session: &Arc<Mutex<SharedState>>,
    app: tauri::AppHandle,
) -> Result<Context, String> {
    let client = state.conn.client_handle.clone().ok_or("not connected")?;
    let server_id = state.server_id.ok_or("missing server id")?.to_string();
    let own_session = state
        .conn
        .own_session
        .ok_or("server synchronization incomplete")?;
    let channel = state.current_channel.ok_or("not in a channel")?;
    let epoch = state.conn.epoch;
    let weak = Arc::downgrade(session);
    Ok(Context {
        app,
        client,
        server_id,
        own_session,
        p2p_available: state.server.config.webrtc_p2p_relay_available,
        sfu_available: state.server.config.webrtc_sfu_available,
        members: Box::new(move || {
            let session = weak.upgrade()?;
            let state = session.lock().ok()?;
            if state.conn.epoch != epoch
                || state.conn.status != ConnectionStatus::Connected
                || state.current_channel != Some(channel)
            {
                return None;
            }
            Some(
                state
                    .users
                    .values()
                    .filter(|u| u.channel_id == channel && u.session != own_session)
                    .map(|u| u.session)
                    .collect::<HashSet<_>>(),
            )
        }),
    })
}

/// Consume only the signals owned by a running native broadcaster.
pub(super) fn route(state: &SharedState, signal: &mumble_tcp::WebRtcSignal) -> bool {
    let Some(handle) = &state.native_broadcast else {
        return false;
    };
    if !handle.status.borrow().running {
        return false;
    }
    let Ok(kind) = SignalType::try_from(signal.signal_type.unwrap_or(-1)) else {
        return false;
    };
    let sender = signal.sender_session.unwrap_or(0);
    let owns = match kind {
        SignalType::SdpAnswer | SignalType::IceCandidate => {
            sender == 0 || Some(sender) == state.conn.own_session
        }
        SignalType::P2pRequest
        | SignalType::P2pAnswer
        | SignalType::P2pIce
        | SignalType::P2pLeave => {
            state.server.config.webrtc_p2p_relay_available
                && signal.target_session == state.conn.own_session
                && state
                    .users
                    .get(&sender)
                    .is_some_and(|u| Some(u.channel_id) == state.current_channel)
        }
        _ => false,
    };
    if owns
        && handle
            .events
            .try_send(Event::Signal {
                sender,
                kind,
                payload: signal.payload.clone().unwrap_or_default(),
            })
            .is_err()
    {
        handle.cancel.cancel();
    }
    owns
}
