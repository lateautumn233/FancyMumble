//! Independent per-peer sending, ICE, and RTCP processing.

use std::sync::Arc;
use std::time::Duration;

use rtc::peer_connection::state::RTCPeerConnectionState;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::peer_connection::{PeerConnectionEventHandler, RTCPeerConnectionIceEvent};

use super::{send_signal, Event, Signal};
use crate::media::connection::{Codec, Connection};
use crate::media::frame::EncodedFrame;
use mumble_protocol::client::ClientHandle;
use mumble_protocol::proto::mumble_tcp::web_rtc_signal::SignalType;

/// Messages addressed to one established negotiation.
pub(super) enum Input {
    /// Remote SDP answer.
    Answer(String),
    /// Remote ICE candidate JSON.
    Ice(String),
}

/// Owns the task's cancellation and bounded signaling mailbox.
pub(super) struct Peer {
    pub(super) id: u64,
    pub(super) input: mpsc::Sender<Input>,
    pub(super) cancel: CancellationToken,
    pub(super) task: tokio::task::JoinHandle<()>,
}

/// Stable destination and encoding for a peer task.
pub(super) struct Config {
    pub(super) target: u32,
    pub(super) id: u64,
    pub(super) codec: Codec,
    pub(super) fps: u32,
    pub(super) client: ClientHandle,
    pub(super) events: mpsc::Sender<Event>,
    pub(super) preview: Option<tauri::ipc::Channel<serde_json::Value>>,
}

enum Local {
    Ice(String),
    State(RTCPeerConnectionState),
}

struct Handler(mpsc::Sender<Local>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(mut candidate) = event.candidate.to_json() {
            // webrtc-rs emits an empty MID; browsers resolve that as an
            // unknown media section. The video m-line is identified by index.
            candidate.sdp_mid = None;
            if let Ok(payload) = serde_json::to_string(&candidate) {
                let _ = self.0.try_send(Local::Ice(payload));
            }
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.0.try_send(Local::State(state));
    }
}

/// Start one sender. A slow viewer never shares a queue or await with another.
pub(super) fn spawn(config: Config, frames: broadcast::Receiver<Arc<EncodedFrame>>) -> Peer {
    let (input, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let id = config.id;
    let task = tokio::spawn(async move {
        let (local_tx, local_rx) = mpsc::channel(64);
        let create = async {
            let handler = Arc::new(Handler(local_tx));
            let runtime = Arc::new(webrtc::runtime::TokioRuntime);
            if config.preview.is_some() {
                Connection::with_ice_servers(
                    config.codec,
                    config.fps,
                    handler,
                    runtime,
                    vec![],
                    vec!["127.0.0.1:0".to_owned()],
                )
                .await
            } else {
                Connection::new(config.codec, config.fps, handler, runtime).await
            }
        };
        let result = tokio::select! {
            () = token.cancelled() => return,
            result = tokio::time::timeout(Duration::from_secs(20), create) => result,
        };
        let outcome = match result {
            Ok(Ok(mut connection)) => {
                let result = tokio::select! {
                    () = token.cancelled() => Ok(()),
                    result = run(&config, &mut connection, frames, rx, local_rx, &token) => result,
                };
                connection.close().await;
                result
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err("WebRTC creation timed out".to_owned()),
        };
        if !token.is_cancelled() {
            if let Some(channel) = &config.preview {
                let _ = channel.send(serde_json::json!({
                    "kind": "error", "payload": outcome.err().unwrap_or_else(|| "Preview connection closed".to_owned()),
                }));
                return;
            }
            let _ = config.events.try_send(Event::PeerEnded {
                target: config.target,
                id,
                error: outcome.err(),
            });
        }
    });
    Peer {
        id,
        input,
        cancel,
        task,
    }
}

async fn run(
    config: &Config,
    connection: &mut Connection,
    mut frames: broadcast::Receiver<Arc<EncodedFrame>>,
    mut input: mpsc::Receiver<Input>,
    mut local: mpsc::Receiver<Local>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let offer = connection.create_offer().await?;
    config
        .signal(SignalType::SdpOffer, SignalType::P2pOffer, offer)
        .await?;
    let track = connection.track();
    let mut connected = false;
    let mut answered = false;
    let mut waiting_key = true;
    let mut feedback_open = true;
    let mut pending_ice = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            () = &mut timeout, if !connected => return Err("WebRTC connection timed out after 20 seconds".to_owned()),
            message = input.recv() => match message {
                Some(Input::Answer(sdp)) if !answered => {
                    connection.accept_answer(sdp).await?;
                    answered = true;
                    for candidate in pending_ice.drain(..) {
                        connection.add_ice_candidate(candidate).await?;
                    }
                }
                Some(Input::Ice(candidate)) => {
                    if answered { connection.add_ice_candidate(candidate).await?; }
                    else if pending_ice.len() < 64 { pending_ice.push(candidate); }
                    else { return Err("too many pending ICE candidates".to_owned()); }
                }
                Some(Input::Answer(_)) => {}
                None => return Ok(()),
            },
            event = local.recv() => match event {
                Some(Local::Ice(candidate)) => config.signal(SignalType::IceCandidate, SignalType::P2pIce, candidate).await?,
                Some(Local::State(RTCPeerConnectionState::Connected)) => {
                    connected = true;
                    config.key_frame();
                }
                Some(Local::State(RTCPeerConnectionState::Failed | RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Closed)) =>
                    return Err("WebRTC connection lost".to_owned()),
                Some(Local::State(_)) => {}
                None => return Ok(()),
            },
            feedback = track.poll(), if connected && answered && feedback_open => {
                if let Some(TrackLocalEvent::OnRtcpPacket(packets)) = feedback {
                    if packets.iter().any(|p| {
                        p.as_any().is::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()
                        || p.as_any().is::<rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest>()
                    }) { config.key_frame(); }
                } else { feedback_open = false; }
            }
            frame = frames.recv() => match frame {
                Ok(frame) if connected && answered => {
                    if waiting_key && !frame.is_key { continue; }
                    waiting_key = false;
                    tokio::time::timeout(Duration::from_secs(3), connection.send(&frame)).await
                        .map_err(|_| "video sender stalled".to_owned())??;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => { waiting_key = true; config.key_frame(); }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

impl Config {
    fn key_frame(&self) {
        let _ = self.events.try_send(Event::KeyFrame);
    }

    async fn signal(
        &self,
        sfu: SignalType,
        direct: SignalType,
        payload: String,
    ) -> Result<(), String> {
        if let Some(channel) = &self.preview {
            return channel.send(serde_json::json!({
                "kind": if sfu == SignalType::SdpOffer { "offer" } else { "ice" }, "payload": payload,
            })).map_err(|e| format!("preview signaling failed: {e}"));
        }
        send_signal(
            &self.client,
            Signal {
                target: self.target,
                kind: if self.target == 0 { sfu } else { direct },
                payload,
            },
        )
        .await
    }
}
