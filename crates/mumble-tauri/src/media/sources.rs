//! Source-picker descriptors. Handles cross JavaScript as decimal strings.

#![allow(
    unsafe_code,
    reason = "Win32 window enumeration with a synchronous callback"
)]

use serde::Serialize;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextW, IsIconic, IsWindowVisible,
};

use super::{display, settings::OutputSize};

/// Displayed source metadata plus the exact pipeline source descriptor.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceInfo {
    id: String,
    name: String,
    size: OutputSize,
    source: serde_json::Value,
}

/// Enumerate displays and capturable visible top-level windows.
pub(crate) fn enumerate() -> Result<Vec<SourceInfo>, String> {
    let _dpi = display::DpiAware::enter();
    let mut sources: Vec<_> = display::enumerate()?
        .into_iter()
        .map(|d| SourceInfo {
            id: format!("monitor:{}", d.hmonitor),
            name: d.device_name,
            size: d.size,
            source: serde_json::json!({
                "kind": "monitor", "outputIndex": d.output_index,
                "hmonitor": d.hmonitor.to_string(),
            }),
        })
        .collect();
    // SAFETY: the callback borrows this vector only during synchronous enumeration.
    if unsafe { EnumWindows(Some(collect_window), (&raw mut sources) as LPARAM) } == 0 {
        return Err("could not enumerate capture windows".to_owned());
    }
    Ok(sources)
}

unsafe extern "system" fn collect_window(hwnd: HWND, context: LPARAM) -> BOOL {
    // SAFETY: Windows validates handles; output buffers are writable and sized.
    if unsafe { IsWindowVisible(hwnd) } == 0 || unsafe { IsIconic(hwnd) } != 0 {
        return 1;
    }
    let mut cloaked = 0_u32;
    // SAFETY: attributes write at most the supplied buffer size; errors leave the default.
    let _ =
        unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&raw mut cloaked).cast(), 4) };
    if cloaked != 0 {
        return 1;
    }
    let mut title = [0_u16; 1024];
    let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), 1024) };
    let Ok(length) = usize::try_from(length) else {
        return 1;
    };
    if length == 0 {
        return 1;
    }
    let mut rect = RECT::default();
    // DWM bounds exclude invisible resize borders, matching the captured window.
    let bounds = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut rect).cast(),
            16,
        )
    };
    if bounds < 0 && unsafe { GetWindowRect(hwnd, &raw mut rect) } == 0 {
        return 1;
    }
    let (Ok(width), Ok(height)) = (
        u32::try_from(rect.right.saturating_sub(rect.left)),
        u32::try_from(rect.bottom.saturating_sub(rect.top)),
    ) else {
        return 1;
    };
    if width == 0 || height == 0 {
        return 1;
    }
    // SAFETY: context points to the vector owned by enumerate, and is not retained.
    let sources = unsafe { &mut *(context as *mut Vec<SourceInfo>) };
    let handle = hwnd as usize;
    sources.push(SourceInfo {
        id: format!("window:{handle}"),
        name: String::from_utf16_lossy(&title[..length]),
        size: OutputSize { width, height },
        source: serde_json::json!({ "kind": "window", "hwnd": handle.to_string() }),
    });
    1
}
