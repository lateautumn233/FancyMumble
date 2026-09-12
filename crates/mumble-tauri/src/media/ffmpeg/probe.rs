//! Open each candidate encoder and encode one frame.
//!
//! "Built into `FFmpeg`" and "works on this machine" are different things:
//! `h264_amf` is always compiled in but fails instantly without an AMD
//! driver, and `h264_qsv` needs an Intel GPU present.  The only reliable
//! test is to open the encoder and get a key frame out of it, which is what
//! this module does.  It runs in the probe subprocess (see
//! [`super::super::encoder`]), so a driver that hangs or crashes cannot
//! take the app down with it.
//!
//! Encoder parameters follow the table in `TODO.md` 0.6.

#![allow(
    unsafe_code,
    reason = "FFmpeg encoder setup and frame handling; confined to this module"
)]

use std::ffi::CString;
use std::ptr;

use ffmpeg_sys_next as ff;

use super::{errstr, log};
use crate::media::encoder::{ProbeResult, CANDIDATES};

/// Probe frame size.  720p is large enough that every hardware encoder
/// takes it seriously, and small enough to encode in milliseconds.
const PROBE_WIDTH: i32 = 1280;
/// Probe frame height; see [`PROBE_WIDTH`].
const PROBE_HEIGHT: i32 = 720;

/// Mid-grey luma and neutral chroma, so the frame is not degenerate.
const GREY: u8 = 0x80;

/// GOP length for encoders that accept an effectively infinite one.
///
/// We never want an encoder inserting IDR frames on its own schedule: key
/// frames cost bandwidth and the receivers ask for them when they actually
/// need one (PLI/FIR).  `i32::MAX` is what hwcodec uses and NVENC accepts it.
const GOP_INFINITE: i32 = i32::MAX;

/// GOP length for the D3D12 encoders, which cannot take [`GOP_INFINITE`].
///
/// `h264_d3d12va` derives H.264's `log2_max_frame_num_minus4` from the GOP
/// length, and the field only reaches 12, i.e. a frame number wrapping at
/// 2^16.  `i32::MAX` computes to 27 and it refuses the very first frame with
/// `log2_max_frame_num_minus4 out of range`.  16384 frames is 9 minutes at
/// 30 fps, which is "never" for our purposes and well inside the field.
///
/// Applied to the HEVC and AV1 D3D12 encoders too: they derive equivalent
/// bitstream fields the same way, so the same ceiling problem applies even
/// though the field names differ.
const GOP_D3D12VA: i32 = 16_384;

/// Owns an `AVCodecContext` and frees it on drop.
struct CodecContext(*mut ff::AVCodecContext);

impl Drop for CodecContext {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this pointer and it is freed exactly once.
            unsafe { ff::avcodec_free_context(&mut self.0) };
        }
    }
}

/// Owns an `AVFrame` and frees it on drop.
struct Frame(*mut ff::AVFrame);

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this pointer and it is freed exactly once.
            unsafe { ff::av_frame_free(&mut self.0) };
        }
    }
}

/// Owns an `AVPacket` and frees it on drop.
struct Packet(*mut ff::AVPacket);

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this pointer and it is freed exactly once.
            unsafe { ff::av_packet_free(&mut self.0) };
        }
    }
}

/// Owns an `AVBufferRef` (hardware device or frames context).
struct BufferRef(*mut ff::AVBufferRef);

impl Drop for BufferRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this reference and unref it exactly once.
            unsafe { ff::av_buffer_unref(&mut self.0) };
        }
    }
}

/// What kind of frame an encoder accepts as input.
#[derive(Debug, Clone, Copy)]
enum InputMode {
    /// A plain system-memory frame in this pixel format.
    SysMem(ff::AVPixelFormat),
    /// A hardware frame from this device type, backed by this software
    /// format.  Used for `h264_d3d12va`, which rejects system memory.
    Hardware {
        /// Hardware device to create.
        device: ff::AVHWDeviceType,
        /// Software pixel format the surface pool is built from.
        sw_format: ff::AVPixelFormat,
    },
}

