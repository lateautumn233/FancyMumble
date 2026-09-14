//! Encoder configuration shared by the probe and the live pipeline.
//!
//! One copy of the parameter table from `TODO.md` 0.6, so an encoder that
//! passes probing is opened with the same settings when it matters.  Drift
//! between the two would be the worst kind of bug here: probing would bless a
//! configuration the pipeline never uses.
//!
//! Everything is low latency.  No B-frames, no lookahead, and a GOP long
//! enough that the encoder never emits an IDR by itself - key frames cost
//! bandwidth, and receivers ask for one (PLI/FIR) when they actually need it.

#![allow(
    unsafe_code,
    reason = "FFmpeg encoder configuration; confined to media::ffmpeg"
)]

use std::ffi::CString;

use ffmpeg_sys_next as ff;

use super::errstr;

/// GOP length for encoders that accept an effectively infinite one.
const GOP_INFINITE: i32 = i32::MAX;

/// GOP length for the D3D12 encoders, which cannot take [`GOP_INFINITE`].
///
/// `h264_d3d12va` derives H.264's `log2_max_frame_num_minus4` from the GOP
/// length, and the field only reaches 12, i.e. a frame number wrapping at
/// 2^16.  `i32::MAX` computes to 27 and it refuses the very first frame with
/// `log2_max_frame_num_minus4 out of range`.  16384 frames is 9 minutes at
/// 30 fps, which is "never" for our purposes and well inside the field.
///
/// The HEVC and AV1 D3D12 encoders derive equivalent bitstream fields the
/// same way, so they get the same ceiling.
const GOP_D3D12VA: i32 = 16_384;

/// Time base denominator: presentation timestamps are wall-clock
/// milliseconds throughout the pipeline.
pub(crate) const TIME_BASE_HZ: i32 = 1000;

/// The size, rate and bit rate an encoder context is opened with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncoderParams {
    /// Frame width in pixels.  Must equal the input frames' width: an
    /// encoder context smaller than its input *crops*, it does not scale
    /// (`TODO.md` 0.7).
    pub(crate) width: i32,
    /// Frame height in pixels; see [`Self::width`].
    pub(crate) height: i32,
    /// Frame rate, for the encoder's rate control.
    pub(crate) fps: u32,
    /// Target bit rate in bits per second.
    pub(crate) bitrate_bps: i64,
}

/// The GOP length this encoder tolerates.
fn gop_for(id: &str) -> i32 {
    if id.ends_with("_d3d12va") {
        return GOP_D3D12VA;
    }
    GOP_INFINITE
}

/// Apply the shared parameters to a freshly allocated context.
///
/// Called before `avcodec_open2`, and before the caller attaches a hardware
/// frames context or picks a software pixel format.
pub(crate) fn configure(ctx: *mut ff::AVCodecContext, id: &str, params: &EncoderParams) {
    // SAFETY: `ctx` is a live context the caller exclusively owns, and every
    // field written here is a plain scalar declared by `AVCodecContext`.
    unsafe {
        let c = &mut *ctx;
        c.width = params.width;
        c.height = params.height;
        c.time_base = ff::AVRational { num: 1, den: TIME_BASE_HZ };
        c.framerate = ff::AVRational {
            num: i32::try_from(params.fps).unwrap_or(30),
            den: 1,
        };
        c.bit_rate = params.bitrate_bps;
        let gop = gop_for(id);
        c.gop_size = gop;
        c.keyint_min = gop;
        c.max_b_frames = 0;
        c.has_b_frames = 0;
        c.slices = 1;
        c.thread_type = ff::FF_THREAD_SLICE;
        c.flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
        // Repeat the parameter sets with every key frame, so a receiver that
        // joins mid-stream can decode from the next one without being sent
        // out-of-band extradata.
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
/// what the SFU negotiates (`TODO.md` 0.1).  HEVC Main and AV1 Main are the
/// 8-bit 4:2:0 baselines any decoder for those codecs handles.
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
/// Keyed on the vendor suffix, so the HEVC and AV1 variants of a vendor's
/// encoder get the same treatment as the H.264 one.
fn private_options(id: &str) -> &'static [(&'static str, &'static str)] {
    if id.ends_with("_nvenc") {
        // `delay=0` returns each packet immediately instead of buffering;
        // `rc=cbr` gives the constant rate a live stream wants.
        //
        // `forced_idr=1` matters more than it looks.  Its default is 0, which
        // turns a requested key frame into `FORCEINTRA` - an intra-coded
        // picture that is *not* an IDR and does not reset the reference
        // chain.  A receiver that just joined, or that lost a packet and sent
        // a PLI, cannot start decoding from one of those.
        //
        // Note the hyphen: NVENC spells this option `forced-idr`, while AMF
        // and QSV spell theirs `forced_idr`.
        return &[("delay", "0"), ("rc", "cbr"), ("forced-idr", "1")];
    }
    if id.ends_with("_amf") {
        // AMF blocks up to `query_timeout` ms waiting for output rather
        // than spinning.  `forced_idr` is the same story as for NVENC, and
        // here it additionally makes AMF insert SPS/PPS with the IDR.
        return &[("query_timeout", "1000"), ("rc", "cbr"), ("forced_idr", "1")];
    }
    if id.ends_with("_qsv") {
        // One frame in flight, so latency does not grow with queue depth.
        // QSV takes the IDR request from the frame's key flag as well as from
        // this option, but setting it keeps the three vendors consistent.
        return &[("async_depth", "1"), ("forced_idr", "1")];
    }
    if id.ends_with("_mf") {
        // Tell the MFT this is screen content: DisplayRemoting biases
        // towards low latency and sharp text.
        //
        // Deliberately *not* `hw_encoding=1`.  Media Foundation is listed as
        // a software fallback in the catalogue, and forcing a hardware MFT
        // would make it fail outright on machines that only have the software
        // one - the case it exists to cover.
        return &[("scenario", "display_remoting")];
    }
    &[]
}

/// Apply [`private_options`] to the codec's private context.
///
/// An option the build does not recognise is logged and skipped: these are
/// tuning hints, and refusing to encode because a driver dropped one would
/// be worse than encoding slightly differently than intended.
pub(crate) fn apply_private_options(ctx: *mut ff::AVCodecContext, id: &str) {
    for (key, value) in private_options(id) {
        let Ok(key_c) = CString::new(*key) else { continue };
        let Ok(value_c) = CString::new(*value) else { continue };
        // SAFETY: `priv_data` belongs to the codec's private context,
        // allocated by `avcodec_alloc_context3`; both strings are valid and
        // NUL-terminated for the duration of the call.
        let ret = unsafe { ff::av_opt_set((*ctx).priv_data, key_c.as_ptr(), value_c.as_ptr(), 0) };
        if ret < 0 {
            tracing::debug!("{id}: option {key}={value} rejected ({})", errstr(ret));
        }
    }
}
