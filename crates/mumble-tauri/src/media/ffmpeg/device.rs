//! Hardware devices for the capture and encode pipeline.
//!
//! One D3D11VA device is shared by the capture filter, any scaling stage and
//! the encoder, so frames never cross devices on the fast path.  It is
//! created on a **specific adapter**, not the default one: `ddagrab`
//! enumerates its outputs on whatever adapter the device it was handed
//! belongs to, so on a machine with more than one GPU a default device would
//! capture a different screen than the user picked, or fail to find the
//! output at all.
//!
//! A second D3D12VA device exists only for the `*_d3d12va` encoders, which
//! accept nothing but D3D12 frames.

#![allow(
    unsafe_code,
    reason = "FFmpeg hardware device creation; confined to media::ffmpeg"
)]

use std::ffi::CString;
use std::ptr;

use ffmpeg_sys_next as ff;

use super::errstr;

/// Owns an `AVBufferRef` holding a hardware device context.
pub(super) struct Device(*mut ff::AVBufferRef);

impl Drop for Device {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this reference and unref it exactly once.
            unsafe { ff::av_buffer_unref(&mut self.0) };
        }
    }
}

// SAFETY: `AVHWDeviceContext` is reference-counted and has no thread
// affinity; FFmpeg's D3D11VA implementation sets the device's multithread
// protection flag when it creates it, which is what makes concurrent use by
// the capture and encode threads defined.
unsafe impl Send for Device {}
// SAFETY: as above - shared references only hand out the buffer pointer,
// which FFmpeg's own filters and encoders take by reference.
unsafe impl Sync for Device {}

impl Device {
    /// The underlying device reference, for handing to a filter or encoder.
    pub(super) fn as_ptr(&self) -> *mut ff::AVBufferRef {
        self.0
    }
}

/// Create a D3D11VA device on a specific DXGI adapter.
///
/// `adapter_index` is an index into `IDXGIFactory::EnumAdapters`, which is
/// how `FFmpeg` interprets the device string for this device type - the same
/// ordering [`crate::media::display`] enumerates with.
pub(super) fn d3d11(adapter_index: u32) -> Result<Device, String> {
    let name = CString::new(adapter_index.to_string())
        .map_err(|_| "adapter index contains NUL".to_owned())?;
    create(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, Some(&name))
}

/// Create a D3D12VA device on the default adapter.
///
/// Only used to upload frames for the `*_d3d12va` encoders, which is already
/// a CPU round trip, so pinning the adapter buys nothing here.
pub(super) fn d3d12() -> Result<Device, String> {
    create(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA, None)
}

/// Create a hardware device of `device_type`, optionally naming the adapter.
fn create(device_type: ff::AVHWDeviceType, name: Option<&CString>) -> Result<Device, String> {
    let mut ptr_out: *mut ff::AVBufferRef = ptr::null_mut();
    let name_ptr = name.map_or(ptr::null(), |n| n.as_ptr());

    // SAFETY: the out-parameter is a live pointer slot; `name_ptr` is either
    // null (default adapter) or a NUL-terminated string valid for the call,
    // and no options are passed.
    let ret = unsafe {
        ff::av_hwdevice_ctx_create(&mut ptr_out, device_type, name_ptr, ptr::null_mut(), 0)
    };
    if ret < 0 {
        return Err(format!(
            "could not create {} device ({})",
            type_name(device_type),
            errstr(ret)
        ));
    }
    if ptr_out.is_null() {
        return Err(format!("{} device is null", type_name(device_type)));
    }
    Ok(Device(ptr_out))
}

/// `FFmpeg`'s own name for a device type, for error messages.
fn type_name(device_type: ff::AVHWDeviceType) -> String {
    // SAFETY: returns a static string owned by FFmpeg, or null for an
    // unknown type.
    unsafe { super::cstr(ff::av_hwdevice_get_type_name(device_type)) }
}
