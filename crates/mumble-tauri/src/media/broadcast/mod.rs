//! One broadcast owns capture, slot allocation, and independent WebRTC senders.

mod peer;
mod source;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mumble_protocol::client::ClientHandle;
use mumble_protocol::command;
use mumble_protocol::proto::mumble_tcp::web_rtc_signal::SignalType;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::{
    connection::Codec,
    frame,
    settings::ScreenShareSettings,
    transport::{Allocator, Serving},
};

/// Parameters supplied by the capture source picker.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartRequest {
    /// Serialized `CaptureSource`, validated by the native capture backend.
    pub(crate) source: serde_json::Value,
    /// Encoding and transport preferences.
    pub(crate) settings: ScreenShareSettings,
    /// Whether to include the pointer in captured frames.
    #[serde(default = "default_cursor")]
    pub(crate) draw_cursor: bool,
    /// Capture endpoint audio for displays, or process audio for windows.
    #[serde(default = "default_cursor")]
    pub(crate) share_audio: bool,
}

const fn default_cursor() -> bool {
    true
}

/// Runtime state emitted as `native-screen-share-state` and returned by commands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    /// Server connection that owns this broadcast.
    pub(crate) server_id: String,
    /// Unique broadcast identity, including across rapid restarts.
    pub(crate) broadcast_id: String,
    /// True until every sender and the capture source have stopped.
    pub(crate) running: bool,
    /// Effective encoded video frame rate used by the native pipeline.
    pub(crate) fps: u32,
    /// Encoder in use once capture has started.
    pub(crate) encoder_id: Option<String>,
    /// Current assignments, keyed by viewer session.
    pub(crate) viewers: HashMap<u32, Serving>,
    /// Remaining direct connection budget.
    pub(crate) free_direct_slots: u32,
    /// Terminal error or source stop reason.
    pub(crate) error: Option<String>,
}

/// Commands and connection feedback serialized by the broadcast owner.
pub(crate) enum Event {
    /// Incoming signal on the owning Mumble connection.
    Signal {
        sender: u32,
        kind: SignalType,
        payload: String,
    },
    /// A sender terminated; generation prevents stale tasks releasing new slots.
    PeerEnded {
        target: u32,
        id: u64,
        error: Option<String>,
    },
    /// Receiver feedback or frame queue overflow requires a new key frame.
    KeyFrame,
}

