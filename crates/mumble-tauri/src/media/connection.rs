//! One WebRTC connection carrying an encoded screen-share stream.
//!
//! The same shape serves both transports: the SFU and a direct viewer differ
//! only in who is on the far end and which signal types carry the handshake.
//! Both offer a send-only video track and optional stereo Opus audio.
//!
//! Frames arrive from the encode pipeline through [`Connection::send`] and are
//! written as WebRTC samples.  A sample is a whole encoded frame; the RTP
//! packetiser splits it into FU-A / STAP-A units, which is `packetization-mode=1`
//! and what the SFU negotiates (see `TODO.md` 0.1).

use std::sync::Arc;
use std::time::Duration;

use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MediaEngine, MIME_TYPE_AV1, MIME_TYPE_H264, MIME_TYPE_HEVC,
};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::{RTCIceCandidateInit, RTCIceServer};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use rtc::rtp_transceiver::{PayloadType, RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{PeerConnection, PeerConnectionBuilder};
use webrtc::rtp_transceiver::RtpSender;

use super::frame::EncodedFrame;

mod audio;
mod feedback;
mod network;

/// Clock rate every WebRTC video codec uses, in Hz.
const VIDEO_CLOCK_RATE: u32 = 90_000;

/// Payload type we offer for H.264.
///
/// 114 is what str0m registers for High profile with `packetization-mode=1`,
/// which is the variant our encoders produce (`TODO.md` 0.1).  Matching its
/// number is not required - the answer decides - but offering the same one
/// makes the negotiated result easy to recognise in a capture.
const PT_H264: PayloadType = 114;

/// Payload type we offer for HEVC.
const PT_HEVC: PayloadType = 98;

/// Payload type we offer for AV1.
const PT_AV1: PayloadType = 45;

/// STUN servers used to discover our public address.
///
/// The SFU is ICE-lite and needs none of this, but a direct viewer does.  Same
/// servers the browser implementation used, so behaviour does not change for
/// users who could already share.
const STUN_URLS: [&str; 2] = [
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
];

/// The codec a connection carries, derived from the encoder in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// H.264, the only codec automatic encoder selection produces.
    H264,
    /// HEVC.  Needs `enable_h265` on the server's SFU.
    Hevc,
    /// AV1.
    Av1,
}

impl Codec {
    /// Which codec an `FFmpeg` encoder name produces.
    ///
    /// `None` for a name we do not recognise: offering the wrong codec is
    /// worse than refusing to start, because a payload-type mismatch makes the
    /// SFU drop every frame silently (`TODO.md` 0.1).
    pub(crate) fn from_encoder(id: &str) -> Option<Self> {
        if id.starts_with("h264") || id == "libopenh264" {
            return Some(Self::H264);
        }
        if id.starts_with("hevc") {
            return Some(Self::Hevc);
        }
        if id.starts_with("av1") {
            return Some(Self::Av1);
        }
        None
    }

    /// The RTP codec parameters to offer.
    ///
    /// The H.264 `fmtp` line has to describe what the encoder actually emits.
    /// str0m requires the profile and packetization-mode to match exactly and
    /// only tolerates a different level; a mismatch is not an error but a
    /// silent frame drop, so `profile-level-id` says High 3.1 (`64001f`)
    /// because that is the profile stage 1 configures every H.264 encoder for.
    fn codec_parameters(self) -> RTCRtpCodecParameters {
        let (mime, payload_type, fmtp) = match self {
            Self::H264 => (
                MIME_TYPE_H264,
                PT_H264,
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=64001f",
            ),
            Self::Hevc => (MIME_TYPE_HEVC, PT_HEVC, ""),
            Self::Av1 => (MIME_TYPE_AV1, PT_AV1, ""),
        };

        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: mime.to_owned(),
                clock_rate: VIDEO_CLOCK_RATE,
                channels: 0,
                sdp_fmtp_line: fmtp.to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type,
        }
    }
}

/// A send-only WebRTC connection carrying screen video and optional source audio.
pub(crate) struct Connection {
    /// The peer connection itself.
    peer: Arc<dyn PeerConnection>,
    /// The video track frames are written to.
    track: Arc<TrackLocalStaticSample>,
    /// The track's sender, for reading back the negotiated payload type.
    sender: Arc<dyn RtpSender>,
    /// Synchronisation source for the RTP stream.
    ssrc: u32,
    /// Payload type the far end agreed to, once an answer has arrived.
    ///
    /// `None` until then, which is also why [`Self::send`] does nothing before
    /// the answer: `write_rtp` rejects a payload type that is not part of a
    /// negotiated codec.
    payload_type: Option<PayloadType>,
    /// Nominal frame interval, used as each sample's duration.
    frame_duration: Duration,
    /// Previous input timestamp, used to preserve gaps when frames are dropped.
    last_pts_ms: Option<i64>,
    audio: Option<audio::Sender>,
}

