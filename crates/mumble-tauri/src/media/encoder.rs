//! Video encoder catalogue, probing and result cache.
//!
//! Compile-time availability tells us nothing about whether an encoder
//! actually works: `h264_amf` is always built in, but fails immediately on
//! a machine without AMD drivers.  So we probe - open each candidate and
//! encode one frame - and keep only the ones that produce a key frame.
//!
//! Probing runs in a **subprocess** (`--probe-video-encoders`), because a
//! broken or absent driver can hang or crash inside the vendor DLL and we
//! do not want that to take the app with it.  Results are cached in the app
//! data directory, keyed by a GPU signature, so the cost is paid once per
//! machine and re-paid automatically after a GPU or driver change.
//!
//! Without the `native-screenshare` feature every entry reports
//! `available: false` and [`EncoderReport::supported`] is `false`.

use serde::{Deserialize, Serialize};

use super::settings::EncoderChoice;

/// Hidden CLI flag that puts the process into probe mode.
pub(crate) const PROBE_FLAG: &str = "--probe-video-encoders";

/// How long the parent waits for the probe subprocess before killing it.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// File name of the probe-result cache inside the app data directory.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
const CACHE_FILE: &str = "video-encoders.json";

/// One encoder candidate in the catalogue.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate {
    /// `FFmpeg` encoder name, e.g. `h264_nvenc`.
    pub(crate) id: &'static str,
    /// Human-readable vendor / API label for the settings UI.
    pub(crate) display_name: &'static str,
    /// Codec produced: `h264`, `hevc` or `av1`.
    pub(crate) codec: &'static str,
    /// Whether this is a true hardware encoder.  Automatic selection
    /// prefers these; the others are fallbacks.
    pub(crate) is_hardware: bool,
}

/// Every encoder we know how to probe, grouped by codec.
///
/// Within each codec the order is the priority automatic selection walks:
/// the three vendor encoders first, then the generic D3D12 encoder, then
/// Media Foundation (which may resolve to a software MFT), and for H.264 a
/// BSD-licensed software encoder as a guaranteed fallback.
///
/// H.264 comes first as a matter of documentation, but automatic selection
/// does not rely on that - see [`EncoderReport::auto_pick`], which will only
/// ever choose H.264.  HEVC and AV1 are enumerated so a user can pick one
/// deliberately, not so we can default to them.
pub(crate) const CANDIDATES: &[Candidate] = &[
    // --- H.264: universally decodable, and the only codec we auto-select.
    Candidate {
        id: "h264_nvenc",
        display_name: "H.264 - NVIDIA NVENC",
        codec: "h264",
        is_hardware: true,
    },
    Candidate {
        id: "h264_amf",
        display_name: "H.264 - AMD AMF",
        codec: "h264",
        is_hardware: true,
    },
    Candidate {
        id: "h264_qsv",
        display_name: "H.264 - Intel Quick Sync",
        codec: "h264",
        is_hardware: true,
    },
    Candidate {
        id: "h264_d3d12va",
        display_name: "H.264 - Direct3D 12 Video Encode",
        codec: "h264",
        is_hardware: true,
    },
    Candidate {
        id: "h264_mf",
        display_name: "H.264 - Media Foundation",
        codec: "h264",
        is_hardware: false,
    },
    Candidate {
        id: "libopenh264",
        display_name: "H.264 - OpenH264 (software)",
        codec: "h264",
        is_hardware: false,
    },
    // --- HEVC: needs `enable_h265` on the server and is not decodable by
    //     every browser.  Opt-in only.
    Candidate {
        id: "hevc_nvenc",
        display_name: "HEVC - NVIDIA NVENC",
        codec: "hevc",
        is_hardware: true,
    },
    Candidate {
        id: "hevc_amf",
        display_name: "HEVC - AMD AMF",
        codec: "hevc",
        is_hardware: true,
    },
    Candidate {
        id: "hevc_qsv",
        display_name: "HEVC - Intel Quick Sync",
        codec: "hevc",
        is_hardware: true,
    },
    Candidate {
        id: "hevc_d3d12va",
        display_name: "HEVC - Direct3D 12 Video Encode",
        codec: "hevc",
        is_hardware: true,
    },
    Candidate {
        id: "hevc_mf",
        display_name: "HEVC - Media Foundation",
        codec: "hevc",
        is_hardware: false,
    },
    // --- AV1: in str0m's default codec set, so the SFU forwards it, but
    //     hardware support is limited to recent GPUs.  Opt-in only.
    Candidate {
        id: "av1_nvenc",
        display_name: "AV1 - NVIDIA NVENC",
        codec: "av1",
        is_hardware: true,
    },
    Candidate {
        id: "av1_amf",
        display_name: "AV1 - AMD AMF",
        codec: "av1",
        is_hardware: true,
    },
    Candidate {
        id: "av1_qsv",
        display_name: "AV1 - Intel Quick Sync",
        codec: "av1",
        is_hardware: true,
    },
    Candidate {
        id: "av1_d3d12va",
        display_name: "AV1 - Direct3D 12 Video Encode",
        codec: "av1",
        is_hardware: true,
    },
    Candidate {
        id: "av1_mf",
        display_name: "AV1 - Media Foundation",
        codec: "av1",
        is_hardware: false,
    },
];

