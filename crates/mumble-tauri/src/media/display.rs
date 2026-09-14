//! Enumerate the displays the capture backends can address.
//!
//! Both backends need a different name for the same monitor: `ddagrab` takes
//! a DXGI output index, `gfxcapture` an `HMONITOR`.  Enumerating DXGI gives
//! both at once, plus the exact pixel size, which is what lets the pipeline
//! build its filter graph in one pass instead of configuring a throwaway
//! graph just to ask how big the source is.
//!
//! The index is per adapter, exactly as `IDXGIAdapter::EnumOutputs` orders
//! them, because that is what `ddagrab` passes it to.  The adapter index
//! comes along so the pipeline can create its D3D11 device on the adapter
//! that actually drives the chosen display - `ddagrab` enumerates outputs on
//! the device's own adapter, so a mismatch would silently capture the wrong
//! screen or fail.

#![allow(
    unsafe_code,
    reason = "DXGI COM enumeration; all calls are confined to this module"
)]

use serde::{Deserialize, Serialize};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, DXGI_OUTPUT_DESC,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use super::capture::CaptureSource;
use super::settings::OutputSize;

/// Makes the calling thread per-monitor DPI aware, and restores whatever it
/// was on drop.
///
/// Without this, DXGI reports a display's desktop rectangle in DPI-virtualised
/// coordinates: a 2560x1440 panel at 150% scaling comes back as 1707x960.  The
/// capture filters work in the panel's real pixels, so the virtualised figure
/// would make the pipeline think it had to scale when it did not - and would
/// make a "1080p" preset a no-op on exactly the machines that need it.
struct DpiAware(Option<DPI_AWARENESS_CONTEXT>);

impl DpiAware {
    /// Enter per-monitor DPI awareness for this thread.
    fn enter() -> Self {
        // SAFETY: a plain thread-local setting with no preconditions; the
        // return value is the previous context, or null if the call failed.
        let previous = unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        };
        if previous.0.is_null() {
            tracing::debug!("could not set per-monitor DPI awareness for display enumeration");
            return Self(None);
        }
        Self(Some(previous))
    }
}

impl Drop for DpiAware {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            // SAFETY: restoring a context this thread was previously in.
            let _ = unsafe { SetThreadDpiAwarenessContext(previous) };
        }
    }
}

/// One display, addressable by either capture backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisplayInfo {
    /// Index of the adapter that drives this display.
    pub(crate) adapter_index: u32,
    /// Index of the output on that adapter - `ddagrab`'s `output_idx`.
    pub(crate) output_index: u32,
    /// The display's `HMONITOR` - `gfxcapture`'s `hmonitor`.
    pub(crate) hmonitor: u64,
    /// Device name, e.g. `\\.\DISPLAY1`.
    pub(crate) device_name: String,
    /// Name of the adapter driving it, for a UI that needs to disambiguate.
    pub(crate) adapter_name: String,
    /// The display's size in pixels, from its desktop coordinates.
    pub(crate) size: OutputSize,
    /// Whether the desktop origin lies on this display, which is the usual
    /// meaning of "primary".
    pub(crate) is_primary: bool,
}

impl DisplayInfo {
    /// The capture source that names this display.
    pub(crate) fn source(&self) -> CaptureSource {
        CaptureSource::Monitor {
            output_index: self.output_index,
            hmonitor: self.hmonitor,
        }
    }
}

/// Every attached display, adapter by adapter.
///
/// An adapter that cannot be described is skipped rather than failing the
/// whole enumeration: one broken virtual adapter should not stop a user from
/// sharing their real screen.
pub(crate) fn enumerate() -> Result<Vec<DisplayInfo>, String> {
    // Real pixels, not DPI-scaled ones - the capture filters work in the
    // former, so the sizes we report have to match.
    let _dpi = DpiAware::enter();

    // SAFETY: a plain COM factory call with no preconditions; the returned
    // interface is reference-counted by the `windows` crate wrapper.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| format!("CreateDXGIFactory1: {e}"))?;

    let mut displays = Vec::new();
    for adapter_index in 0.. {
        // SAFETY: enumerating by increasing index is the documented
        // contract, and the first error is `DXGI_ERROR_NOT_FOUND` once the
        // adapters run out.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
            break;
        };
        collect_outputs(&adapter, adapter_index, &mut displays);
    }

    if displays.is_empty() {
        return Err("no DXGI output found".to_owned());
    }
    Ok(displays)
}