impl Connection {
    /// Open a connection and add a video track for `codec`.
    ///
    /// Nothing is negotiated yet: the caller drives that with
    /// [`Self::create_offer`], then [`Self::accept_answer`].
    pub(crate) async fn new(
        codec: Codec,
        fps: u32,
        handler: Arc<dyn webrtc::peer_connection::PeerConnectionEventHandler>,
        runtime: Arc<dyn webrtc::runtime::Runtime>,
    ) -> Result<Self, String> {
        let (runtime, udp_addrs) = network::configure(runtime)?;
        Self::with_ice_servers(
            codec,
            fps,
            handler,
            runtime,
            vec![RTCIceServer {
                urls: STUN_URLS.iter().map(|u| (*u).to_owned()).collect(),
                ..Default::default()
            }],
            udp_addrs,
        )
        .await
    }

    pub(crate) async fn with_ice_servers(
        codec: Codec,
        fps: u32,
        handler: Arc<dyn webrtc::peer_connection::PeerConnectionEventHandler>,
        runtime: Arc<dyn webrtc::runtime::Runtime>,
        ice_servers: Vec<RTCIceServer>,
        udp_addrs: Vec<String>,
    ) -> Result<Self, String> {
        let codec_parameters = codec.codec_parameters();

        let mut media_engine = MediaEngine::default();
        media_engine
            .register_codec(codec_parameters.clone(), RtpCodecKind::Video)
            .map_err(|e| format!("could not register {codec:?}: {e}"))?;
        media_engine
            .register_codec(audio::codec_parameters(), RtpCodecKind::Audio)
            .map_err(|e| format!("could not register Opus: {e}"))?;

        let registry = register_default_interceptors(Registry::new(), &mut media_engine)
            .map_err(|e| format!("could not register interceptors: {e}"))?;
        let registry = registry.with(feedback::KeyFrameFeedback::new);

        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(ice_servers)
            .build();

        let peer: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(config)
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .with_handler(handler)
                .with_runtime(runtime)
                // Bind on every interface: ICE picks the usable one, and a
                // broadcaster on a machine with several NICs should not have
                // to guess which one reaches the far end.
                .with_udp_addrs(udp_addrs)
                .build()
                .await
                .map_err(|e| format!("could not create peer connection: {e}"))?,
        );