/// The codec automatic selection is allowed to choose.
///
/// Deliberately narrow.  The SFU forwards whatever it negotiates, but a
/// mismatch is a *silent* frame drop (`no_pt_match`, see `TODO.md` 0.1), and
/// HEVC additionally needs the server to call `enable_h265`.  Defaulting to
/// anything but H.264 would risk a black stream with no error anywhere.
pub(crate) const AUTO_CODEC: &str = "h264";

/// One catalogue entry plus what probing found out about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EncoderInfo {
    /// `FFmpeg` encoder name, used as the settings value.
    pub(crate) id: String,
    /// Human-readable label for the settings UI.
    pub(crate) display_name: String,
    /// Codec produced.
    pub(crate) codec: String,
    /// Whether this is a true hardware encoder.
    pub(crate) is_hardware: bool,
    /// Whether it opened and produced a key frame on this machine.
    pub(crate) available: bool,
    /// Why it is unavailable, taken from `FFmpeg`'s own error output, e.g.
    /// `DLL amfrt64.dll failed to open`.  `None` when available.
    pub(crate) detail: Option<String>,
}

impl EncoderInfo {
    /// Build an entry marked unavailable for the stated reason.
    fn unavailable(candidate: &Candidate, reason: impl Into<String>) -> Self {
        Self {
            id: candidate.id.to_owned(),
            display_name: candidate.display_name.to_owned(),
            codec: candidate.codec.to_owned(),
            is_hardware: candidate.is_hardware,
            available: false,
            detail: Some(reason.into()),
        }
    }
}

/// The payload the frontend renders the encoder picker from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EncoderReport {
    /// Whether this build can encode natively at all.  `false` on
    /// non-Windows targets and in builds without `native-screenshare`; the
    /// frontend then keeps the existing browser WebRTC path.
    pub(crate) supported: bool,
    /// Every catalogue entry, in priority order, available or not.
    pub(crate) encoders: Vec<EncoderInfo>,
    /// Which encoder `"auto"` resolves to right now, if any.
    pub(crate) auto_selected: Option<String>,
    /// GPU signature these results belong to; the cache key.
    pub(crate) gpu_signature: String,
    /// When the probe ran, as a Unix timestamp in seconds.
    pub(crate) probed_at: i64,
}

impl EncoderReport {
    /// A report for builds and platforms without native encoding.
    #[cfg(any(test, not(all(target_os = "windows", feature = "native-screenshare"))))]
    fn unsupported(reason: &str) -> Self {
        Self {
            supported: false,
            encoders: CANDIDATES
                .iter()
                .map(|c| EncoderInfo::unavailable(c, reason))
                .collect(),
            auto_selected: None,
            gpu_signature: String::new(),
            probed_at: 0,
        }
    }

