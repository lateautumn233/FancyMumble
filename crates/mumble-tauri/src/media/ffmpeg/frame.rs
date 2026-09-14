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

use super::errstr;

/// An owned `AVFrame`.
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
