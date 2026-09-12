//! GPU signature used as the encoder-cache key.
//!
//! Probe results are only valid for the hardware and drivers they were
//! measured on.  Enumerating the DXGI adapters gives us a cheap fingerprint
//! covering both: swapping a GPU changes the LUID and device ID, and a
//! driver update changes the user-mode driver version.  When the signature
//! changes the cache is discarded and the probe re-runs.
//!
//! Failure is not an error - if DXGI is unavailable we return a marker that
//! simply never matches a cached signature, so we re-probe every time
//! instead of trusting stale results.

#![allow(
    unsafe_code,
    reason = "DXGI COM enumeration; all calls are confined to this module"
)]

use sha2::{Digest, Sha256};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_DESC1,
};
use windows::core::Interface;

/// Returned when DXGI cannot be queried, so no cache entry ever matches.
const UNKNOWN: &str = "gpu-unknown";

/// A short, stable fingerprint of this machine's GPU configuration.
///
/// Hashed rather than stored verbatim: the raw text runs to a few hundred
/// bytes on multi-GPU machines and lands in a JSON file the user may read.
pub(crate) fn signature() -> String {
    match describe_adapters() {
        Ok(text) if !text.is_empty() => {
            let digest = Sha256::digest(text.as_bytes());
            format!("gpu-{digest:x}")
        }
        Ok(_) => UNKNOWN.to_owned(),
        Err(e) => {
            tracing::warn!("could not enumerate DXGI adapters: {e}");
            UNKNOWN.to_owned()
        }
    }
}

/// Build the raw descriptor text for every hardware adapter.
fn describe_adapters() -> Result<String, windows::core::Error> {
    // SAFETY: `CreateDXGIFactory1` is a plain COM factory call with no
    // preconditions; the returned interface is reference-counted by the
    // `windows` crate wrapper.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;

    let mut parts = Vec::new();
    for index in 0.. {
        // SAFETY: enumerating by increasing index is the documented
        // contract; the loop stops on the first error, which is
        // `DXGI_ERROR_NOT_FOUND` once the adapters run out.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        if let Some(text) = describe_adapter(&adapter) {
            parts.push(text);
        }
    }
    Ok(parts.join(";"))
}

/// Describe one adapter, or `None` for software adapters and query failures.
fn describe_adapter(adapter: &IDXGIAdapter1) -> Option<String> {
    // SAFETY: `GetDesc1` fills a caller-allocated descriptor; the `windows`
    // wrapper owns that allocation and returns it by value.
    let desc: DXGI_ADAPTER_DESC1 = unsafe { adapter.GetDesc1() }.ok()?;

    let name_len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(&desc.Description[..name_len]);

    // The Basic Render Driver has no video encoder, and its presence must
    // not perturb the signature of the real adapters.
    if name.contains("Basic Render Driver") {
        return None;
    }

    Some(format!(
        "{}|{:04x}:{:04x}|{:x}:{:x}|drv={}",
        name.trim(),
        desc.VendorId,
        desc.DeviceId,
        desc.AdapterLuid.HighPart,
        desc.AdapterLuid.LowPart,
        driver_version(adapter),
    ))
}

/// The user-mode driver version, or `0` when it cannot be determined.
///
/// `CheckInterfaceSupport` with `IDXGIDevice` is the long-standing way to
/// read the UMD version without creating a D3D device.  It is not supported
/// by every adapter, so a failure here is expected and harmless: the rest
/// of the signature still changes when the hardware does.
fn driver_version(adapter: &IDXGIAdapter1) -> i64 {
    // SAFETY: passing a valid interface GUID; the out-parameter is written
    // only on success, which `Result` encodes.
    unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "acceptable in test code")]

    use super::*;

    #[test]
    fn signature_is_stable_and_shaped_as_expected() {
        let first = signature();
        assert!(first.starts_with("gpu-"), "unexpected shape: {first}");
        assert_eq!(first, signature(), "signature must be stable across calls");
    }

    #[test]
    fn describes_at_least_one_adapter_on_this_machine() {
        // Any machine running these tests has a display adapter; if DXGI
        // itself is unavailable the signature degrades to a marker instead,
        // which would silently disable caching, so assert we got real data.
        let text = describe_adapters().expect("DXGI enumeration should succeed");
        assert!(!text.is_empty(), "no hardware adapter found");
        assert!(text.contains("|"), "adapter descriptor looks malformed: {text}");
    }
}