/// The input mode each candidate needs.
///
/// Measured on this machine (see `TODO.md` 0.4): `libopenh264` rejects NV12
/// and only takes `yuv420p`, and `h264_d3d12va` rejects system memory
/// entirely and needs a real D3D12 frame pool.
fn input_mode(id: &str) -> InputMode {
    if id == "libopenh264" {
        return InputMode::SysMem(ff::AVPixelFormat::AV_PIX_FMT_YUV420P);
    }
    if id.ends_with("_d3d12va") {
        return InputMode::Hardware {
            device: ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA,
            sw_format: ff::AVPixelFormat::AV_PIX_FMT_NV12,
        };
    }
    InputMode::SysMem(ff::AVPixelFormat::AV_PIX_FMT_NV12)
}

/// Probe every candidate in catalogue order.
///
/// Runs in the probe subprocess and never panics: each candidate's outcome,
/// success or failure, becomes one [`ProbeResult`].
pub(crate) fn probe_all() -> Vec<ProbeResult> {
    log::install();

    CANDIDATES
        .iter()
        .map(|candidate| {
            let started = std::time::Instant::now();
            log::begin_capture();
            let outcome = probe_one(candidate.id);
            let logged = log::take_errors();
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

            match outcome {
                Ok(()) => ProbeResult {
                    id: candidate.id.to_owned(),
                    available: true,
                    detail: None,
                    elapsed_ms,
                },
                Err(reason) => ProbeResult {
                    id: candidate.id.to_owned(),
                    available: false,
                    detail: Some(detail(&reason, &logged)),
                    elapsed_ms,
                },
            }
        })
        .collect()
}

/// Combine our own failure reason with what `FFmpeg` logged.
///
/// `FFmpeg`'s own message is the useful one for a user ("DLL amfrt64.dll
/// failed to open"), so it leads; our reason names the step that failed.
fn detail(reason: &str, logged: &[String]) -> String {
    if logged.is_empty() {
        return reason.to_owned();
    }
    format!("{}: {}", reason, logged.join("; "))
}

/// Open one encoder and encode a single frame through it.
///
/// Returns `Ok(())` only when a packet with `AV_PKT_FLAG_KEY` comes back:
/// an encoder that opens but produces nothing usable is not available as
/// far as the pipeline is concerned.
fn probe_one(id: &str) -> Result<(), String> {
    let name = CString::new(id).map_err(|_| "encoder name contains NUL".to_owned())?;

    // SAFETY: `name` is a valid NUL-terminated string; the returned codec
    // is a static descriptor owned by FFmpeg.
    let codec = unsafe { ff::avcodec_find_encoder_by_name(name.as_ptr()) };
    if codec.is_null() {
        return Err("not compiled into this FFmpeg build".to_owned());
    }

    // SAFETY: `codec` is non-null, as checked above.
    let ctx = CodecContext(unsafe { ff::avcodec_alloc_context3(codec) });
    if ctx.0.is_null() {
        return Err("could not allocate codec context".to_owned());
    }

    configure(ctx.0, id);

    // Hardware-frame encoders need a device and frame pool that stay alive
    // for as long as the codec context uses them, hence the bindings here.
    let (_device, _frames, frame) = match input_mode(id) {
        InputMode::SysMem(pix_fmt) => {
            // SAFETY: `ctx.0` is a live, freshly allocated context.
            unsafe { (*ctx.0).pix_fmt = pix_fmt };
            (BufferRef(ptr::null_mut()), BufferRef(ptr::null_mut()), sysmem_frame(pix_fmt)?)
        }
        InputMode::Hardware { device, sw_format } => {
            let (device, frames, frame) = hardware_frame(ctx.0, device, sw_format)?;
            (device, frames, frame)
        }
    };

    open_encoder(ctx.0, codec, id)?;
    encode_one(ctx.0, frame.0)
}

