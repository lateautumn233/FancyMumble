//! Hidden CLI self-test for the capture and encode pipeline.
//!
//! Stage 2 has no transport yet, and the frontend picker belongs to stage 4,
//! so there is otherwise no way to run the pipeline against real hardware.
//! This runs it for a few seconds on a chosen display and prints what came
//! out - packet sizes, key frames, encode times, and whether the parameter
//! sets are being repeated with each key frame the way the transport stage
//! will need them to be.
//!
//! Not a unit test: it needs a GPU, a real desktop, and a compositor that is
//! actually drawing.  CI has none of those.
//!
//! ```text
//! mumble-tauri.exe --capture-selftest                       # 60 frames, defaults
//! mumble-tauri.exe --capture-selftest --frames 120 --fps 60
//! mumble-tauri.exe --capture-selftest --backend gfxcapture --resolution 720
//! mumble-tauri.exe --capture-selftest --encoder libopenh264
//! mumble-tauri.exe --capture-selftest --list
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::pipeline::{EncodedFrame, PipelineConfig};
use super::settings::{CaptureBackend, EncoderChoice, Resolution, ScreenShareSettings};

/// Hidden CLI flag that runs the self-test.
pub(crate) const SELFTEST_FLAG: &str = "--capture-selftest";

/// How long to wait for the pipeline to produce the requested frames before
/// giving up.  Generous: a first hardware-encoder open pays driver
/// initialisation, and window capture pays a WGC session setup.
const OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Handle [`SELFTEST_FLAG`] if present, and report whether we did.
///
/// Called from the same place as the encoder probe, before any other startup
/// work.
pub(crate) fn handle_cli() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|arg| arg == SELFTEST_FLAG) {
        return false;
    }

    // The app's own logging is set up well after this point, and without a
    // subscriber every `FFmpeg` error - the one thing a diagnostic tool needs
    // to show - would be discarded.  Errors go to stderr so that stdout stays
    // the report.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();

    let options = Options::parse(&args);
    if let Err(e) = run(&options) {
        eprintln!("capture self-test failed: {e}");
    }
    true
}

/// What the self-test was asked to do.
#[derive(Debug)]
struct Options {
    /// Only list the displays and exit.
    list: bool,
    /// Which display to capture, by index into the enumeration.
    display: usize,
    /// A window handle to capture instead of a display.
    hwnd: Option<u64>,
    /// How many encoded frames to collect.
    frames: u64,
    /// Which capture backend to use.
    backend: CaptureBackend,
    /// Which encoder to use, or automatic.
    encoder: EncoderChoice,
    /// Target frame rate.
    fps: u32,
    /// Target resolution.
    resolution: Resolution,
}

impl Options {
    /// Read the options from the command line, falling back to defaults.
    fn parse(args: &[String]) -> Self {
        let value = |name: &str| -> Option<&String> {
            args.iter().position(|a| a == name).and_then(|i| args.get(i + 1))
        };
        let number = |name: &str| -> Option<u64> { value(name).and_then(|v| v.parse().ok()) };

        let backend = match value("--backend").map(String::as_str) {
            Some("gfxcapture" | "wgc") => CaptureBackend::Gfxcapture,
            _ => CaptureBackend::Ddagrab,
        };
        let resolution = match number("--resolution") {
            Some(0) | None => Resolution::Native,
            Some(lines) => Resolution::Preset { lines: u32::try_from(lines).unwrap_or(1080) },
        };
        let encoder = value("--encoder")
            .map_or(EncoderChoice::Auto, |id| EncoderChoice::Id(id.clone()));

        Self {
            list: args.iter().any(|a| a == "--list"),
            display: usize::try_from(number("--display").unwrap_or(0)).unwrap_or(0),
            hwnd: number("--hwnd").filter(|h| *h != 0),
            frames: number("--frames").unwrap_or(60).max(1),
            backend,
            encoder,
            fps: u32::try_from(number("--fps").unwrap_or(30)).unwrap_or(30).clamp(5, 120),
            resolution,
        }
    }

    /// The settings these options describe.
    fn settings(&self) -> ScreenShareSettings {
        ScreenShareSettings {
            capture: self.backend,
            encoder: self.encoder.clone(),
            resolution: self.resolution,
            fps: self.fps,
            bitrate_kbps: None,
        }
    }
}

