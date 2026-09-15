//! Routing regressions: publisher answers must not consume viewer traffic.

use super::*;
use crate::media::broadcast::Handle;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

fn broadcaster() -> (SharedState, mpsc::Receiver<Event>) {
    let (events, rx) = mpsc::channel(8);
    let (_, status) = watch::channel(Status {
        server_id: "pinned-server".to_owned(),
        broadcast_id: "test-broadcast".to_owned(),
        running: true,
        fps: 30,
        encoder_id: None,
        viewers: Default::default(),
        free_direct_slots: 2,
        error: None,
    });
    let mut state = SharedState::default();
    state.conn.own_session = Some(7);
    state.current_channel = Some(3);
    state.server.config.webrtc_p2p_relay_available = true;
    let _ = state.users.insert(
        42,
        super::super::types::UserEntry {
            session: 42,
            channel_id: 3,
            ..super::super::types::UserEntry::new(42)
        },
    );
    state.native_broadcast = Some(Handle {
        events,
        status,
        cancel: CancellationToken::new(),
        #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
        preview: Default::default(),
    });
    (state, rx)
}

fn signal(sender: u32, kind: SignalType) -> mumble_tcp::WebRtcSignal {
    mumble_tcp::WebRtcSignal {
        sender_session: Some(sender),
        target_session: Some(7),
        signal_type: Some(kind as i32),
        payload: Some("payload".to_owned()),
    }
}

#[test]
fn sfu_answers_for_other_broadcasters_still_reach_the_browser() {
    let (state, mut rx) = broadcaster();
    assert!(!route(&state, &signal(42, SignalType::SdpAnswer)));
    assert!(rx.try_recv().is_err());
    assert!(route(&state, &signal(7, SignalType::SdpAnswer)));
    assert!(matches!(
        rx.try_recv(),
        Ok(Event::Signal {
            sender: 7,
            kind: SignalType::SdpAnswer,
            ..
        })
    ));
}

#[test]
fn direct_requests_require_capability_target_and_channel_membership() {
    let (mut state, mut rx) = broadcaster();
    let request = signal(42, SignalType::P2pRequest);
    assert!(route(&state, &request));
    assert!(matches!(
        rx.try_recv(),
        Ok(Event::Signal {
            sender: 42,
            kind: SignalType::P2pRequest,
            ..
        })
    ));
    state.server.config.webrtc_p2p_relay_available = false;
    assert!(!route(&state, &request));
    state.server.config.webrtc_p2p_relay_available = true;
    assert!(!route(
        &state,
        &mumble_tcp::WebRtcSignal {
            target_session: Some(9),
            ..request.clone()
        }
    ));
    state.current_channel = Some(4);
    assert!(!route(&state, &request));
    assert!(rx.try_recv().is_err());
}