        // The SSRC is ours to choose and has to be declared up front: it
        // identifies this RTP stream, and `sample_writer` stamps it on every
        // packet, so it must be the one the track was built with.
        let ssrc: u32 = rand::random();
        let track = Arc::new(
            TrackLocalStaticSample::new(MediaStreamTrack::new(
                "fancy-screenshare".to_owned(),
                "fancy-screenshare-video".to_owned(),
                "screen".to_owned(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: codec_parameters.rtp_codec.clone(),
                    ..Default::default()
                }],
            ))
            .map_err(|e| format!("could not create video track: {e}"))?,
        );

        let transceiver = peer
            .add_transceiver_from_track(
                Arc::clone(&track) as Arc<dyn TrackLocal>,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Sendonly,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("could not add video track: {e}"))?;
        let sender = transceiver
            .sender()
            .await
            .map_err(|e| format!("could not read video sender: {e}"))?
            .ok_or("video transceiver has no sender")?;

        Ok(Self {
            peer,
            track,
            sender,
            ssrc,
            payload_type: None,
            frame_duration: Duration::from_secs(1) / fps.max(1),
            last_pts_ms: None,
            audio: None,
        })
    }

    /// Add source audio before creating the offer.
    pub(crate) async fn enable_audio(&mut self) -> Result<(), String> {
        self.audio = Some(audio::Sender::new(self.peer.as_ref()).await?);
        Ok(())
    }

    /// Send a timestamped Opus packet to this viewer.
    pub(crate) async fn send_audio(&mut self, packet: &super::audio::Packet) -> Result<(), String> {
        if let Some(audio) = &mut self.audio {
            audio.send(packet).await?;
        }
        Ok(())
    }

    /// Create an offer and set it as the local description.
    ///
    /// Returns the SDP to signal to the far end.
    pub(crate) async fn create_offer(&self) -> Result<String, String> {
        let offer = self
            .peer
            .create_offer(None)
            .await
            .map_err(|e| format!("could not create offer: {e}"))?;
        let sdp = offer.sdp.clone();
        self.peer
            .set_local_description(offer)
            .await
            .map_err(|e| format!("could not set local description: {e}"))?;
        Ok(sdp)
    }

    /// Apply the far end's answer, and learn the payload type it agreed to.
    ///
    /// Until this succeeds, [`Self::send`] discards frames: there is nothing
    /// to send them over, and no negotiated payload type to stamp them with.
    pub(crate) async fn accept_answer(&mut self, sdp: String) -> Result<(), String> {
        let answer =
            RTCSessionDescription::answer(sdp).map_err(|e| format!("malformed answer: {e}"))?;
        self.peer
            .set_remote_description(answer)
            .await
            .map_err(|e| format!("could not set remote description: {e}"))?;

        let parameters = self
            .sender
            .get_parameters()
            .await
            .map_err(|e| format!("could not read sender parameters: {e}"))?;
        let negotiated = parameters
            .rtp_parameters
            .codecs
            .first()
            .map(|codec| codec.payload_type)
            .ok_or_else(|| {
                // The far end accepted the session but not our codec. Worth an
                // error rather than a silent no-op: nothing would ever arrive.
                "the far end negotiated no video codec".to_owned()
            })?;
        self.payload_type = Some(negotiated);
        if let Some(audio) = &mut self.audio {
            audio.negotiate().await?;
        }
        Ok(())
    }

    /// Add a remote ICE candidate.
    pub(crate) async fn add_ice_candidate(&self, candidate: String) -> Result<(), String> {
        let init: RTCIceCandidateInit = serde_json::from_str(&candidate)
            .map_err(|e| format!("malformed ICE candidate: {e}"))?;
        self.peer
            .add_ice_candidate(init)
            .await
            .map_err(|e| format!("could not add ICE candidate: {e}"))
    }

    /// Write one encoded frame.
    ///
    /// A no-op before the answer has been applied.  Frames keep arriving from
    /// the encoder during the handshake and dropping them is correct: a viewer
    /// that has not finished negotiating could not decode them, and the next
    /// key frame gets it started.
    pub(crate) async fn send(&mut self, frame: &EncodedFrame) -> Result<(), String> {
        let Some(payload_type) = self.payload_type else {
            return Ok(());
        };

        // The packetizer advances AFTER a sample. Advance with an empty
        // sample first, then stamp this frame at its actual PTS. This keeps
        // dropped-frame gaps without adding one frame of buffering latency.
        if let Some(previous) = self.last_pts_ms {
            let duration = u64::try_from(frame.pts_ms.saturating_sub(previous))
                .ok()
                .filter(|delta| *delta > 0)
                .map_or(self.frame_duration, Duration::from_millis);
            self.track
                .sample_writer(self.ssrc, payload_type)
                .write_sample(&Sample {
                    duration,
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("could not advance video timestamp: {e}"))?;
        }
        self.last_pts_ms = Some(frame.pts_ms);
        self.track
            .sample_writer(self.ssrc, payload_type)
            .write_sample(&Sample {
                data: frame.bytes.clone().into(),
                duration: Duration::ZERO,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("could not write sample: {e}"))
    }

    /// Track feedback is consumed in a separate task so writes never block it.
    pub(crate) fn track(&self) -> Arc<TrackLocalStaticSample> {
        Arc::clone(&self.track)
    }

    /// Close the connection.
    pub(crate) async fn close(&self) {
        if let Err(e) = self.peer.close().await {
            tracing::debug!("closing screen-share peer connection failed: {e}");
        }
    }
}

#[cfg(test)]
mod integration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_names_map_to_the_codec_they_produce() {
        // Getting this wrong means offering a codec we do not emit, and a
        // payload-type mismatch is a silent frame drop rather than an error.
        for (id, expected) in [
            ("h264_nvenc", Some(Codec::H264)),
            ("libopenh264", Some(Codec::H264)),
            ("hevc_qsv", Some(Codec::Hevc)),
            ("av1_nvenc", Some(Codec::Av1)),
            ("vp9_something", None),
        ] {
            assert_eq!(Codec::from_encoder(id), expected, "{id}");
        }
    }

    #[test]
    fn h264_advertises_the_profile_the_encoders_actually_emit() {
        // str0m matches H.264 on profile and packetization-mode exactly and
        // drops non-matching frames without logging an error, so this fmtp
        // line has to agree with the High profile stage 1 configures.
        let fmtp = Codec::H264.codec_parameters().rtp_codec.sdp_fmtp_line;
        assert!(fmtp.contains("profile-level-id=64001f"), "{fmtp}");
        assert!(fmtp.contains("packetization-mode=1"), "{fmtp}");
    }
}