    /// The encoder `"auto"` picks: the first available [`AUTO_CODEC`]
    /// hardware encoder in catalogue order, else the first available
    /// [`AUTO_CODEC`] encoder of any kind.
    ///
    /// Restricted to one codec on purpose.  Picking HEVC or AV1 just because
    /// it happens to be available would risk a silently black stream: the
    /// SFU drops frames it cannot match a payload type for without logging an
    /// error, and HEVC needs a server-side opt-in.  A user who wants those
    /// selects them explicitly.
    #[cfg(any(test, all(target_os = "windows", feature = "native-screenshare")))]
    fn auto_pick(encoders: &[EncoderInfo]) -> Option<String> {
        let usable = || encoders.iter().filter(|e| e.available && e.codec == AUTO_CODEC);
        usable()
            .find(|e| e.is_hardware)
            .or_else(|| usable().next())
            .map(|e| e.id.clone())
    }

    /// Turn a user's [`EncoderChoice`] into a concrete encoder name.
    ///
    /// A choice that is no longer available - the user swapped GPUs, or
    /// uninstalled a driver - silently falls back to automatic selection
    /// and reports that, so the UI can tell the user once.
    pub(crate) fn resolve_choice(&self, choice: &EncoderChoice) -> Result<Selection, String> {
        let requested = choice.id();
        if let Some(id) = requested {
            if self.encoders.iter().any(|e| e.id == id && e.available) {
                return Ok(Selection { id: id.to_owned(), fell_back: false });
            }
        }
        let picked = self
            .auto_selected
            .clone()
            .ok_or_else(|| format!("no working {AUTO_CODEC} encoder found on this machine"))?;
        Ok(Selection { id: picked, fell_back: requested.is_some() })
    }
}

/// The outcome of resolving an [`EncoderChoice`] against a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Selection {
    /// The `FFmpeg` encoder the pipeline will open.
    pub(crate) id: String,
    /// `true` when the user's explicit choice was unavailable and
    /// automatic selection stepped in.
    pub(crate) fell_back: bool,
}

/// One candidate's probe outcome, as reported by the subprocess.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeResult {
    /// `FFmpeg` encoder name.
    pub(crate) id: String,
    /// Whether it opened and produced a key frame.
    pub(crate) available: bool,
    /// `FFmpeg`'s error text when it did not.
    pub(crate) detail: Option<String>,
    /// How long the attempt took; a hardware encoder's first open pays
    /// driver-initialisation cost (~300 ms for NVENC on a warm machine).
    pub(crate) elapsed_ms: u64,
}

/// What the probe subprocess prints to stdout: exactly one JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeOutput {
    /// One entry per candidate, in catalogue order.
    pub(crate) results: Vec<ProbeResult>,
}

/// Merge probe results onto the catalogue, preserving catalogue order.
///
/// Candidates the subprocess did not report on stay unavailable, so a
/// truncated or partial probe can never make an encoder look usable.
#[cfg(any(test, all(target_os = "windows", feature = "native-screenshare")))]
fn merge(results: &[ProbeResult]) -> Vec<EncoderInfo> {
    CANDIDATES
        .iter()
        .map(|candidate| {
            let found = results.iter().find(|r| r.id == candidate.id);
            match found {
                Some(r) if r.available => EncoderInfo {
                    id: candidate.id.to_owned(),
                    display_name: candidate.display_name.to_owned(),
                    codec: candidate.codec.to_owned(),
                    is_hardware: candidate.is_hardware,
                    available: true,
                    detail: None,
                },
                Some(r) => EncoderInfo::unavailable(
                    candidate,
                    r.detail.clone().unwrap_or_else(|| "unavailable".to_owned()),
                ),
                None => EncoderInfo::unavailable(candidate, "not probed"),
            }
        })
        .collect()
}

