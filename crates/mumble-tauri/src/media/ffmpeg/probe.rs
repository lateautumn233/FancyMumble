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

use super::params::{self, EncoderParams};
use super::{errstr, log};
use crate::media::capture::EncoderInput;
use crate::media::encoder::{ProbeResult, CANDIDATES};

/// Probe frame size.  720p is large enough that every hardware encoder
/// takes it seriously, and small enough to encode in milliseconds.
const PROBE_WIDTH: i32 = 1280;
/// Probe frame height; see [`PROBE_WIDTH`].
const PROBE_HEIGHT: i32 = 720;

/// Mid-grey luma and neutral chroma, so the frame is not degenerate.
const GREY: u8 = 0x80;

/// Bit rate used while probing.  Never leaves this process, but rate control
/// has to be given something plausible for the frame size.
const PROBE_BITRATE_BPS: i64 = 4_000_000;

/// The parameters every probe attempt is made with.
///
/// Shared with the live pipeline through [`params::configure`], so an encoder
/// that passes probing is opened the same way when it matters.
const PROBE_PARAMS: EncoderParams = EncoderParams {
    width: PROBE_WIDTH,
    height: PROBE_HEIGHT,
    fps: 30,
    bitrate_bps: PROBE_BITRATE_BPS,
};

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

/// Input paths tried in the same order as OBS: prefer a zero-copy D3D11
/// surface, then portable system-memory formats, then D3D12.
const INPUT_CANDIDATES: &[EncoderInput] = &[
    EncoderInput::HardwareD3d11,
    EncoderInput::SysMemNv12,
    EncoderInput::HardwareD3d12,
    EncoderInput::SysMemYuv420p,
];

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
                Ok(input) => ProbeResult {
                    id: candidate.id.to_owned(),
                    available: true,
                    detail: None,
                    elapsed_ms,
                    input: Some(input),
                },
                Err(reason) => ProbeResult {
                    id: candidate.id.to_owned(),
                    available: false,
                    detail: Some(detail(&reason, &logged)),
                    elapsed_ms,
                    input: None,
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
fn probe_one(id: &str) -> Result<EncoderInput, String> {
    let name = CString::new(id).map_err(|_| "encoder name contains NUL".to_owned())?;

    // SAFETY: `name` is a valid NUL-terminated string; the returned codec
    // is a static descriptor owned by FFmpeg.
    let codec = unsafe { ff::avcodec_find_encoder_by_name(name.as_ptr()) };
    if codec.is_null() {
        return Err("not compiled into this FFmpeg build".to_owned());
    }

    let advertised = supported_formats(codec)?;
    let mut reasons = Vec::new();
    // Try FFmpeg-advertised paths first, then still verify the remaining
    // candidates. A few vendor drivers report an incomplete list even though
    // their fallback system-memory path works.
    for pass in [true, false] {
        for input in INPUT_CANDIDATES {
            if advertised.supports(*input) != pass {
                continue;
            }
            match probe_attempt(codec, id, *input) {
                Ok(()) => return Ok(*input),
                Err(reason) => reasons.push(format!("{input:?}: {reason}")),
            }
        }
    }
    if reasons.is_empty() {
        Err(format!("no supported input path (advertised: {})", advertised.describe()))
    } else {
        Err(reasons.join("; "))
    }
}

/// The advertised pixel formats for an encoder, queried through FFmpeg's
/// capability API rather than inferred from its name.
struct SupportedFormats {
    values: Vec<ff::AVPixelFormat>,
    all: bool,
}

impl SupportedFormats {
    fn supports(&self, input: EncoderInput) -> bool {
        self.all || self.values.iter().copied().any(|format| match input {
            EncoderInput::HardwareD3d11 => format == ff::AVPixelFormat::AV_PIX_FMT_D3D11,
            EncoderInput::HardwareD3d12 => format == ff::AVPixelFormat::AV_PIX_FMT_D3D12,
            EncoderInput::SysMemNv12 => format == ff::AVPixelFormat::AV_PIX_FMT_NV12,
            EncoderInput::SysMemYuv420p => format == ff::AVPixelFormat::AV_PIX_FMT_YUV420P,
        })
    }

    fn describe(&self) -> String {
        if self.all {
            return "all".to_owned();
        }
        self.values
            .iter()
            .map(|format| unsafe { super::cstr(ff::av_get_pix_fmt_name(*format)) })
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn supported_formats(codec: *const ff::AVCodec) -> Result<SupportedFormats, String> {
    let mut configs: *const std::ffi::c_void = ptr::null();
    let mut count = 0;
    // SAFETY: codec is a static FFmpeg descriptor and output pointers are
    // valid local slots. FFmpeg owns the returned array.
    let ret = unsafe {
        ff::avcodec_get_supported_config(
            ptr::null(),
            codec,
            ff::AVCodecConfig::AV_CODEC_CONFIG_PIX_FORMAT,
            0,
            &mut configs,
            &mut count,
        )
    };
    if ret < 0 {
        return Err(format!("could not query pixel formats ({})", errstr(ret)));
    }
    if configs.is_null() {
        return Ok(SupportedFormats { values: Vec::new(), all: true });
    }
    if count < 0 {
        return Err("FFmpeg returned a negative pixel-format count".to_owned());
    }
    // SAFETY: FFmpeg documents a count-sized array of AVPixelFormat values.
    let values = unsafe {
        std::slice::from_raw_parts(configs.cast::<ff::AVPixelFormat>(), count as usize).to_vec()
    };
    Ok(SupportedFormats { values, all: false })
}

fn probe_attempt(codec: *const ff::AVCodec, id: &str, input: EncoderInput) -> Result<(), String> {
    // SAFETY: codec is a static descriptor returned by FFmpeg.
    let ctx = CodecContext(unsafe { ff::avcodec_alloc_context3(codec) });
    if ctx.0.is_null() {
        return Err("could not allocate codec context".to_owned());
    }
    params::configure(ctx.0, id, &PROBE_PARAMS);

    // Hardware-frame encoders need a device and frame pool kept alive until
    // the encoder has accepted the test frame.
    let (_device, _frames, frame) = match input {
        EncoderInput::SysMemNv12 => {
            unsafe { (*ctx.0).pix_fmt = ff::AVPixelFormat::AV_PIX_FMT_NV12 };
            (BufferRef(ptr::null_mut()), BufferRef(ptr::null_mut()), sysmem_frame(ff::AVPixelFormat::AV_PIX_FMT_NV12)?)
        }
        EncoderInput::SysMemYuv420p => {
            unsafe { (*ctx.0).pix_fmt = ff::AVPixelFormat::AV_PIX_FMT_YUV420P };
            (BufferRef(ptr::null_mut()), BufferRef(ptr::null_mut()), sysmem_frame(ff::AVPixelFormat::AV_PIX_FMT_YUV420P)?)
        }
        EncoderInput::HardwareD3d11 => hardware_frame(
            ctx.0,
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            ff::AVPixelFormat::AV_PIX_FMT_BGRA,
        )?,
        EncoderInput::HardwareD3d12 => hardware_frame(
            ctx.0,
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA,
            ff::AVPixelFormat::AV_PIX_FMT_NV12,
        )?,
    };

    open_encoder(ctx.0, codec, id)?;
    encode_one(ctx.0, frame.0)
}

/// Apply the vendor-private options, then open the encoder.
fn open_encoder(
    ctx: *mut ff::AVCodecContext,
    codec: *const ff::AVCodec,
    id: &str,
) -> Result<(), String> {
    params::apply_private_options(ctx, id);

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
