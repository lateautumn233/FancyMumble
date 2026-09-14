//! Real loopback ICE/DTLS/RTP regression coverage without a GPU or STUN service.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{PeerConnectionEventHandler, RTCPeerConnectionIceEvent};
use webrtc::runtime::TokioRuntime;

struct Receiver {
    ice: mpsc::UnboundedSender<RTCIceCandidateInit>,
    tracks: mpsc::UnboundedSender<Arc<dyn TrackRemote>>,
    connected: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Receiver {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(mut candidate) = event.candidate.to_json() {
            candidate.sdp_mid = None;
            let _ = self.ice.send(candidate);
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.tracks.send(track);
    }

    async fn on_connection_state_change(
        &self,
        state: rtc::peer_connection::state::RTCPeerConnectionState,
    ) {
        self.connected.store(
            state == rtc::peer_connection::state::RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
    }
}

#[tokio::test]
async fn loopback_negotiates_sends_rtp_and_preserves_dropped_frame_timestamps(
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("FANCY_WEBRTC_TEST_TRACE").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("webrtc=debug,rtc=debug")
            .with_test_writer()
            .try_init();
    }
    let (send_ice, mut from_sender) = mpsc::unbounded_channel();
    let (recv_ice, mut from_receiver) = mpsc::unbounded_channel();
    let (tracks, remote_tracks) = mpsc::unbounded_channel();
    let connected = Arc::new(AtomicBool::new(false));
    let mut sender = Connection::with_ice_servers(
        Codec::H264,
        30,
        Arc::new(Receiver {
            ice: send_ice,
            tracks: tracks.clone(),
            connected: Arc::clone(&connected),
        }),
        Arc::new(TokioRuntime),
        vec![],
        vec!["127.0.0.1:0".to_owned()],
    )
    .await?;
    let mut engine = MediaEngine::default();
    engine.register_codec(Codec::H264.codec_parameters(), RtpCodecKind::Video)?;
    let receiver: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_media_engine(engine)
            .with_runtime(Arc::new(TokioRuntime))
            .with_handler(Arc::new(Receiver {
                ice: recv_ice,
                tracks,
                connected: Arc::new(AtomicBool::new(false)),
            }))
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .build()
            .await?,
    );
    let peer = Arc::clone(&receiver);
    let outgoing = tokio::spawn(async move {
        while let Some(candidate) = from_sender.recv().await {
            let _ = peer.add_ice_candidate(candidate).await;
        }
    });
    let peer = Arc::clone(&sender.peer);
    let incoming = tokio::spawn(async move {
        while let Some(candidate) = from_receiver.recv().await {
            let json = serde_json::to_string(&candidate)?;
            let parsed: RTCIceCandidateInit = serde_json::from_str(&json)?;
            peer.add_ice_candidate(parsed).await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        exercise(&mut sender, receiver.as_ref(), remote_tracks, &connected),
    )
    .await;
    outgoing.abort();
    incoming.abort();
    let _ = outgoing.await;
    let _ = incoming.await;
    sender.close().await;
    receiver.close().await?;
    result??;
    Ok(())
}

async fn exercise(
    sender: &mut Connection,
    receiver: &dyn PeerConnection,
    mut remote_tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
    connected: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    let offer = sender.create_offer().await?;
    assert!(offer.contains("a=sendonly"));
    assert!(offer.contains("nack pli"));
    receiver
        .set_remote_description(RTCSessionDescription::offer(offer)?)
        .await?;
    let answer = receiver.create_answer(None).await?;
    receiver.set_local_description(answer.clone()).await?;
    sender.accept_answer(answer.sdp).await?;
    while !connected.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for pts_ms in [0, 33, 200] {
        sender
            .send(&EncodedFrame {
                bytes: vec![0, 0, 0, 1, 0x65, 0x88, 0x84],
                pts_ms,
                is_key: true,
            })
            .await?;
    }
    let track = remote_tracks.recv().await.ok_or("no remote track")?;
    let mut timestamps = Vec::new();
    while timestamps.len() < 3 {
        if let Some(TrackRemoteEvent::OnRtpPacket(packet)) = track.poll().await {
            timestamps.push(packet.header.timestamp);
        }
    }
    assert_eq!(timestamps[1].wrapping_sub(timestamps[0]), 33 * 90);
    assert_eq!(timestamps[2].wrapping_sub(timestamps[0]), 200 * 90);
    track
        .write_rtcp(vec![Box::new(
            rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 42,
                media_ssrc: sender.ssrc,
            },
        )])
        .await?;
    loop {
        if let Some(webrtc::media_stream::track_local::TrackLocalEvent::OnRtcpPacket(packets)) =
            sender.track().poll().await
        {
            if packets.iter().any(|packet| packet.as_any().is::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()) { break; }
        }
    }
    assert!(sender
        .add_ice_candidate("not JSON".to_owned())
        .await
        .is_err());
    Ok(())
}
