//! Deciding which viewers get a direct connection and which go through the SFU.
//!
//! A broadcast can serve both at once: the first few viewers are offered a
//! peer-to-peer connection and everyone after that is served by the server's
//! SFU.  The encoder runs once either way - the pipeline's frame sink fans
//! out - so a direct viewer costs uplink bandwidth, not CPU.  That is what
//! the slot limit is really bounding.
//!
//! The broadcaster arbitrates.  A viewer cannot decide for itself whether a
//! slot is free, because several may ask at the same moment and only one side
//! sees the whole picture.  So the protocol is viewer-asks, broadcaster-
//! answers: `P2P_REQUEST` in, then either a `P2P_OFFER` or a `P2P_DECLINE`
//! back (see `TODO.md` 3.1).
//!
//! Slots are reserved when a viewer is admitted, not when its connection
//! finally succeeds.  Two viewers asking simultaneously must not both be
//! admitted to the same slot, and a reservation that never connects is
//! released by [`Allocator::release`] on failure or timeout.

use std::collections::HashMap;

use serde::Serialize;

use super::settings::ScreenShareSettings;

/// Why a viewer is being served by the SFU rather than directly.
///
/// Kept distinct so the UI can say something true rather than a generic
/// "using the server": whether P2P was off, full, unavailable or simply
/// failed is exactly what a user asking "why is this slower for me" wants to
/// know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SfuReason {
    /// The user turned peer-to-peer off.
    Disabled,
    /// Every direct slot is taken.
    NoSlots,
    /// The server does not relay the P2P signal types.
    ServerUnsupported,
    /// A direct connection was tried and did not come up.  Without a TURN
    /// server this is the expected outcome when either side is behind a
    /// symmetric NAT.
    DirectFailed,
}

/// How a viewer is being served.
///
/// Doubles as the answer to a viewer's request to watch, since deciding and
/// recording are the same act: a slot is held from the moment a direct
/// connection is offered, not from when it succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Serving {
    /// A direct connection to the broadcaster, holding one slot.
    Direct,
    /// Through the server's SFU.
    #[serde(rename_all = "camelCase")]
    Sfu {
        /// Why this viewer is not direct.
        reason: SfuReason,
    },
}

impl Serving {
    /// Whether this viewer holds a direct slot.
    fn is_direct(self) -> bool {
        matches!(self, Self::Direct)
    }

    /// An SFU assignment with the given reason.
    fn sfu(reason: SfuReason) -> Self {
        Self::Sfu { reason }
    }
}

/// Tracks which viewers hold a direct slot.
///
/// One of these per broadcast.  Not thread-safe by itself: the caller owns it
/// alongside the rest of the broadcast's state.
#[derive(Debug)]
pub(crate) struct Allocator {
    /// How many viewers may hold a direct connection at once.  Zero when
    /// peer-to-peer is off, or when the server cannot relay for it.
    slots: u32,
    /// Why `slots` is zero, when it is.  Remembered rather than recomputed so
    /// a declined viewer can be told which of the reasons applied.
    disabled_reason: Option<SfuReason>,
    /// How each known viewer is being served.
    viewers: HashMap<u32, Serving>,
}

impl Allocator {
    /// Build an allocator for one broadcast.
    ///
    /// `server_relays_p2p` comes from `ServerConfig.webrtc_p2p_relay_available`.
    /// Without it, direct connections cannot be negotiated at all: a server
    /// with an SFU loaded intercepts the ordinary signal types, and proto2
    /// would read the P2P ones as `START`.
    pub(crate) fn new(settings: &ScreenShareSettings, server_relays_p2p: bool) -> Self {
        let wanted = settings.p2p_slots();
        let (slots, disabled_reason) = if !server_relays_p2p {
            (0, Some(SfuReason::ServerUnsupported))
        } else if wanted == 0 {
            (0, Some(SfuReason::Disabled))
        } else {
            (wanted, None)
        };

        Self { slots, disabled_reason, viewers: HashMap::new() }
    }

    /// Why no viewer can be direct, or `None` when direct slots exist.
    ///
    /// Lets the UI explain the situation before anyone has asked to watch,
    /// without restating the precedence between "turned off" and "server
    /// cannot relay".
    pub(crate) fn sfu_only_reason(&self) -> Option<SfuReason> {
        self.disabled_reason
    }

    /// How many direct slots are currently free.
    pub(crate) fn free_slots(&self) -> u32 {
        self.slots.saturating_sub(self.direct_count())
    }

    /// How many viewers hold a direct connection.
    fn direct_count(&self) -> u32 {
        let held = self.viewers.values().filter(|s| s.is_direct()).count();
        u32::try_from(held).unwrap_or(u32::MAX)
    }

