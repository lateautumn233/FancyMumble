//! Owned `AVFrame` and `AVPacket` wrappers.
//!
//! Both are reference-counted by `FFmpeg`, which is what makes the pipeline's
//! shape possible: the capture thread hands a frame to the encode thread by
//! taking a new reference to the same underlying surface, and the encode
//! thread can hold on to it to re-send while the desktop is idle.  No pixels
//! are copied for either.

#![allow(
    unsafe_code,
    reason = "FFmpeg frame and packet lifetimes; confined to media::ffmpeg"
)]

use ffmpeg_sys_next as ff;
use std::ptr;

use super::errstr;

/// An owned `AVFrame`.
#[derive(Debug)]
pub(super) struct Frame(*mut ff::AVFrame);

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this frame and free it exactly once; freeing
            // also drops whatever buffer reference it holds.
            unsafe { ff::av_frame_free(&mut self.0) };
        }
    }
}

// SAFETY: `AVFrame` reference counting is atomic and documented as safe to
// use across threads, provided a single frame is not mutated concurrently.
// Each `Frame` here has exactly one owner at a time; sharing a surface
// between threads goes through [`Frame::new_ref`], which creates a second
// independent frame pointing at the same buffer.
unsafe impl Send for Frame {}

impl Frame {
    /// Allocate an empty frame.
    pub(super) fn empty() -> Result<Self, String> {
        // SAFETY: allocates a zeroed frame; ownership passes to the guard.
        let ptr = unsafe { ff::av_frame_alloc() };
        if ptr.is_null() {
            return Err("could not allocate frame".to_owned());
        }
        Ok(Self(ptr))
    }

    /// The raw pointer, for handing to an `FFmpeg` call.
    pub(super) fn as_ptr(&self) -> *mut ff::AVFrame {
        self.0
    }

    /// A second frame referencing the same picture buffer.
    ///
    /// This is how a captured frame reaches the encoder, and how an idle
    /// desktop's last frame is re-sent: the reference is cheap, the surface
    /// is shared, and the new frame's own metadata - notably `pts` and
    /// `pict_type` - can be set without touching the original.
    pub(super) fn new_ref(&self) -> Result<Self, String> {
        let copy = Self::empty()?;
        // SAFETY: both frames are live; `av_frame_ref` fills the empty
        // destination with a new reference to the source's buffers.
        let ret = unsafe { ff::av_frame_ref(copy.0, self.0) };
        if ret < 0 {
            return Err(format!("could not reference frame ({})", errstr(ret)));
        }
        Ok(copy)
    }

    /// Drop this frame's reference, leaving it empty and reusable.
    pub(super) fn unref(&mut self) {
        // SAFETY: `self.0` is live; unreferencing an already-empty frame is
        // explicitly allowed.
        unsafe { ff::av_frame_unref(self.0) };
    }

    /// Set the presentation timestamp, in the encoder's time base.
    pub(super) fn set_pts(&mut self, pts: i64) {
        // SAFETY: plain scalar field on a live frame.
        unsafe { (*self.0).pts = pts };
    }

    /// Force this frame to be coded as a key frame, or leave it to the
    /// encoder's own decision.
    ///
    /// This is how a receiver's PLI or FIR turns into an IDR: the GOP is set
    /// long enough that the encoder would otherwise never produce one.
    ///
    /// Both the picture type and the key flag are set.  The picture type is
    /// what NVENC, AMF and Media Foundation look at; QSV additionally upgrades
    /// an intra picture to a true IDR when the frame is flagged as a key
    /// frame.  The encoders are also configured with `forced_idr=1`
    /// ([`super::params`]), without which a "key frame" request produces a
    /// merely intra-coded picture that a joining receiver cannot decode from.
    pub(super) fn set_force_key(&mut self, force: bool) {
        // SAFETY: plain scalar fields on a live frame.
        unsafe {
            let frame = &mut *self.0;
            if force {
                frame.pict_type = ff::AVPictureType::AV_PICTURE_TYPE_I;
                frame.flags |= ff::AV_FRAME_FLAG_KEY;
            } else {
                frame.pict_type = ff::AVPictureType::AV_PICTURE_TYPE_NONE;
                frame.flags &= !ff::AV_FRAME_FLAG_KEY;
            }
        }
    }

