//! Optional Opus sender on the same peer connection as the screen video.

use super::*;
use crate::media::audio::{Packet, SAMPLE_RATE};
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_OPUS;

pub(super) fn codec_parameters() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: SAMPLE_RATE,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
    }
}

pub(super) struct Sender {
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    ssrc: u32,
    payload_type: Option<PayloadType>,
    last_pts: Option<u64>,
}

impl Sender {
    pub(super) async fn new(peer: &dyn PeerConnection) -> Result<Self, String> {
        let ssrc = rand::random();
        let track = Arc::new(
            TrackLocalStaticSample::new(MediaStreamTrack::new(
                "fancy-screenshare".to_owned(),
                "fancy-screenshare-audio".to_owned(),
                "screen-audio".to_owned(),
                RtpCodecKind::Audio,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: codec_parameters().rtp_codec,
                    ..Default::default()
                }],
            ))
            .map_err(|e| format!("could not create audio track: {e}"))?,
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
            .map_err(|e| format!("could not add audio track: {e}"))?;
        let sender = transceiver
            .sender()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("audio transceiver has no sender")?;
        Ok(Self {
            track,
            sender,
            ssrc,
            payload_type: None,
            last_pts: None,
        })
    }

    pub(super) async fn negotiate(&mut self) -> Result<(), String> {
        let parameters = self
            .sender
            .get_parameters()
            .await
            .map_err(|e| e.to_string())?;
        self.payload_type = parameters
            .rtp_parameters
            .codecs
            .iter()
            .find(|codec| {
                codec
                    .rtp_codec
                    .mime_type
                    .eq_ignore_ascii_case(MIME_TYPE_OPUS)
            })
            .map(|codec| codec.payload_type);
        if self.payload_type.is_none() {
            return Err("the far end negotiated no Opus codec".to_owned());
        }
        Ok(())
    }

    pub(super) async fn send(&mut self, packet: &Packet) -> Result<(), String> {
        let Some(payload_type) = self.payload_type else {
            return Ok(());
        };
        if let Some(previous) = self.last_pts {
            if packet.pts_samples <= previous {
                return Ok(());
            }
            // Empty samples advance the RTP clock before writing, preserving lost audio.
            let delta = packet.pts_samples - previous;
            self.track
                .sample_writer(self.ssrc, payload_type)
                .write_sample(&Sample {
                    duration: Duration::from_secs_f64(delta as f64 / SAMPLE_RATE as f64),
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("could not advance audio timestamp: {e}"))?;
        }
        self.track
            .sample_writer(self.ssrc, payload_type)
            .write_sample(&Sample {
                data: packet.bytes.clone().into(),
                duration: Duration::ZERO,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("could not write audio sample: {e}"))?;
        self.last_pts = Some(packet.pts_samples);
        Ok(())
    }
}