/// Per-session handle. Dropping it always requests cleanup.
pub(crate) struct Handle {
    pub(crate) events: mpsc::Sender<Event>,
    pub(crate) status: watch::Receiver<Status>,
    pub(crate) cancel: CancellationToken,
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    pub(crate) preview: Arc<Mutex<Option<crate::media::pipeline::PreviewHandle>>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Context captured once at start, never resolved through the active tab.
pub(crate) struct Context {
    pub(crate) app: tauri::AppHandle,
    pub(crate) client: ClientHandle,
    pub(crate) server_id: String,
    pub(crate) own_session: u32,
    pub(crate) p2p_available: bool,
    pub(crate) sfu_available: bool,
    /// `None` means disconnected, replaced connection, or a channel change.
    pub(crate) members: Box<dyn Fn() -> Option<HashSet<u32>> + Send + Sync>,
}

struct Signal {
    target: u32,
    kind: SignalType,
    payload: String,
}

async fn send_signal(client: &ClientHandle, signal: Signal) -> Result<(), String> {
    tokio::time::timeout(
        Duration::from_secs(3),
        client.send(command::SendWebRtcSignal {
            target_session: signal.target,
            signal_type: signal.kind as i32,
            payload: signal.payload,
        }),
    )
    .await
    .map_err(|_| "screen-share signaling timed out".to_owned())?
    .map_err(|e| format!("screen-share signaling failed: {e}"))
}

/// Reserve the broadcast before asynchronous capture initialization begins.
pub(crate) fn spawn(
    context: Context,
    request: StartRequest,
) -> (Handle, oneshot::Receiver<Result<Status, String>>) {
    let (events, rx) = mpsc::channel(128);
    let cancel = CancellationToken::new();
    let allocator = Allocator::new(&request.settings, context.p2p_available);
    let initial = Status {
        server_id: context.server_id.clone(),
        broadcast_id: uuid::Uuid::new_v4().to_string(),
        running: true,
        fps: request.settings.fps,
        encoder_id: None,
        viewers: HashMap::new(),
        free_direct_slots: allocator.free_slots(),
        error: None,
    };
    let (status, status_rx) = watch::channel(initial);
    let (ready, ready_rx) = oneshot::channel();
    let handle = Handle {
        events: events.clone(),
        status: status_rx,
        cancel: cancel.clone(),
        #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
        preview: Arc::new(Mutex::new(None)),
    };
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    let preview = Arc::clone(&handle.preview);
    let _task = tokio::spawn(async move {
        let mut owner = Owner {
            context,
            allocator,
            events,
            status,
            cancel,
            peers: HashMap::new(),
            members: HashSet::new(),
            next_peer: 0,
            key_requested: false,
            last_key: Instant::now() - Duration::from_secs(1),
            announced: false,
            audio: None,
            #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
            preview_handle: preview,
        };
        let mut ready = Some(ready);
        let result = owner.start(request, rx, &mut ready).await;
        owner.shutdown().await;
        owner.status.send_modify(|state| {
            state.running = false;
            state.error = result.as_ref().err().cloned();
        });
        owner.emit();
        if let Some(ready) = ready {
            let error = result
                .err()
                .unwrap_or_else(|| "screen sharing cancelled".to_owned());
            let _ = ready.send(Err(error));
        }
    });
    (handle, ready_rx)
}

struct Owner {
    context: Context,
    allocator: Allocator,
    events: mpsc::Sender<Event>,
    status: watch::Sender<Status>,
    cancel: CancellationToken,
    peers: HashMap<u32, peer::Peer>,
    members: HashSet<u32>,
    next_peer: u64,
    key_requested: bool,
    last_key: Instant,
    announced: bool,
    audio: Option<broadcast::Sender<Arc<super::audio::Packet>>>,
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    preview_handle: Arc<Mutex<Option<crate::media::pipeline::PreviewHandle>>>,
}

impl Owner {
    async fn start(
        &mut self,
        request: StartRequest,
        rx: mpsc::Receiver<Event>,
        ready: &mut Option<oneshot::Sender<Result<Status, String>>>,
    ) -> Result<(), String> {
        let (frames, _) = broadcast::channel(8);
        let sink_tx = frames.clone();
        self.audio = request.share_audio.then(|| broadcast::channel(5).0);
        let audio = self.audio.clone();
        let data_dir = crate::e2e_data_dir(&self.context.app)?;
        let capture = tokio::task::spawn_blocking(move || {
            source::start(
                request,
                &data_dir,
                Box::new(move |frame| {
                    let _ = sink_tx.send(Arc::new(frame));
                }),
                audio,
            )
        })
        .await
        .map_err(|e| format!("capture startup task failed: {e}"))?;
        let mut capture = capture?;
        #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
        if let Ok(mut preview) = self.preview_handle.lock() {
            *preview = capture.preview();
        }
        let result = self.run_capture(capture.as_mut(), frames, rx, ready).await;
        // PipelineHandle::drop joins native threads and must not block Tokio.
        let _ = tokio::task::spawn_blocking(move || drop(capture)).await;
        result
    }

