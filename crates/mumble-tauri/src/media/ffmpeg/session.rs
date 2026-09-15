//! Open an encoder and feed it frames from the capture graph.
//!
//! The encoder is configured from the same table as the probe
//! ([`super::params`]), so an encoder that passed probing is opened with the
//! same settings when it matters.  Hardware frames come from the capture
//! graph's own frames context, so they never leave the device they were
//! captured on.

#![allow(
    unsafe_code,
    reason = "FFmpeg encoder open and frame I/O; confined to media::ffmpeg"
)]

use std::ffi::CString;
use std::ptr;
use std::time::Instant;

use ffmpeg_sys_next as ff;

use super::frame::{Frame, Packet};
use super::graph::SinkFormat;
use super::params::{self, EncoderParams};
use super::errstr;

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

/// An opened encoder, ready to take frames.
pub(super) struct Encoder {
    /// The encoder context.
    ctx: CodecContext,
    /// Reused across `receive_packet` calls.
    packet: Packet,
    /// `FFmpeg` encoder name, for log lines.
    id: String,
}

// SAFETY: an opened `AVCodecContext` is used from one thread at a time, which
// the pipeline guarantees by moving the encoder onto the encode thread and
// never sharing it.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Open `id` against the capture graph's output.
    ///
    /// `hw_frames_ctx` is the sink's hardware frames context, or null for
    /// system-memory input.  When non-null the encoder takes a new reference
    /// to it, so the graph can be dropped independently of the encoder.
    pub(super) fn open(
        id: &str,
        format: SinkFormat,
        bitrate_bps: i64,
        fps: u32,
        hw_frames_ctx: *mut ff::AVBufferRef,
    ) -> Result<Self, String> {
        let name = CString::new(id).map_err(|_| "encoder name contains NUL".to_owned())?;
        // SAFETY: `name` is NUL-terminated; the returned codec is a static
        // descriptor owned by FFmpeg.
        let codec = unsafe { ff::avcodec_find_encoder_by_name(name.as_ptr()) };
        if codec.is_null() {
            return Err(format!("{id} is not in this FFmpeg build"));
        }

        // SAFETY: `codec` is non-null, as just checked.
        let ctx = CodecContext(unsafe { ff::avcodec_alloc_context3(codec) });
        if ctx.0.is_null() {
            return Err("could not allocate codec context".to_owned());
        }

        params::configure(
            ctx.0,
            id,
            &EncoderParams {
                width: format.width,
                height: format.height,
                fps,
                bitrate_bps,
            },
        );

        // SAFETY: `ctx.0` is a live, unopened context we exclusively own.
        unsafe {
            (*ctx.0).pix_fmt = std::mem::transmute::<i32, ff::AVPixelFormat>(format.pix_fmt);
            if !hw_frames_ctx.is_null() {
                (*ctx.0).hw_frames_ctx = ff::av_buffer_ref(hw_frames_ctx);
                if (*ctx.0).hw_frames_ctx.is_null() {
                    return Err("could not reference hardware frame context".to_owned());
                }
            }
        }

        params::apply_private_options(ctx.0, id);

        // The hardware pixel format alone does not identify the underlying texture format.
        // SAFETY: the owned codec context and its referenced hardware frame context are live.
        unsafe {
            let c = &*ctx.0;
            let sw_format = if c.hw_frames_ctx.is_null() {
                c.pix_fmt
            } else {
                (*((*c.hw_frames_ctx).data.cast::<ff::AVHWFramesContext>())).sw_format
            };
            tracing::info!(encoder = id, width = c.width, height = c.height, fps,
                bitrate_bps, pixel_format = %super::cstr(ff::av_get_pix_fmt_name(c.pix_fmt)),
                software_format = %super::cstr(ff::av_get_pix_fmt_name(sw_format)),
                hardware_frames = !c.hw_frames_ctx.is_null(),
                ffmpeg_version = %super::cstr(ff::av_version_info()),
                avcodec_version = ff::avcodec_version(),
                time_base_num = c.time_base.num, time_base_den = c.time_base.den,
                "screen-share encoder opening");
        }

        // SAFETY: `ctx.0` is configured and owned by us; `codec` is the
        // static descriptor the context was allocated from.
        let ret = unsafe { ff::avcodec_open2(ctx.0, codec, ptr::null_mut()) };
        if ret < 0 {
            tracing::error!(encoder = id, operation = "avcodec_open2",
                error_code = ret, error = %errstr(ret), "screen-share encoder open failed");
            return Err(format!("could not open {id} ({})", errstr(ret)));
        }

        Ok(Self { ctx, packet: Packet::empty()?, id: id.to_owned() })
    }

    /// Send one frame and drain every packet it produces.
    ///
    /// `force_key` sets `pict_type = I` on the frame, which NVENC, AMF, QSV
    /// and `libopenh264` honour as a request for an IDR.  Media Foundation's
    /// behaviour is unverified (`TODO.md` stage 2).
    pub(super) fn encode(
        &mut self,
        frame: &mut Frame,
        force_key: bool,
        packets: &mut Vec<EncodedPacket>,
    ) -> Result<std::time::Duration, String> {
        frame.set_force_key(force_key);

        let started = Instant::now();
        // SAFETY: `ctx` is an opened encoder and `frame` matches its
        // configured format and dimensions.
        let ret = unsafe { ff::avcodec_send_frame(self.ctx.0, frame.as_ptr()) };
        if ret < 0 {
            tracing::error!(encoder = %self.id, operation = "avcodec_send_frame",
                error_code = ret, error = %errstr(ret), force_key,
                "screen-share encoder frame submission failed");
            return Err(format!("{} rejected the frame ({})", self.id, errstr(ret)));
        }
        self.drain(packets)?;
        Ok(started.elapsed())
    }

    /// Drain every packet currently available.
    fn drain(&mut self, packets: &mut Vec<EncodedPacket>) -> Result<(), String> {
        loop {
            // SAFETY: `ctx` is open and `packet` is a live, unreferenced
            // packet, which is what `receive_packet` requires.
            let ret = unsafe { ff::avcodec_receive_packet(self.ctx.0, self.packet.as_ptr()) };
            if ret == ff::AVERROR(ff::EAGAIN) || ret == ff::AVERROR_EOF {
                return Ok(());
            }
            if ret < 0 {
                tracing::error!(encoder = %self.id, operation = "avcodec_receive_packet",
                    error_code = ret, error = %errstr(ret),
                    "screen-share encoder packet receive failed");
                return Err(format!("{} produced no packet ({})", self.id, errstr(ret)));
            }

            if let Some((bytes, pts, key)) = self.packet.to_encoded() {
                packets.push(EncodedPacket { bytes, pts, key });
            }
            self.packet.unref();
        }
    }
}

/// One encoded packet, copied out of `FFmpeg`.
#[derive(Debug, Clone)]
pub(super) struct EncodedPacket {
    /// The bitstream: Annex-B for H.264 / HEVC, OBU for AV1.
    pub(super) bytes: Vec<u8>,
    /// Presentation timestamp, in milliseconds from the start of the stream.
    pub(super) pts: i64,
    /// Whether this packet is a key frame (`AV_PKT_FLAG_KEY`).
    pub(super) key: bool,
}
