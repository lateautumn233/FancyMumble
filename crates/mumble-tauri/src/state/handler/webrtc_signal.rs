use mumble_protocol::proto::mumble_tcp;
use tracing::debug;

use super::{HandleMessage, HandlerContext};
use crate::state::types::WebRtcSignalPayload;

impl HandleMessage for mumble_tcp::WebRtcSignal {
    fn handle(&self, ctx: &HandlerContext) {
        #[cfg(not(target_os = "android"))]
        if let Ok(state) = ctx.shared.lock() {
            if crate::state::screen_share::route(&state, self) { return; }
        }
        debug!(
            sender = ?self.sender_session,
            target = ?self.target_session,
            signal_type = ?self.signal_type,
            "webrtc signal received"
        );

        ctx.emit(
            "webrtc-signal",
            WebRtcSignalPayload {
                sender_session: self.sender_session,
                target_session: self.target_session,
                signal_type: self.signal_type.unwrap_or(0),
                payload: self.payload.clone().unwrap_or_default(),
            },
        );
    }
}
