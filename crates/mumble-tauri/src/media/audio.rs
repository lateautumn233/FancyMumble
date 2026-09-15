//! Screen-share audio packets and the existing Opus encoder adapter.

/// WebRTC Opus always uses a 48 kHz RTP clock.
pub(crate) const SAMPLE_RATE: u32 = 48_000;
/// Samples per channel in a 20 ms packet.
#[cfg(any(test, all(target_os = "windows", feature = "native-screenshare")))]
pub(crate) const FRAME_SAMPLES: usize = 960;

/// One stereo Opus packet, timestamped independently of network delivery.
#[derive(Debug, Clone)]
pub(crate) struct Packet {
    pub(crate) bytes: Vec<u8>,
    pub(crate) pts_samples: u64,
}

#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
pub(crate) mod capture;

#[cfg(any(test, all(target_os = "windows", feature = "native-screenshare")))]
mod encoding {
    use mumble_protocol::audio::encoder::{
        AudioEncoder, OpusApplication, OpusEncoder, OpusEncoderConfig,
    };
    use mumble_protocol::audio::sample::{AudioFormat, AudioFrame, SampleFormat};

    use super::{Packet, FRAME_SAMPLES, SAMPLE_RATE};

    const FORMAT: AudioFormat = AudioFormat {
        sample_rate: SAMPLE_RATE,
        channels: 2,
        sample_format: SampleFormat::F32,
    };

    /// Process loopback does not supply a usable device-position counter.
    /// Count converted samples, using QPC (100 ns units) only to preserve gaps.
    #[derive(Default)]
    pub(super) struct CaptureClock {
        next_samples: u64,
        next_qpc: Option<u64>,
    }

    impl CaptureClock {
        pub(super) fn advance(&mut self, frames: u32, qpc: Option<u64>) -> u64 {
            if let (Some(actual), Some(expected)) = (qpc, self.next_qpc) {
                let gap = actual.saturating_sub(expected);
                // Ignore sub-2 ms scheduling/clock jitter, not actual missing audio.
                if gap > 20_000 {
                    self.next_samples += gap.saturating_mul(u64::from(SAMPLE_RATE)) / 10_000_000;
                }
            }
            self.next_qpc =
                qpc.map(|qpc| qpc + u64::from(frames) * 10_000_000 / u64::from(SAMPLE_RATE));
            let pts = self.next_samples;
            self.next_samples += u64::from(frames);
            pts
        }
    }

    pub(super) struct Encoder {
        opus: OpusEncoder,
        pending: Vec<f32>,
        next_pts: Option<u64>,
    }

    impl Encoder {
        pub(super) fn new() -> Result<Self, String> {
            let config = OpusEncoderConfig {
                bitrate: 128_000,
                application: OpusApplication::Audio,
                frame_size: FRAME_SAMPLES,
                ..Default::default()
            };
            Ok(Self {
                opus: OpusEncoder::new(config, FORMAT).map_err(|e| e.to_string())?,
                pending: Vec::with_capacity(FRAME_SAMPLES * 2),
                next_pts: None,
            })
        }

        /// Keep incomplete packets across device callbacks, but never across a gap.
        pub(super) fn push(
            &mut self,
            samples: &[f32],
            pts: u64,
            mut sink: impl FnMut(Packet),
        ) -> Result<(), String> {
            if !samples.len().is_multiple_of(2) {
                return Err("screen-share audio is not interleaved stereo".to_owned());
            }
            let expected = self
                .next_pts
                .map(|start| start + (self.pending.len() / 2) as u64);
            if expected.is_none_or(|expected| expected.abs_diff(pts) > 1) {
                self.pending.clear();
                self.next_pts = Some(pts);
            }
            self.pending.extend_from_slice(samples);
            while self.pending.len() >= FRAME_SAMPLES * 2 {
                let data = self
                    .pending
                    .drain(..FRAME_SAMPLES * 2)
                    .flat_map(f32::to_ne_bytes)
                    .collect();
                let pts_samples = self.next_pts.unwrap_or(pts);
                let frame = AudioFrame {
                    data,
                    format: FORMAT,
                    sequence: 0,
                    is_silent: false,
                };
                let packet = self.opus.encode(&frame).map_err(|e| e.to_string())?;
                sink(Packet {
                    bytes: packet.data,
                    pts_samples,
                });
                self.next_pts = Some(pts_samples + FRAME_SAMPLES as u64);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumble_protocol::audio::decoder::{AudioDecoder, OpusDecoder};
    use mumble_protocol::audio::sample::{AudioFormat, SampleFormat};

    #[test]
    fn capture_clock_counts_samples_and_preserves_time_across_silence() {
        let mut clock = encoding::CaptureClock::default();
        assert_eq!(clock.advance(480, Some(1_000_000)), 0);
        assert_eq!(clock.advance(480, Some(1_100_010)), 480);
        assert_eq!(clock.advance(480, Some(2_200_010)), 5760);
        assert_eq!(clock.advance(480, None), 6240);
        assert_eq!(clock.advance(480, Some(2_400_010)), 6720);
    }

    #[test]
    fn stereo_opus_preserves_channels_and_packet_boundaries() -> Result<(), String> {
        let mut encoder = encoding::Encoder::new()?;
        let mut packets = Vec::new();
        let samples: Vec<f32> = (0..FRAME_SAMPLES * 3)
            .flat_map(|i| {
                let phase = i as f32 * std::f32::consts::TAU / SAMPLE_RATE as f32;
                [(phase * 440.0).sin() * 0.3, (phase * 880.0).sin() * 0.3]
            })
            .collect();
        encoder.push(&samples[..600], 0, |packet| packets.push(packet))?;
        assert!(packets.is_empty());
        encoder.push(&samples[600..], 300, |packet| packets.push(packet))?;
        assert_eq!(
            packets.iter().map(|p| p.pts_samples).collect::<Vec<_>>(),
            vec![0, 960, 1920]
        );
        let format = AudioFormat {
            sample_rate: SAMPLE_RATE,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut decoder = OpusDecoder::new(format).map_err(|e| e.to_string())?;
        let mut difference = 0.0;
        for packet in packets {
            let decoded = decoder
                .decode(&mumble_protocol::audio::encoder::EncodedPacket {
                    data: packet.bytes,
                    sequence: 0,
                    frame_samples: FRAME_SAMPLES as u32,
                })
                .map_err(|e| e.to_string())?;
            assert_eq!(decoded.sample_count(), FRAME_SAMPLES);
            for pair in decoded.as_f32_samples().chunks_exact(2) {
                difference += (pair[0] - pair[1]).abs();
            }
        }
        assert!(difference > 10.0, "stereo channels must remain distinct");
        Ok(())
    }

    #[test]
    fn capture_gaps_discard_partial_audio_without_compressing_time() -> Result<(), String> {
        let mut encoder = encoding::Encoder::new()?;
        let mut packets = Vec::new();
        encoder.push(&vec![0.0; 800], 0, |p| packets.push(p))?;
        encoder.push(&vec![0.0; FRAME_SAMPLES * 2], 48_000, |p| packets.push(p))?;
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].pts_samples, 48_000);
        Ok(())
    }
}
