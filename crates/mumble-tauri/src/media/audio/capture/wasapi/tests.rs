//! Opt-in hardware verification, with a quiet tone and a separate silent process.

use super::*;
use crate::media::audio::encoding::Encoder;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

struct SilentProcess(Child);
impl Drop for SilentProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn missing_window_audio_never_falls_back_to_the_device() {
    assert!(window_process(0).is_err());
    assert!(window_process(u64::MAX).is_err());
}

#[test]
#[ignore = "plays a quiet tone on the default Windows output device"]
fn device_and_process_loopback_isolate_audio() -> Result<(), Box<dyn std::error::Error>> {
    let _com = Com::new()?;
    let silent = SilentProcess(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut device = Capture::from_client(device_client()?, None)?;
    let mut included = Capture::from_client(process_client(std::process::id())?, None)?;
    let mut excluded = Capture::from_client(process_client(silent.0.id())?, None)?;
    device.start()?;
    included.start()?;
    excluded.start()?;
    let output = tone()?;
    output.play()?;
    let started = Instant::now();
    let mut energy = [0.0_f64; 3];
    let mut counts = [0_usize; 3];
    let mut encoded = [0_usize; 3];
    let mut previous = [None; 3];
    let mut encoders = [Encoder::new()?, Encoder::new()?, Encoder::new()?];
    let mut samples = Vec::new();
    while started.elapsed() < Duration::from_secs(2) {
        for (i, reader) in [&mut device, &mut included, &mut excluded]
            .iter_mut()
            .enumerate()
        {
            if let Some(pts) = reader.read(&mut samples)? {
                energy[i] += samples.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
                counts[i] += samples.len();
                encoders[i].push(&samples, pts, |packet| {
                    assert!(previous[i].is_none_or(|pts| packet.pts_samples > pts));
                    previous[i] = Some(packet.pts_samples);
                    encoded[i] += 1;
                })?;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(output);
    eprintln!("loopback sample counts {counts:?}, energies {energy:?}, Opus packets {encoded:?}");
    assert!(
        encoded[0] > 50 && encoded[1] > 50,
        "both sources must produce continuous Opus packets"
    );
    assert!(
        counts[0] > 48_000 && energy[0] > 0.01,
        "device loopback must hear the tone"
    );
    assert!(
        counts[1] > 48_000 && energy[1] > 0.01,
        "target process loopback must hear the tone"
    );
    assert!(
        energy[2] < energy[1] * 0.001,
        "unrelated process audio must not leak"
    );
    Ok(())
}

fn tone() -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    let default = device.default_output_config()?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        return Err("test needs a float output device".into());
    }
    let config: cpal::StreamConfig = default.into();
    let rate = config.sample_rate as f32;
    let channels = config.channels as usize;
    let mut position = 0_u64;
    Ok(device.build_output_stream(
        &config,
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(channels) {
                let value = (position as f32 * std::f32::consts::TAU * 440.0 / rate).sin() * 0.025;
                frame.fill(value);
                position += 1;
            }
        },
        |error| eprintln!("tone output failed: {error}"),
        None,
    )?)
}