    /// Decide how to serve a viewer that asked to watch.
    ///
    /// Idempotent for a viewer already being served: a repeated request - a
    /// retry, or a duplicate arriving while the first is still in flight -
    /// returns the same answer instead of consuming a second slot.
    pub(crate) fn admit(&mut self, viewer: u32) -> Serving {
        if let Some(existing) = self.viewers.get(&viewer) {
            return *existing;
        }

        let decision = match self.disabled_reason {
            Some(reason) => Serving::sfu(reason),
            None if self.free_slots() == 0 => Serving::sfu(SfuReason::NoSlots),
            None => Serving::Direct,
        };
        let _ = self.viewers.insert(viewer, decision);
        decision
    }

    /// Give up on a viewer's direct connection and move it to the SFU.
    ///
    /// For a connection that failed to establish or dropped: the slot is
    /// freed for the next viewer that asks.  Returns `true` if this viewer
    /// really was on a direct connection, so the caller only signals a
    /// fallback that actually happened.
    pub(crate) fn demote(&mut self, viewer: u32) -> bool {
        if !self.viewers.get(&viewer).is_some_and(|s| s.is_direct()) {
            return false;
        }
        let _ = self.viewers.insert(viewer, Serving::sfu(SfuReason::DirectFailed));
        true
    }

    /// Forget a viewer that stopped watching, releasing any slot it held.
    pub(crate) fn release(&mut self, viewer: u32) {
        let _ = self.viewers.remove(&viewer);
    }

    /// How a viewer is currently being served, if it is known at all.
    pub(crate) fn serving(&self, viewer: u32) -> Option<Serving> {
        self.viewers.get(&viewer).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::settings::{P2pMode, MAX_P2P_VIEWERS};

    fn settings(mode: P2pMode, cap: u32) -> ScreenShareSettings {
        ScreenShareSettings { p2p: mode, p2p_max_viewers: cap, ..Default::default() }
    }

    fn allocator(cap: u32) -> Allocator {
        Allocator::new(&settings(P2pMode::Auto, cap), true)
    }

    #[test]
    fn direct_slots_run_out_and_the_rest_go_to_the_sfu() {
        let mut alloc = allocator(2);
        assert_eq!(alloc.admit(10), Serving::Direct);
        assert_eq!(alloc.admit(11), Serving::Direct);
        assert_eq!(alloc.admit(12), Serving::sfu(SfuReason::NoSlots));
        assert_eq!(alloc.free_slots(), 0);
    }

    #[test]
    fn a_repeated_request_does_not_consume_a_second_slot() {
        // A viewer retrying, or a duplicate request racing the first, must
        // not eat into the budget twice.
        let mut alloc = allocator(2);
        assert_eq!(alloc.admit(10), Serving::Direct);
        assert_eq!(alloc.admit(10), Serving::Direct);
        assert_eq!(alloc.free_slots(), 1, "one viewer must only hold one slot");
    }

    #[test]
    fn a_failed_direct_connection_frees_its_slot_and_says_why() {
        let mut alloc = allocator(1);
        assert_eq!(alloc.admit(10), Serving::Direct);
        assert_eq!(alloc.admit(11), Serving::sfu(SfuReason::NoSlots));

        assert!(alloc.demote(10), "viewer 10 was on a direct connection");
        assert_eq!(alloc.serving(10), Some(Serving::sfu(SfuReason::DirectFailed)));
        assert_eq!(alloc.free_slots(), 1);
        assert_eq!(alloc.admit(12), Serving::Direct, "the freed slot is reusable");

        // Demoting again is not a second failure to report.
        assert!(!alloc.demote(10));
    }

    #[test]
    fn leaving_releases_a_slot() {
        let mut alloc = allocator(1);
        assert_eq!(alloc.admit(10), Serving::Direct);
        alloc.release(10);
        assert_eq!(alloc.serving(10), None);
        assert_eq!(alloc.admit(11), Serving::Direct);
    }

    #[test]
    fn every_way_of_not_getting_a_direct_connection_is_reported_distinctly() {
        // The user turned it off.
        let mut off = Allocator::new(&settings(P2pMode::Disabled, 4), true);
        assert_eq!(off.admit(10), Serving::sfu(SfuReason::Disabled));

        // The server cannot relay for it, whatever the user asked for.
        let mut unsupported = Allocator::new(&settings(P2pMode::Auto, 4), false);
        assert_eq!(unsupported.admit(10), Serving::sfu(SfuReason::ServerUnsupported));
    }

    #[test]
    fn the_cap_is_clamped_to_what_this_build_serves() {
        // Settings stored by a future build with a higher ceiling must not
        // hand out more slots than this one is prepared to serve.
        let alloc = allocator(999);
        assert_eq!(alloc.free_slots(), MAX_P2P_VIEWERS);
    }
}