/// What the run produced, for printing.
#[derive(Debug, Default)]
struct Collected {
    /// Total packets seen.
    packets: u64,
    /// Total bytes seen.
    bytes: u64,
    /// Key frames seen.
    key_frames: u64,
    /// Key frames that carried their own parameter sets.
    key_frames_with_headers: u64,
    /// The first few packets, described.
    first: Vec<String>,
    /// The last timestamp seen.
    last_pts: i64,
}

/// Run the self-test.
///
/// The settings are validated first, on every platform, so a nonsensical
/// command line is reported as such rather than as a capture failure.
fn run(options: &Options) -> Result<(), String> {
    let settings = options.settings();
    settings.validate()?;

    let displays = super::display::enumerate()?;
    println!("displays:");
    for (index, d) in displays.iter().enumerate() {
        println!(
            "  [{index}] {} {}x{} adapter={} output_idx={} hmonitor={:#x}{}",
            d.device_name,
            d.size.width,
            d.size.height,
            d.adapter_index,
            d.output_index,
            d.hmonitor,
            if d.is_primary { " (primary)" } else { "" }
        );
    }
    if options.list {
        return Ok(());
    }

    let source = match options.hwnd {
        Some(hwnd) => super::capture::CaptureSource::Window { hwnd },
        None => displays
            .get(options.display)
            .ok_or_else(|| format!("no display at index {}", options.display))?
            .source(),
    };

    let encoder_id = pick_encoder(options)?;
    let config = PipelineConfig { source, settings, encoder_id, draw_cursor: true };
    capture_and_report(&config, options.frames)
}

/// Resolve the encoder choice against the probe results.
fn pick_encoder(options: &Options) -> Result<String, String> {
    // The self-test runs before the Tauri app exists, so there is no app data
    // directory to cache into; a temporary one keeps the probe from writing
    // into the real cache with a half-configured process.
    let cache_dir = std::env::temp_dir().join("fancy-mumble-selftest");
    let report = super::encoder::report(&cache_dir, false);
    if !report.supported {
        return Err("encoder probing reported no support".to_owned());
    }
    let selection = report.resolve_choice(&options.encoder)?;
    if selection.fell_back {
        println!("note: {:?} is unavailable, using {}", options.encoder, selection.id);
    }
    Ok(selection.id)
}