/// Apply the shared encoder parameters from `TODO.md` 0.6.
///
/// Low latency throughout: no B-frames, no lookahead, and a GOP so long
/// that the encoder never inserts an IDR on its own - key frames are the
/// caller's business, driven by receiver PLI/FIR.
fn configure(ctx: *mut ff::AVCodecContext, id: &str) {
    // SAFETY: `ctx` is a live context that we exclusively own, and every
    // field written here is a plain scalar declared by `AVCodecContext`.
    unsafe {
        let c = &mut *ctx;
        c.width = PROBE_WIDTH;
        c.height = PROBE_HEIGHT;
        // Millisecond time base: presentation timestamps are wall-clock ms.
        c.time_base = ff::AVRational { num: 1, den: 1000 };
        c.framerate = ff::AVRational { num: 30, den: 1 };
        c.bit_rate = 4_000_000;
        let gop = if id.ends_with("_d3d12va") { GOP_D3D12VA } else { GOP_INFINITE };
        c.gop_size = gop;
        c.keyint_min = gop;
        c.max_b_frames = 0;
        c.has_b_frames = 0;
        c.slices = 1;
        c.thread_type = ff::FF_THREAD_SLICE;
        c.flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
        c.flags2 |= ff::AV_CODEC_FLAG2_LOCAL_HEADER;
        c.color_range = ff::AVColorRange::AVCOL_RANGE_MPEG;
        c.colorspace = ff::AVColorSpace::AVCOL_SPC_SMPTE170M;
        c.color_primaries = ff::AVColorPrimaries::AVCOL_PRI_SMPTE170M;
        c.color_trc = ff::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE170M;

        if let Some(profile) = profile_for(id) {
            c.profile = profile;
        }

        // QSV emulates CBR through VBR with a matched ceiling, and needs
        // relaxed compliance to accept it.
        if id.ends_with("_qsv") {
            c.rc_max_rate = c.bit_rate;
            c.strict_std_compliance = ff::FF_COMPLIANCE_UNOFFICIAL;
        }
    }
}

/// The profile to request, or `None` to let the encoder decide.
///
/// H.264 High matches str0m's `profile-level-id=64001f` variant, which is
/// what the SFU negotiates (see `TODO.md` 0.1).  HEVC Main and AV1 Main are
/// the 8-bit 4:2:0 baselines every decoder that supports the codec at all
/// can handle.
///
/// The D3D12 encoders derive the profile from the frame format themselves and
/// reject the hint, so they get `None`.
fn profile_for(id: &str) -> Option<i32> {
    if id.ends_with("_d3d12va") {
        return None;
    }
    if id.starts_with("h264") || id == "libopenh264" {
        return Some(ff::AV_PROFILE_H264_HIGH);
    }
    if id.starts_with("hevc") {
        return Some(ff::AV_PROFILE_HEVC_MAIN);
    }
    if id.starts_with("av1") {
        return Some(ff::AV_PROFILE_AV1_MAIN);
    }
    None
}

/// Vendor-private low-latency options, per `TODO.md` 0.6.
///
/// Set on `priv_data` before opening.  An option a given build does not
/// recognise is logged and skipped rather than treated as fatal: these are
/// tuning hints, and the probe's question is whether the encoder works at
/// all.
fn private_options(id: &str) -> &'static [(&'static str, &'static str)] {
    // Keyed on the vendor suffix, so the HEVC and AV1 variants of each
    // vendor's encoder get the same treatment as the H.264 one.
    if id.ends_with("_nvenc") {
        // `delay=0` makes NVENC return each packet immediately instead of
        // buffering; `rc=cbr` gives the constant rate a live stream wants.
        return &[("delay", "0"), ("rc", "cbr")];
    }
    if id.ends_with("_amf") {
        // AMF blocks up to `query_timeout` ms waiting for output rather
        // than spinning.
        return &[("query_timeout", "1000"), ("rc", "cbr")];
    }
    if id.ends_with("_qsv") {
        // One frame in flight, so latency does not grow with queue depth.
        return &[("async_depth", "1")];
    }
    &[]
}

/// Apply [`private_options`], then open the encoder.
fn open_encoder(
    ctx: *mut ff::AVCodecContext,
    codec: *const ff::AVCodec,
    id: &str,
) -> Result<(), String> {
    for (key, value) in private_options(id) {
        let Ok(key_c) = CString::new(*key) else { continue };
        let Ok(value_c) = CString::new(*value) else { continue };
        // SAFETY: `priv_data` belongs to the codec's private context,
        // allocated by `avcodec_alloc_context3`; both strings are valid and
        // NUL-terminated for the duration of the call.
        let ret = unsafe {
            ff::av_opt_set((*ctx).priv_data, key_c.as_ptr(), value_c.as_ptr(), 0)
        };
        if ret < 0 {
            tracing::debug!("{id}: option {key}={value} rejected ({})", errstr(ret));
        }
    }

    // SAFETY: `ctx` is configured and owned by us; `codec` is the static
    // descriptor the context was allocated from.
    let ret = unsafe { ff::avcodec_open2(ctx, codec, ptr::null_mut()) };
    if ret < 0 {
        return Err(format!("could not open encoder ({})", errstr(ret)));
    }
    Ok(())
}