    /// Convert the frame to a small RGB JPEG for the local native preview.
    ///
    /// Capture normally produces a D3D11 hardware frame.  The transfer is
    /// performed only when the UI asks for a preview image, so the broadcast
    /// path remains zero-copy while sharing is running.
    pub(super) fn to_jpeg(&self, max_width: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let software = self.to_software_frame()?;
        // SAFETY: the owned software frame remains live until scaling finishes.
        let (src_width, src_height, src_format) = unsafe {
            let frame = &*software.0;
            if frame.width <= 0 || frame.height <= 0 {
                return Err("preview frame has invalid dimensions".to_owned());
            }
            (frame.width, frame.height, frame.format)
        };
        let target_width = max_width.clamp(1, 1280).min(src_width as u32);
        let target_height = ((src_height as u64 * target_width as u64) / src_width as u64)
            .max(1) as u32;
        // Bound portrait and extreme-aspect-ratio sources as well as wide ones.
        let (target_width, target_height) = if target_height > 1280 {
            (((u64::from(target_width) * 1280) / u64::from(target_height)).max(1) as u32, 1280)
        } else {
            (target_width, target_height)
        };
        let target = Frame::empty()?;
        // SAFETY: the empty destination is exclusively owned and allocated below.
        unsafe {
            (*target.0).format = ff::AVPixelFormat::AV_PIX_FMT_RGB24 as i32;
            (*target.0).width = target_width as i32;
            (*target.0).height = target_height as i32;
            let ret = ff::av_frame_get_buffer(target.0, 1);
            if ret < 0 {
                return Err(format!("could not allocate preview image ({})", errstr(ret)));
            }
        }
        // SAFETY: format is an AVPixelFormat provided by FFmpeg; dimensions are positive.
        let scaler = unsafe {
            ff::sws_getContext(
                src_width,
                src_height,
                std::mem::transmute::<i32, ff::AVPixelFormat>(src_format),
                target_width as i32,
                target_height as i32,
                ff::AVPixelFormat::AV_PIX_FMT_RGB24,
                ff::SwsFlags::SWS_BILINEAR as i32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if scaler.is_null() {
            return Err("could not create preview scaler".to_owned());
        }
        // SAFETY: both frames have allocated planes matching the scaler configuration.
        let scaled = unsafe {
            ff::sws_scale(
                scaler,
                (*software.0).data.as_ptr() as *const *const u8,
                (*software.0).linesize.as_ptr(),
                0,
                src_height,
                (*target.0).data.as_mut_ptr(),
                (*target.0).linesize.as_ptr(),
            )
        };
        // SAFETY: the scaler is uniquely owned and no longer used.
        unsafe { ff::sws_freeContext(scaler) };
        if scaled <= 0 {
            return Err("could not scale preview frame".to_owned());
        }

        // SAFETY: target is an allocated RGB24 frame; FFmpeg supplies its row stride.
        let stride = unsafe { (*target.0).linesize[0].max(0) as usize };
        let row_len = target_width as usize * 3;
        let mut rgb = Vec::with_capacity(row_len * target_height as usize);
        // SAFETY: copy only the visible RGB bytes of each allocated row, excluding padding.
        unsafe {
            let data = (*target.0).data[0];
            for row in 0..target_height as usize {
                let row_ptr = data.add(row * stride);
                rgb.extend_from_slice(std::slice::from_raw_parts(row_ptr, row_len));
            }
        }
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75)
            .encode(&rgb, target_width, target_height, image::ExtendedColorType::Rgb8)
            .map_err(|e| format!("could not encode preview image: {e}"))?;
        Ok((target_width, target_height, bytes))
    }

    fn to_software_frame(&self) -> Result<Self, String> {
        // SAFETY: this is a live, referenced frame, immutable for this call.
        let hardware = unsafe {
            !(*self.0).hw_frames_ctx.is_null()
                || (*self.0).format == ff::AVPixelFormat::AV_PIX_FMT_D3D11 as i32
                || (*self.0).format == ff::AVPixelFormat::AV_PIX_FMT_D3D12 as i32
        };
        if !hardware {
            return self.new_ref();
        }
        let target = Self::empty()?;
        // SAFETY: FFmpeg allocates system memory for an empty destination and
        // synchronizes the transfer through the frame's hardware device context.
        let ret = unsafe { ff::av_hwframe_transfer_data(target.0, self.0, 0) };
        if ret < 0 {
            return Err(format!("could not transfer preview frame ({})", errstr(ret)));
        }
        Ok(target)
    }
}

/// An owned `AVPacket`.
pub(super) struct Packet(*mut ff::AVPacket);

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this packet and free it exactly once.
            unsafe { ff::av_packet_free(&mut self.0) };
        }
    }
}

// SAFETY: as for `Frame` - reference counting is atomic, and each packet has
// one owner.
unsafe impl Send for Packet {}

impl Packet {
    /// Allocate an empty packet.
    pub(super) fn empty() -> Result<Self, String> {
        // SAFETY: allocates a packet with default fields; ownership passes to
        // the guard.
        let ptr = unsafe { ff::av_packet_alloc() };
        if ptr.is_null() {
            return Err("could not allocate packet".to_owned());
        }
        Ok(Self(ptr))
    }

    /// The raw pointer, for handing to an `FFmpeg` call.
    pub(super) fn as_ptr(&self) -> *mut ff::AVPacket {
        self.0
    }

    /// Copy the payload out, with its key-frame flag and timestamp.
    ///
    /// A copy, not a borrow: the encoder reuses the packet's buffer for the
    /// next call, and the transport stage keeps the bitstream for as long as
    /// it needs to retransmit it.
    pub(super) fn to_encoded(&self) -> Option<(Vec<u8>, i64, bool)> {
        // SAFETY: `self.0` is live; after a successful `receive_packet` its
        // `data` points to `size` readable bytes.
        unsafe {
            let packet = &*self.0;
            if packet.size <= 0 || packet.data.is_null() {
                return None;
            }
            let len = usize::try_from(packet.size).ok()?;
            let bytes = std::slice::from_raw_parts(packet.data.cast::<u8>(), len).to_vec();
            let key = (packet.flags & ff::AV_PKT_FLAG_KEY) != 0;
            Some((bytes, packet.pts, key))
        }
    }

    /// Release the payload, leaving the packet reusable.
    pub(super) fn unref(&mut self) {
        // SAFETY: `self.0` is live; unreferencing twice is allowed.
        unsafe { ff::av_packet_unref(self.0) };
    }
}