/// Append every output of one adapter.
fn collect_outputs(adapter: &IDXGIAdapter1, adapter_index: u32, out: &mut Vec<DisplayInfo>) {
    let adapter_name = adapter_description(adapter);

    for output_index in 0.. {
        // SAFETY: as with adapters, increasing index until the first error.
        let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
            break;
        };
        if let Some(info) = describe_output(&output, adapter_index, output_index, &adapter_name) {
            out.push(info);
        }
    }
}

/// The adapter's description string, or an empty string if unavailable.
fn adapter_description(adapter: &IDXGIAdapter1) -> String {
    // SAFETY: `GetDesc1` fills a caller-allocated descriptor returned by
    // value through the `windows` wrapper.
    let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
        return String::new();
    };
    utf16_prefix(&desc.Description)
}

/// Describe one output, or `None` when DXGI will not say.
fn describe_output(
    output: &IDXGIOutput,
    adapter_index: u32,
    output_index: u32,
    adapter_name: &str,
) -> Option<DisplayInfo> {
    // SAFETY: `GetDesc` fills a caller-allocated descriptor; the wrapper
    // returns it by value and reports failure through `Result`.
    let desc: DXGI_OUTPUT_DESC = unsafe { output.GetDesc() }.ok()?;

    let rect = desc.DesktopCoordinates;
    let width = u32::try_from(rect.right - rect.left).ok()?;
    let height = u32::try_from(rect.bottom - rect.top).ok()?;
    if width == 0 || height == 0 {
        return None;
    }

    Some(DisplayInfo {
        adapter_index,
        output_index,
        hmonitor: desc.Monitor.0 as usize as u64,
        device_name: utf16_prefix(&desc.DeviceName),
        adapter_name: adapter_name.trim().to_owned(),
        size: OutputSize { width, height },
        is_primary: rect.left == 0 && rect.top == 0,
    })
}

/// Decode a fixed-size, NUL-padded UTF-16 field.
fn utf16_prefix(field: &[u16]) -> String {
    let len = field.iter().position(|&c| c == 0).unwrap_or(field.len());
    String::from_utf16_lossy(&field[..len]).trim().to_owned()
}

/// Look up one display by the source that names it.
///
/// Matches on `HMONITOR` when the source carries one, because that is stable
/// across re-enumeration; the output index alone is ambiguous on a multi-
/// adapter machine, where each adapter numbers its outputs from zero.
pub(crate) fn find(source: CaptureSource) -> Result<DisplayInfo, String> {
    let CaptureSource::Monitor { output_index, hmonitor } = source else {
        return Err("not a monitor source".to_owned());
    };

    let displays = enumerate()?;
    if hmonitor != 0 {
        if let Some(found) = displays.iter().find(|d| d.hmonitor == hmonitor) {
            return Ok(found.clone());
        }
    }
    displays
        .iter()
        .find(|d| d.output_index == output_index)
        .cloned()
        .ok_or_else(|| format!("no display with output index {output_index}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "acceptable in test code")]

    use super::*;

    #[test]
    fn finds_at_least_one_display_on_this_machine() {
        let displays = enumerate().expect("a machine running tests has a display");
        assert!(!displays.is_empty());
        for d in &displays {
            assert!(d.size.width >= 640, "implausible width: {d:?}");
            assert!(d.size.height >= 480, "implausible height: {d:?}");
            assert_ne!(d.hmonitor, 0, "HMONITOR is what gfxcapture needs: {d:?}");
        }
        // `find` must round-trip: a source that names a display we just
        // enumerated has to resolve to the same one, or ddagrab would capture
        // the wrong screen.
        let first = displays.first().expect("at least one display");
        let found = find(first.source()).expect("the display we just enumerated");
        assert_eq!(&found, first);
    }
}