/// Number of planes, and each plane's height relative to the frame.
///
/// NV12 has interleaved chroma in one half-height plane; yuv420p has two
/// quarter-size planes.  Both are filled with the same byte, which is
/// mid-grey luma and neutral chroma.
fn plane_heights(pix_fmt: ff::AVPixelFormat) -> &'static [i32] {
    match pix_fmt {
        ff::AVPixelFormat::AV_PIX_FMT_YUV420P => &[1, 2, 2],
        // NV12 and anything else we probe: luma plus one chroma plane.
        _ => &[1, 2],
    }
}

/// Allocate a system-memory frame filled with mid-grey.
fn sysmem_frame(pix_fmt: ff::AVPixelFormat) -> Result<Frame, String> {
    // SAFETY: allocates a frame with all fields zeroed; ownership passes to
    // the `Frame` guard, which frees it.
    let frame = Frame(unsafe { ff::av_frame_alloc() });
    if frame.0.is_null() {
        return Err("could not allocate frame".to_owned());
    }

    // SAFETY: `frame.0` is a live frame we exclusively own.
    let ret = unsafe {
        (*frame.0).format = pix_fmt as i32;
        (*frame.0).width = PROBE_WIDTH;
        (*frame.0).height = PROBE_HEIGHT;
        ff::av_frame_get_buffer(frame.0, 0)
    };
    if ret < 0 {
        return Err(format!("could not allocate frame buffer ({})", errstr(ret)));
    }

    for (plane, divisor) in plane_heights(pix_fmt).iter().enumerate() {
        // SAFETY: `plane` is within the plane count for this pixel format,
        // so `data[plane]` is a buffer of at least `linesize * height`
        // bytes as allocated by `av_frame_get_buffer` above.
        unsafe {
            let data = (*frame.0).data[plane];
            if data.is_null() {
                continue;
            }
            let linesize = (*frame.0).linesize[plane].max(0) as usize;
            let rows = (PROBE_HEIGHT / divisor).max(0) as usize;
            ptr::write_bytes(data, GREY, linesize * rows);
        }
    }

    // SAFETY: presentation timestamp is a plain field.
    unsafe { (*frame.0).pts = 0 };
    Ok(frame)
}

/// The hardware pixel format a device type produces.
fn hw_pix_fmt(device: ff::AVHWDeviceType) -> ff::AVPixelFormat {
    match device {
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA => ff::AVPixelFormat::AV_PIX_FMT_D3D12,
        _ => ff::AVPixelFormat::AV_PIX_FMT_D3D11,
    }
}

/// Build a hardware device, a frame pool, and one uploaded grey frame.
///
/// Used for `h264_d3d12va`, which rejects system-memory input outright, so
/// probing it means standing up a real D3D12 device and surface pool.
fn hardware_frame(
    ctx: *mut ff::AVCodecContext,
    device_type: ff::AVHWDeviceType,
    sw_format: ff::AVPixelFormat,
) -> Result<(BufferRef, BufferRef, Frame), String> {
    let mut device_ptr: *mut ff::AVBufferRef = ptr::null_mut();
    // SAFETY: out-parameter is a live pointer slot; passing no device name
    // and no options selects the default adapter.
    let ret = unsafe {
        ff::av_hwdevice_ctx_create(&mut device_ptr, device_type, ptr::null(), ptr::null_mut(), 0)
    };
    let device = BufferRef(device_ptr);
    if ret < 0 {
        return Err(format!("could not create hardware device ({})", errstr(ret)));
    }

    // SAFETY: `device.0` is a valid device reference, as just created.
    let frames = BufferRef(unsafe { ff::av_hwframe_ctx_alloc(device.0) });
    if frames.0.is_null() {
        return Err("could not allocate hardware frame context".to_owned());
    }

    let hw_format = hw_pix_fmt(device_type);
    // SAFETY: `frames.0->data` points to an `AVHWFramesContext`, which is
    // this buffer's documented payload; it is uninitialised until
    // `av_hwframe_ctx_init`, which is exactly when we fill these fields.
    let ret = unsafe {
        let frames_ctx = (*frames.0).data.cast::<ff::AVHWFramesContext>();
        (*frames_ctx).format = hw_format;
        (*frames_ctx).sw_format = sw_format;
        (*frames_ctx).width = PROBE_WIDTH;
        (*frames_ctx).height = PROBE_HEIGHT;
        (*frames_ctx).initial_pool_size = 2;
        ff::av_hwframe_ctx_init(frames.0)
    };
    if ret < 0 {
        return Err(format!("could not init hardware frame context ({})", errstr(ret)));
    }

    // SAFETY: the codec context takes ownership of the reference it is
    // given, so we hand it a new one and keep ours in the guard.
    unsafe {
        (*ctx).pix_fmt = hw_format;
        (*ctx).hw_frames_ctx = ff::av_buffer_ref(frames.0);
        if (*ctx).hw_frames_ctx.is_null() {
            return Err("could not reference hardware frame context".to_owned());
        }
    }

    // Upload before moving `frames` into the tuple: the frame pool has to
    // still be owned here for `upload_grey` to draw a surface from it.
    let frame = upload_grey(frames.0, sw_format)?;
    Ok((device, frames, frame))
}