    async fn run_capture(
        &mut self,
        capture: &mut dyn source::Capture,
        frames: broadcast::Sender<Arc<frame::EncodedFrame>>,
        mut rx: mpsc::Receiver<Event>,
        ready: &mut Option<oneshot::Sender<Result<Status, String>>>,
    ) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err("screen sharing cancelled".to_owned());
        }
        self.members = (self.context.members)().ok_or("screen-share connection changed")?;
        let codec = Codec::from_encoder(capture.encoder_id()).ok_or("unsupported video codec")?;
        let fps = capture.encoding().fps;
        self.status
            .send_modify(|s| {
                s.encoder_id = Some(capture.encoder_id().to_owned());
                s.fps = fps;
            });
        self.announce().await?;
        self.announced = true;
        if self.context.sfu_available {
            self.add_peer(0, codec, fps, &frames);
        }
        self.emit();
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(self.status.borrow().clone()));
        }
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return Ok(()),
                event = rx.recv() => match event {
                    Some(event) => self.event(event, codec, fps, &frames).await?,
                    None => return Ok(()),
                },
                _ = tick.tick() => {
                    if let Some(result) = capture.stopped() { return result; }
                    self.refresh_members().await?;
                    self.reap_finished().await?;
                    if self.key_requested && self.last_key.elapsed() >= Duration::from_millis(500) {
                        capture.request_key_frame();
                        self.key_requested = false;
                        self.last_key = Instant::now();
                    }
                }
            }
        }
    }

    fn add_peer(
        &mut self,
        target: u32,
        codec: Codec,
        fps: u32,
        frames: &broadcast::Sender<Arc<frame::EncodedFrame>>,
    ) {
        self.next_peer += 1;
        let config = peer::Config {
            target,
            id: self.next_peer,
            codec,
            fps,
            client: self.context.client.clone(),
            events: self.events.clone(),
        };
        let _ = self.peers.insert(
            target,
            peer::spawn(
                config,
                frames.subscribe(),
                self.audio.as_ref().map(broadcast::Sender::subscribe),
            ),
        );
    }

    async fn event(
        &mut self,
        event: Event,
        codec: Codec,
        fps: u32,
        frames: &broadcast::Sender<Arc<frame::EncodedFrame>>,
    ) -> Result<(), String> {
        match event {
            Event::KeyFrame => self.key_requested = true,
            Event::PeerEnded { target, id, error } => {
                if self.peers.get(&target).is_none_or(|peer| peer.id != id) {
                    return Ok(());
                }
                self.remove_peer(target).await;
                if target == 0 {
                    return Err(error.unwrap_or_else(|| "SFU connection closed".to_owned()));
                }
                if self.allocator.demote(target) {
                    self.update_viewer(target);
                    self.decline(target).await?;
                }
            }
            Event::Signal {
                sender,
                kind,
                payload,
            } => {
                self.signal(sender, kind, payload, codec, fps, frames)
                    .await?
            }
        }
        Ok(())
    }

    async fn signal(
        &mut self,
        sender: u32,
        kind: SignalType,
        payload: String,
        codec: Codec,
        fps: u32,
        frames: &broadcast::Sender<Arc<frame::EncodedFrame>>,
    ) -> Result<(), String> {
        let target = match kind {
            SignalType::SdpAnswer | SignalType::IceCandidate
                if sender == self.context.own_session || sender == 0 =>
            {
                0
            }
            SignalType::P2pRequest
            | SignalType::P2pAnswer
            | SignalType::P2pIce
            | SignalType::P2pLeave
                if self.context.p2p_available
                    && (self.context.members)()
                        .is_some_and(|members| members.contains(&sender)) =>
            {
                sender
            }
            _ => return Ok(()),
        };
        match kind {
            SignalType::P2pRequest => {
                if self.allocator.admit(target) == Serving::Direct {
                    if !self.peers.contains_key(&target) {
                        self.add_peer(target, codec, fps, frames);
                    }
                } else {
                    self.decline(target).await?;
                }
                self.update_viewer(target);
            }
            SignalType::P2pLeave => {
                self.remove_peer(target).await;
                self.allocator.release(target);
                self.status.send_modify(|s| {
                    let _ = s.viewers.remove(&target);
                });
                self.update_viewer(target);
            }
            _ => {
                let message = match kind {
                    SignalType::SdpAnswer | SignalType::P2pAnswer => peer::Input::Answer(payload),
                    _ => peer::Input::Ice(payload),
                };
                if let Some(peer) = self.peers.get(&target) {
                    if peer.input.try_send(message).is_err() {
                        peer.cancel.cancel();
                    }
                }
            }
        }
        Ok(())
    }

    async fn decline(&self, target: u32) -> Result<(), String> {
        let payload = serde_json::json!({ "sfuAvailable": self.context.sfu_available }).to_string();
        send_signal(
            &self.context.client,
            Signal {
                target,
                kind: SignalType::P2pDecline,
                payload,
            },
        )
        .await
    }

    fn update_viewer(&self, target: u32) {
        self.status.send_modify(|s| {
            if let Some(serving) = self.allocator.serving(target) {
                let _ = s.viewers.insert(target, serving);
            }
            s.free_direct_slots = self.allocator.free_slots();
        });
        self.emit();
    }

    async fn announce(&self) -> Result<(), String> {
        let payload = serde_json::json!({ "native": true, "p2p": self.context.p2p_available,
            "sfuAvailable": self.context.sfu_available, "broadcastId": self.status.borrow().broadcast_id }).to_string();
        send_signal(
            &self.context.client,
            Signal {
                target: 0,
                kind: SignalType::Start,
                payload,
            },
        )
        .await
    }

    async fn refresh_members(&mut self) -> Result<(), String> {
        let current =
            (self.context.members)().ok_or("screen-share connection or channel changed")?;
        let joined = current.difference(&self.members).next().is_some();
        let departed: Vec<_> = self.members.difference(&current).copied().collect();
        for viewer in departed {
            self.remove_peer(viewer).await;
            self.allocator.release(viewer);
            self.status.send_modify(|s| {
                let _ = s.viewers.remove(&viewer);
            });
            self.update_viewer(viewer);
        }
        self.members = current;
        if joined {
            self.announce().await?;
        }
        Ok(())
    }

    async fn remove_peer(&mut self, target: u32) {
        if let Some(peer) = self.peers.remove(&target) {
            peer.cancel.cancel();
            let _ = peer.task.await;
        }
    }

    async fn reap_finished(&mut self) -> Result<(), String> {
        let finished: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.task.is_finished())
            .map(|(target, _)| *target)
            .collect();
        for target in finished {
            self.remove_peer(target).await;
            if target == 0 {
                return Err("SFU sender stopped".to_owned());
            }
            if self.allocator.demote(target) {
                self.update_viewer(target);
                self.decline(target).await?;
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) {
        #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
        if let Ok(mut preview) = self.preview_handle.lock() {
            *preview = None;
        }
        for peer in self.peers.values() {
            peer.cancel.cancel();
        }
        for (_, peer) in self.peers.drain() {
            let _ = peer.task.await;
        }
        if self.announced {
            let _ = send_signal(
                &self.context.client,
                Signal {
                    target: 0,
                    kind: SignalType::Stop,
                    payload: String::new(),
                },
            )
            .await;
        }
    }

    fn emit(&self) {
        let _ = self
            .context
            .app
            .emit("native-screen-share-state", self.status.borrow().clone());
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_defaults_on_and_respects_explicit_disable() -> Result<(), serde_json::Error> {
        let mut value = serde_json::json!({
            "source": { "kind": "monitor", "outputIndex": 0, "hmonitor": "1" },
            "settings": ScreenShareSettings::default(),
        });
        assert!(serde_json::from_value::<StartRequest>(value.clone())?.share_audio);
        value["shareAudio"] = serde_json::json!(false);
        assert!(!serde_json::from_value::<StartRequest>(value)?.share_audio);
        Ok(())
    }
}