/// Probe results for this machine, using the cache when it is still valid.
///
/// `data_dir` is the app data directory (where `preferences.json` lives).
/// `refresh` forces a re-probe even when the cache matches.
///
/// Never fails: a probe that crashes, hangs or returns garbage yields a
/// report where every encoder is unavailable with the reason attached, so
/// the settings page can always render something truthful.
pub(crate) fn report(data_dir: &std::path::Path, refresh: bool) -> EncoderReport {
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    {
        native::report(data_dir, refresh)
    }
    #[cfg(not(all(target_os = "windows", feature = "native-screenshare")))]
    {
        let _ = (data_dir, refresh);
        EncoderReport::unsupported(if cfg!(target_os = "windows") {
            "this build was compiled without the native-screenshare feature"
        } else {
            "native screen-share encoding is Windows-only"
        })
    }
}

/// Cache persistence.  Only the native path uses it.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
mod cache {
    use super::{EncoderReport, CACHE_FILE};

    /// Read the cached report, or `None` when absent or unreadable.
    ///
    /// A corrupt cache is a cache miss, never an error: the probe simply
    /// runs again and overwrites it.
    pub(super) fn load(data_dir: &std::path::Path) -> Option<EncoderReport> {
        let raw = std::fs::read_to_string(data_dir.join(CACHE_FILE)).ok()?;
        match serde_json::from_str(&raw) {
            Ok(report) => Some(report),
            Err(e) => {
                tracing::debug!("discarding unreadable encoder cache: {e}");
                None
            }
        }
    }

    /// Write the report to the cache, logging but not propagating failures.
    ///
    /// The app data directory may not exist yet on a first run, so create
    /// it rather than losing the probe result we just paid for.
    pub(super) fn store(data_dir: &std::path::Path, report: &EncoderReport) {
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            tracing::warn!("could not create {}: {e}", data_dir.display());
            return;
        }
        let path = data_dir.join(CACHE_FILE);
        let encoded = match serde_json::to_string_pretty(report) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!("could not serialise encoder cache: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&path, encoded) {
            tracing::warn!("could not write encoder cache to {}: {e}", path.display());
        }
    }
}

/// Subprocess orchestration, Windows + `native-screenshare` only.
#[cfg(all(target_os = "windows", feature = "native-screenshare"))]
mod native {
    use super::{
        cache, merge, EncoderReport, ProbeOutput, CANDIDATES, PROBE_FLAG, PROBE_TIMEOUT,
    };

    /// Poll interval while waiting for the probe subprocess to exit.
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

    /// Probe (or reuse cached results) for the current GPU configuration.
    pub(super) fn report(data_dir: &std::path::Path, refresh: bool) -> EncoderReport {
        let signature = crate::media::gpu::signature();

        if !refresh {
            if let Some(cached) = cache::load(data_dir) {
                if cached.supported && cached.gpu_signature == signature {
                    tracing::debug!("using cached encoder probe for {signature}");
                    return cached;
                }
            }
        }

        let encoders = match spawn_probe() {
            Ok(output) => {
                for r in &output.results {
                    tracing::info!(
                        "encoder probe {}: available={} ({} ms) {}",
                        r.id,
                        r.available,
                        r.elapsed_ms,
                        r.detail.as_deref().unwrap_or("")
                    );
                }
                merge(&output.results)
            }
            Err(e) => {
                tracing::warn!("encoder probe failed: {e}");
                CANDIDATES
                    .iter()
                    .map(|c| super::EncoderInfo::unavailable(c, e.clone()))
                    .collect()
            }
        };

        let report = EncoderReport {
            supported: true,
            auto_selected: EncoderReport::auto_pick(&encoders),
            encoders,
            gpu_signature: signature,
            probed_at: chrono::Utc::now().timestamp(),
        };
        cache::store(data_dir, &report);
        report
    }

    /// Run this executable in probe mode and parse its JSON output.
    fn spawn_probe() -> Result<ProbeOutput, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