/// Allocate a hardware frame and upload a grey system-memory frame into it.
fn upload_grey(
    frames: *mut ff::AVBufferRef,
    sw_format: ff::AVPixelFormat,
) -> Result<Frame, String> {
    let source = sysmem_frame(sw_format)?;

    // SAFETY: allocates an empty frame owned by the guard.
    let target = Frame(unsafe { ff::av_frame_alloc() });
    if target.0.is_null() {
        return Err("could not allocate hardware frame".to_owned());
    }

    // SAFETY: `frames` is an initialised frame context; `target` is empty,
    // which is what `av_hwframe_get_buffer` requires.
    let ret = unsafe { ff::av_hwframe_get_buffer(frames, target.0, 0) };
    if ret < 0 {
        return Err(format!("could not get hardware frame ({})", errstr(ret)));
    }

    // SAFETY: both frames are live, have matching dimensions, and `target`
    // has a hardware frames context attached, as required.
    let ret = unsafe { ff::av_hwframe_transfer_data(target.0, source.0, 0) };
    if ret < 0 {
        return Err(format!("could not upload frame to hardware ({})", errstr(ret)));
    }

    // SAFETY: plain scalar field on a frame we own.
    unsafe { (*target.0).pts = 0 };
    Ok(target)
}

/// Send one frame, flush, and require a key frame back.
///
/// The flush matters: several encoders hold the first frame internally and
/// would otherwise report "no output" for a perfectly working setup.
fn encode_one(ctx: *mut ff::AVCodecContext, frame: *mut ff::AVFrame) -> Result<(), String> {
    // SAFETY: `ctx` is an opened encoder and `frame` matches its configured
    // format and dimensions.
    let ret = unsafe { ff::avcodec_send_frame(ctx, frame) };
    if ret < 0 {
        return Err(format!("encoder rejected the frame ({})", errstr(ret)));
    }

    // SAFETY: a null frame signals end-of-stream, flushing buffered output.
    let _ = unsafe { ff::avcodec_send_frame(ctx, ptr::null()) };

    // SAFETY: allocates a packet owned by the guard.
    let packet = Packet(unsafe { ff::av_packet_alloc() });
    if packet.0.is_null() {
        return Err("could not allocate packet".to_owned());
    }

    let mut got_key = false;
    loop {
        // SAFETY: `ctx` is open and `packet.0` is a live, unreferenced packet.
        let ret = unsafe { ff::avcodec_receive_packet(ctx, packet.0) };
        if ret == ff::AVERROR(ff::EAGAIN) || ret == ff::AVERROR_EOF {
            break;
        }
        if ret < 0 {
            return Err(format!("encoder produced no packet ({})", errstr(ret)));
        }

        // SAFETY: `receive_packet` returned success, so the packet is filled.
        unsafe {
            if ((*packet.0).flags & ff::AV_PKT_FLAG_KEY) != 0 && (*packet.0).size > 0 {
                got_key = true;
            }
            ff::av_packet_unref(packet.0);
        }
    }

    if got_key {
        return Ok(());
    }
    Err("encoder produced no key frame".to_owned())
}