/// Start the pipeline, collect `wanted` frames, and print a summary.
fn capture_and_report(config: &PipelineConfig, wanted: u64) -> Result<(), String> {
    let collected = Arc::new(Mutex::new(Collected::default()));
    let count = Arc::new(AtomicU64::new(0));

    let sink_collected = Arc::clone(&collected);
    let sink_count = Arc::clone(&count);
    let sink = Box::new(move |frame: EncodedFrame| {
        let index = sink_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = sink_collected.lock() {
            c.record(index, &frame);
        }
    });

    println!(
        "starting: backend={:?} encoder={} fps={} resolution={:?}",
        config.settings.capture, config.encoder_id, config.settings.fps, config.settings.resolution
    );
    let started = Instant::now();
    let handle = super::pipeline::start(config, sink)?;
    let encoding = handle.encoding();
    println!(
        "started in {} ms: {} at {}x{}, {} fps, {} kbps",
        started.elapsed().as_millis(),
        handle.encoder_id(),
        encoding.size.width,
        encoding.size.height,
        encoding.fps,
        encoding.bitrate_kbps
    );

    // Ask for a key frame part way through, which is what a receiver's PLI
    // will do once there is a transport.
    let mut asked_for_key = false;
    let deadline = Instant::now() + OVERALL_TIMEOUT;
    while count.load(Ordering::Relaxed) < wanted {
        if handle.stop_reason().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            println!("timed out waiting for {wanted} frames");
            break;
        }
        if !asked_for_key && count.load(Ordering::Relaxed) >= wanted / 2 {
            handle.request_key_frame();
            asked_for_key = true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let elapsed = started.elapsed();
    let stats = handle.stats();
    let reason = handle.stop();

    let summary = collected.lock().map_err(|_| "collector poisoned".to_owned())?;
    summary.print(elapsed, wanted);
    println!(
        "stats: captured={} encoded={} repeated={} dropped={} mean_encode={} us",
        stats.captured, stats.encoded, stats.repeated, stats.dropped, stats.mean_encode_micros
    );
    println!("stopped: {reason:?}");

    if summary.packets == 0 {
        return Err("no packets were produced".to_owned());
    }
    Ok(())
}

impl Collected {
    /// Fold one frame in.
    fn record(&mut self, index: u64, frame: &EncodedFrame) {
        self.packets += 1;
        self.bytes += frame.bytes.len() as u64;
        self.last_pts = frame.pts_ms;
        if frame.is_key {
            self.key_frames += 1;
            if has_parameter_sets(&frame.bytes) {
                self.key_frames_with_headers += 1;
            }
        }
        if (index < 5 || frame.is_key) && self.first.len() < 12 {
            self.first.push(format!(
                "#{index} {} bytes pts={} ms{}{}",
                frame.bytes.len(),
                frame.pts_ms,
                if frame.is_key { " KEY" } else { "" },
                if frame.is_key && has_parameter_sets(&frame.bytes) {
                    " +SPS/PPS"
                } else {
                    ""
                }
            ));
        }
    }

    /// Print the summary.
    fn print(&self, elapsed: Duration, wanted: u64) {
        for line in &self.first {
            println!("  {line}");
        }
        let seconds = elapsed.as_secs_f64().max(0.001);
        println!(
            "collected {}/{wanted} packets in {:.2} s ({:.1} fps, {:.0} kbps)",
            self.packets,
            seconds,
            self.packets as f64 / seconds,
            (self.bytes as f64 * 8.0 / 1000.0) / seconds
        );
        println!(
            "key frames: {} ({} carried parameter sets), last pts {} ms",
            self.key_frames, self.key_frames_with_headers, self.last_pts
        );
        if self.key_frames > 0 && self.key_frames_with_headers < self.key_frames {
            println!(
                "warning: not every key frame carried its parameter sets; \
                 the RTP packetiser will have to insert them"
            );
        }
    }
}

/// Whether an Annex-B bitstream contains a sequence parameter set.
///
/// `AV_CODEC_FLAG2_LOCAL_HEADER` asks the encoder to repeat the parameter sets
/// with every key frame, so a mid-stream receiver can start decoding from one.
/// Whether an encoder honours that is per-encoder, and the transport stage has
/// to insert them itself when it does not - so it is worth measuring here.
///
/// Looks for NAL unit type 7 (H.264 SPS) or 33 (HEVC SPS) after a start code.
/// AV1 has no equivalent to look for, so this reports `false` for it, which
/// only means "unknown" and is why the summary phrases it as a note.
fn has_parameter_sets(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index + 3 < bytes.len() {
        // Both three- and four-byte start codes appear in the same stream.
        let start_code = bytes[index] == 0
            && bytes[index + 1] == 0
            && (bytes[index + 2] == 1
                || (bytes[index + 2] == 0 && bytes.get(index + 3) == Some(&1)));
        if !start_code {
            index += 1;
            continue;
        }
        let header = if bytes[index + 2] == 1 { index + 3 } else { index + 4 };
        let Some(&byte) = bytes.get(header) else { return false };
        // H.264: type is the low five bits.  HEVC: bits 1..6 of the same byte.
        if byte & 0x1F == 7 || (byte >> 1) & 0x3F == 33 {
            return true;
        }
        index = header;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_sps_behind_either_start_code() {
        // Four-byte start code, H.264 SPS (type 7, nal_ref_idc 3 -> 0x67).
        assert!(has_parameter_sets(&[0, 0, 0, 1, 0x67, 0x42]));
        // Three-byte start code.
        assert!(has_parameter_sets(&[0, 0, 1, 0x67]));
        // HEVC SPS is type 33 -> 0x42 in the high bits.
        assert!(has_parameter_sets(&[0, 0, 0, 1, 0x42, 0x01]));
        // A slice on its own is not a parameter set.
        assert!(!has_parameter_sets(&[0, 0, 0, 1, 0x65, 0x88]));
        assert!(!has_parameter_sets(&[]));
        assert!(!has_parameter_sets(&[0, 0, 0, 1]));
    }
}