        let mut command = std::process::Command::new(exe);
        let _ = command
            .arg(PROBE_FLAG)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped());

        // Probing spawns no window: the child never creates one, but
        // without this flag Windows briefly flashes a console.
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            /// `CREATE_NO_WINDOW`
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let _ = command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|e| format!("spawn: {e}"))?;
        let stdout = child.stdout.take().ok_or_else(|| "no stdout pipe".to_owned())?;
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let mut stdout = stdout;
            let _ = stdout.read_to_string(&mut buf);
            buf
        });

        wait_with_timeout(&mut child)?;

        let raw = reader.join().map_err(|_| "probe reader thread panicked".to_owned())?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("could not parse probe output: {e} (got {} bytes)", raw.len()))
    }

    /// Wait for the child, killing it if it outlives [`PROBE_TIMEOUT`].
    ///
    /// A driver that deadlocks inside its own DLL never returns, which is
    /// the whole reason probing is a subprocess.
    fn wait_with_timeout(child: &mut std::process::Child) -> Result<(), String> {
        let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(e) => return Err(format!("try_wait: {e}")),
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "probe timed out after {} s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Handle [`PROBE_FLAG`] if present, and report whether we did.
///
/// Must be called before any other startup work - notably before
/// single-instance handling, which would otherwise forward our arguments to
/// the already-running app and exit.  In probe mode stdout carries exactly
/// one JSON object ([`ProbeOutput`]) and nothing else; diagnostics go to
/// stderr.
pub(crate) fn handle_probe_cli() -> bool {
    if !std::env::args().any(|arg| arg == PROBE_FLAG) {
        return false;
    }

    let output = ProbeOutput { results: probe_all() };
    let encoded = serde_json::to_string(&output)
        .unwrap_or_else(|_| String::from(r#"{"results":[]}"#));

    // Deliberately not `println!`: it panics if the parent closed the pipe,
    // and a probe that cannot report is not worth crashing over.
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(encoded.as_bytes());
    let _ = lock.flush();
    true
}

/// Probe every candidate in catalogue order.
fn probe_all() -> Vec<ProbeResult> {
    #[cfg(all(target_os = "windows", feature = "native-screenshare"))]
    {
        super::ffmpeg::probe::probe_all()
    }
    #[cfg(not(all(target_os = "windows", feature = "native-screenshare")))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "acceptable in test code")]

    use super::*;

    fn result(id: &str, available: bool) -> ProbeResult {
        ProbeResult {
            id: id.to_owned(),
            available,
            detail: if available { None } else { Some("nope".to_owned()) },
            elapsed_ms: 1,
        }
    }

    #[test]
    fn merge_keeps_catalogue_order_and_size() {
        let merged = merge(&[result("h264_mf", true)]);
        assert_eq!(merged.len(), CANDIDATES.len());
        let ids: Vec<&str> = merged.iter().map(|e| e.id.as_str()).collect();
        let expected: Vec<&str> = CANDIDATES.iter().map(|c| c.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn unreported_candidates_are_unavailable() {
        let merged = merge(&[]);
        assert!(merged.iter().all(|e| !e.available));
        assert!(merged.iter().all(|e| e.detail.as_deref() == Some("not probed")));
    }

    #[test]
    fn auto_prefers_hardware_over_software() {
        // Only software encoders present -> falls back to one of those.
        let software_only = merge(&[result("libopenh264", true), result("h264_mf", true)]);
        assert_eq!(
            EncoderReport::auto_pick(&software_only).as_deref(),
            Some("h264_mf"),
            "catalogue order puts h264_mf before libopenh264"
        );

        // Hardware present -> wins regardless of catalogue position.
        let with_hardware = merge(&[result("libopenh264", true), result("h264_qsv", true)]);
        assert_eq!(
            EncoderReport::auto_pick(&with_hardware).as_deref(),
            Some("h264_qsv")
        );
    }

    #[test]
    fn auto_is_none_when_nothing_works() {
        assert_eq!(EncoderReport::auto_pick(&merge(&[])), None);
    }

    #[test]
    fn auto_never_picks_hevc_or_av1() {
        // Every non-H.264 encoder working, no H.264 at all: automatic
        // selection must decline rather than hand back a codec the SFU may
        // silently drop.
        let non_h264: Vec<ProbeResult> = CANDIDATES
            .iter()
            .filter(|c| c.codec != AUTO_CODEC)
            .map(|c| result(c.id, true))
            .collect();
        assert!(!non_h264.is_empty(), "catalogue should contain HEVC and AV1");
        assert_eq!(EncoderReport::auto_pick(&merge(&non_h264)), None);
    }

    #[test]
    fn auto_prefers_h264_software_over_other_codec_hardware() {
        // libopenh264 is software, hevc_nvenc is hardware. H.264 still wins,
        // because codec compatibility outranks hardware acceleration.
        let merged = merge(&[result("hevc_nvenc", true), result("libopenh264", true)]);
        assert_eq!(EncoderReport::auto_pick(&merged).as_deref(), Some("libopenh264"));
    }

    #[test]
    fn explicit_non_h264_choice_is_still_honoured() {
        let encoders = merge(&[result("hevc_nvenc", true), result("h264_nvenc", true)]);
        let report = EncoderReport {
            supported: true,
            auto_selected: EncoderReport::auto_pick(&encoders),
            encoders,
            gpu_signature: "test".to_owned(),
            probed_at: 0,
        };
        let picked = report
            .resolve_choice(&EncoderChoice::Id("hevc_nvenc".to_owned()))
            .unwrap();
        assert_eq!(picked.id, "hevc_nvenc");
        assert!(!picked.fell_back, "an available explicit choice must be kept");
    }

    #[test]
    fn every_candidate_has_a_known_codec() {
        for c in CANDIDATES {
            assert!(
                matches!(c.codec, "h264" | "hevc" | "av1"),
                "{} has unexpected codec {}",
                c.id,
                c.codec
            );
        }
    }

    fn report_with(available: &[&str]) -> EncoderReport {
        let encoders = merge(
            &available.iter().map(|id| result(id, true)).collect::<Vec<_>>(),
        );
        EncoderReport {
            supported: true,
            auto_selected: EncoderReport::auto_pick(&encoders),
            encoders,
            gpu_signature: "test".to_owned(),
            probed_at: 0,
        }
    }

    #[test]
    fn explicit_available_choice_is_honoured() {
        let report = report_with(&["h264_nvenc", "h264_mf"]);
        let picked = report
            .resolve_choice(&EncoderChoice::Id("h264_mf".to_owned()))
            .unwrap();
        assert_eq!(picked.id, "h264_mf");
        assert!(!picked.fell_back);
    }

    #[test]
    fn unavailable_choice_falls_back_and_says_so() {
        let report = report_with(&["h264_nvenc"]);
        let picked = report
            .resolve_choice(&EncoderChoice::Id("h264_qsv".to_owned()))
            .unwrap();
        assert_eq!(picked.id, "h264_nvenc");
        assert!(picked.fell_back, "the UI needs to know the choice was dropped");
    }

    #[test]
    fn auto_choice_does_not_count_as_fallback() {
        let picked = report_with(&["h264_nvenc"])
            .resolve_choice(&EncoderChoice::Auto)
            .unwrap();
        assert_eq!(picked.id, "h264_nvenc");
        assert!(!picked.fell_back);
    }

    #[test]
    fn resolving_without_any_encoder_is_an_error() {
        assert!(report_with(&[]).resolve_choice(&EncoderChoice::Auto).is_err());
    }

    #[test]
    fn unsupported_report_marks_everything_unavailable() {
        let report = EncoderReport::unsupported("no dice");
        assert!(!report.supported);
        assert_eq!(report.encoders.len(), CANDIDATES.len());
        assert!(report.encoders.iter().all(|e| !e.available));
        assert_eq!(report.auto_selected, None);
    }
}
