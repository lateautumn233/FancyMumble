//! Build a capture filter graph from a [`FilterStep`] plan.
//!
//! Filters are created one at a time - `avfilter_graph_alloc_filter`, then
//! options through an `AVDictionary`, then `avfilter_init_dict` - rather than
//! by parsing a filtergraph string.  Two reasons, both measured
//! (`TODO.md` 0.7):
//!
//! - option values can contain characters the filtergraph parser treats as
//!   syntax, and it rejects them with `Trailing garbage after a filter`;
//! - `hwupload` reads `hw_device_ctx` in its `init`, and the parser
//!   initialises every filter immediately, so there is no point at which a
//!   device could be attached in the string form.
//!
//! Each filter that needs one gets the shared device before it initialises,
//! which is also what makes the capture sources use *our* D3D11 device
//! instead of creating one of their own - the equivalent of the `ffmpeg`
//! CLI's `-filter_hw_device`.

#![allow(
    unsafe_code,
    reason = "FFmpeg filter graph construction; confined to media::ffmpeg"
)]

use std::ffi::CString;
use std::ptr;

use ffmpeg_sys_next as ff;

use super::device::Device;
use super::{errstr, log};
use crate::media::capture::{DeviceSlot, FilterStep};
use crate::media::settings::OutputSize;

/// Owns an `AVFilterGraph` and frees it on drop, taking every filter in it
/// with it.
pub(super) struct Graph(*mut ff::AVFilterGraph);

impl Drop for Graph {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this graph and free it exactly once; freeing a
            // graph also frees the filters it contains.
            unsafe { ff::avfilter_graph_free(&mut self.0) };
        }
    }
}

/// What the configured graph will hand out.
#[derive(Debug, Clone, Copy)]
pub(super) struct SinkFormat {
    /// Frame width the sink produces.
    pub(super) width: i32,
    /// Frame height the sink produces.
    pub(super) height: i32,
    /// Pixel format, as an `AVPixelFormat` discriminant.
    pub(super) pix_fmt: i32,
}

impl SinkFormat {
    /// The size, for comparing against what the settings asked for.
    pub(super) fn size(self) -> OutputSize {
        OutputSize {
            width: u32::try_from(self.width).unwrap_or(0),
            height: u32::try_from(self.height).unwrap_or(0),
        }
    }
}

/// What one [`CaptureGraph::pull`] attempt produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pulled {
    /// A frame; the caller's frame now holds it.
    Frame,
    /// Nothing yet.  `gfxcapture` returns this after waiting a second
    /// without the source repainting, which for an idle window is normal.
    Idle,
    /// The source ended: window closed, or display detached.
    Ended,
}

/// A configured capture graph, ready to pull frames from.
pub(super) struct CaptureGraph {
    /// Keeps the graph, and so every filter and the sink, alive.  Never read
    /// directly - the sink pointer below is the handle we use.
    _graph: Graph,
    /// The `buffersink` at the end of the chain.  Borrowed from `_graph`,
    /// which is why it must not outlive it.
    sink: *mut ff::AVFilterContext,
    /// What the sink produces.
    format: SinkFormat,
}

// SAFETY: an `AVFilterGraph` has no thread affinity of its own; FFmpeg's
// contract is that one thread uses a graph at a time.  The pipeline moves a
// graph to its capture thread at startup and only that thread touches it
// afterwards.
unsafe impl Send for CaptureGraph {}

impl CaptureGraph {
    /// What this graph's frames look like.
    pub(super) fn format(&self) -> SinkFormat {
        self.format
    }

    /// The hardware frames context the sink's frames belong to, or null for
    /// system-memory frames.
    ///
    /// The encoder needs a reference to this to accept hardware frames.
    pub(super) fn hw_frames_ctx(&self) -> *mut ff::AVBufferRef {
        // SAFETY: `self.sink` is a live filter inside `self.graph`.
        unsafe { ff::av_buffersink_get_hw_frames_ctx(self.sink) }
    }

    /// Pull one frame, blocking until the source produces something.
    ///
    /// Blocking is unavoidable: both capture filters wait internally for the
    /// next frame, and `gfxcapture` waits up to a second before returning
    /// empty-handed.  A stop request is therefore noticed at the *next* loop
    /// iteration, not immediately - bounded by roughly a second.
    pub(super) fn pull(&mut self, frame: &mut super::frame::Frame) -> Result<Pulled, String> {
        // SAFETY: `self.sink` is live, and `frame` is an allocated frame the
        // caller owns; `av_buffersink_get_frame` overwrites its contents,
        // unreferencing whatever was there.
        let ret = unsafe { ff::av_buffersink_get_frame(self.sink, frame.as_ptr()) };
        if ret == ff::AVERROR_EOF || ret == ff::AVERROR_OUTPUT_CHANGED {
            return Ok(Pulled::Ended);
        }
        if ret == ff::AVERROR(ff::EAGAIN) {
            return Ok(Pulled::Idle);
        }
        if ret < 0 {
            return Err(format!("capture failed ({})", errstr(ret)));
        }
        Ok(Pulled::Frame)
    }
}

/// Build and configure a graph from `steps`.
///
/// `d3d11` is the shared capture device.  `d3d12` is only needed when the
/// plan uploads to a D3D12 encoder, and is created lazily by the caller.
pub(super) fn build(
    steps: &[FilterStep],
    d3d11: &Device,
    d3d12: Option<&Device>,
) -> Result<CaptureGraph, String> {
    if steps.is_empty() {
        return Err("empty filter chain".to_owned());
    }

    log::begin_capture();
    let built = build_inner(steps, d3d11, d3d12);
    let logged = log::take_errors();
    built.map_err(|reason| {
        if logged.is_empty() {
            reason
        } else {
            format!("{reason}: {}", logged.join("; "))
        }
    })
}

/// The fallible part of [`build`], with `FFmpeg`'s log text captured around it.
fn build_inner(
    steps: &[FilterStep],
    d3d11: &Device,
    d3d12: Option<&Device>,
) -> Result<CaptureGraph, String> {
    // SAFETY: allocates an empty graph; ownership passes to the guard.
    let graph = Graph(unsafe { ff::avfilter_graph_alloc() });
    if graph.0.is_null() {
        return Err("could not allocate filter graph".to_owned());
    }

    let mut previous: *mut ff::AVFilterContext = ptr::null_mut();
    for (index, step) in steps.iter().enumerate() {
        let ctx = create_filter(graph.0, index, step, d3d11, d3d12)?;
        if !previous.is_null() {
            // SAFETY: both filters are live and belong to `graph`; every
            // filter in our chains has exactly one input and one output.
            let ret = unsafe { ff::avfilter_link(previous, 0, ctx, 0) };
            if ret < 0 {
                return Err(format!("could not link into {} ({})", step.name, errstr(ret)));
            }
        }
        previous = ctx;
    }

    let sink = create_sink(graph.0, previous)?;

    // SAFETY: `graph.0` is fully built; passing no log context is allowed.
    let ret = unsafe { ff::avfilter_graph_config(graph.0, ptr::null_mut()) };
    if ret < 0 {
        return Err(format!("could not configure filter graph ({})", errstr(ret)));
    }

    // SAFETY: the graph is configured, so the sink's negotiated format and
    // dimensions are settled.
    let format = unsafe {
        SinkFormat {
            width: ff::av_buffersink_get_w(sink),
            height: ff::av_buffersink_get_h(sink),
            pix_fmt: ff::av_buffersink_get_format(sink),
        }
    };

    Ok(CaptureGraph { _graph: graph, sink, format })
}

/// Create one filter, attach its device, and initialise it with its options.
fn create_filter(
    graph: *mut ff::AVFilterGraph,
    index: usize,
    step: &FilterStep,
    d3d11: &Device,
    d3d12: Option<&Device>,
) -> Result<*mut ff::AVFilterContext, String> {
    let name = CString::new(step.name).map_err(|_| "filter name contains NUL".to_owned())?;
    // SAFETY: `name` is NUL-terminated; the returned filter is a static
    // descriptor owned by FFmpeg, and null when the build lacks it.
    let filter = unsafe { ff::avfilter_get_by_name(name.as_ptr()) };
    if filter.is_null() {
        return Err(format!("filter {} is not in this FFmpeg build", step.name));
    }

    // Instance names must be unique within a graph, and our chains can use
    // the same filter twice (`format` appears before and after `scale`).
    let instance = CString::new(format!("f{index}_{}", step.name))
        .map_err(|_| "filter instance name contains NUL".to_owned())?;
    // SAFETY: `graph` is live, `filter` is a static descriptor, and
    // `instance` is NUL-terminated.
    let ctx = unsafe { ff::avfilter_graph_alloc_filter(graph, filter, instance.as_ptr()) };
    if ctx.is_null() {
        return Err(format!("could not allocate filter {}", step.name));
    }

    attach_device(ctx, step.device, d3d11, d3d12)?;
    init_options(ctx, step)?;
    Ok(ctx)
}

/// Give a filter its hardware device, before it initialises.
fn attach_device(
    ctx: *mut ff::AVFilterContext,
    slot: Option<DeviceSlot>,
    d3d11: &Device,
    d3d12: Option<&Device>,
) -> Result<(), String> {
    let device = match slot {
        None => return Ok(()),
        Some(DeviceSlot::D3d11) => d3d11,
        Some(DeviceSlot::D3d12) => {
            d3d12.ok_or_else(|| "no D3D12 device for this chain".to_owned())?
        }
    };

    // SAFETY: the filter takes ownership of the reference it is given, so we
    // hand over a new one and keep ours in the `Device` guard.
    unsafe {
        (*ctx).hw_device_ctx = ff::av_buffer_ref(device.as_ptr());
        if (*ctx).hw_device_ctx.is_null() {
            return Err("could not reference hardware device".to_owned());
        }
    }
    Ok(())
}

/// Initialise a filter with its options.
fn init_options(ctx: *mut ff::AVFilterContext, step: &FilterStep) -> Result<(), String> {
    let mut dict: *mut ff::AVDictionary = ptr::null_mut();
    for (key, value) in &step.options {
        let Ok(key_c) = CString::new(*key) else { continue };
        let Ok(value_c) = CString::new(value.as_str()) else { continue };
        // SAFETY: `dict` starts null, which `av_dict_set` treats as "create";
        // both strings are valid for the duration of the call, and their
        // contents are copied.
        let ret = unsafe { ff::av_dict_set(&mut dict, key_c.as_ptr(), value_c.as_ptr(), 0) };
        if ret < 0 {
            // SAFETY: `dict` is either null or a dictionary we own.
            unsafe { ff::av_dict_free(&mut dict) };
            return Err(format!("could not set {}={value} ({})", key, errstr(ret)));
        }
    }

    // SAFETY: `ctx` is an allocated, uninitialised filter; `init_dict`
    // consumes the options it recognises and leaves the rest behind.
    let ret = unsafe { ff::avfilter_init_dict(ctx, &mut dict) };
    // SAFETY: whatever `init_dict` left is still ours to free.
    let leftover = unsafe { remaining_keys(dict) };
    unsafe { ff::av_dict_free(&mut dict) };

    if ret < 0 {
        return Err(format!("could not initialise {} ({})", step.name, errstr(ret)));
    }
    if !leftover.is_empty() {
        // An unrecognised option means the filter silently ignored something
        // we asked for - a cursor setting, a scale target - so say so rather
        // than letting the stream look subtly wrong.
        return Err(format!("{} rejected option(s): {}", step.name, leftover.join(", ")));
    }
    Ok(())
}

/// The keys still in a dictionary, i.e. the options a filter did not take.
///
/// # Safety
///
/// `dict` must be null or a dictionary we own.
unsafe fn remaining_keys(dict: *mut ff::AVDictionary) -> Vec<String> {
    let mut keys = Vec::new();
    let mut entry: *const ff::AVDictionaryEntry = ptr::null();
    let Ok(empty) = CString::new("") else {
        return keys;
    };
    loop {
        // SAFETY: an empty key with `AV_DICT_IGNORE_SUFFIX` iterates every
        // entry; passing the previous entry advances, and null starts over.
        entry = unsafe {
            ff::av_dict_get(dict, empty.as_ptr(), entry, ff::AV_DICT_IGNORE_SUFFIX)
        };
        if entry.is_null() {
            return keys;
        }
        // SAFETY: a non-null entry has a NUL-terminated key.
        keys.push(unsafe { super::cstr((*entry).key) });
    }
}

/// Append a `buffersink` and link the chain into it.
fn create_sink(
    graph: *mut ff::AVFilterGraph,
    last: *mut ff::AVFilterContext,
) -> Result<*mut ff::AVFilterContext, String> {
    let name = CString::new("buffersink").map_err(|_| "buffersink name".to_owned())?;
    let instance = CString::new("out").map_err(|_| "sink instance name".to_owned())?;

    // SAFETY: `buffersink` is always built into FFmpeg; both strings are
    // NUL-terminated and `graph` is live.
    let sink = unsafe {
        let filter = ff::avfilter_get_by_name(name.as_ptr());
        if filter.is_null() {
            return Err("buffersink is not in this FFmpeg build".to_owned());
        }
        ff::avfilter_graph_alloc_filter(graph, filter, instance.as_ptr())
    };
    if sink.is_null() {
        return Err("could not allocate buffersink".to_owned());
    }

    // SAFETY: `sink` is allocated and uninitialised; `buffersink` needs no
    // options, and accepts a null argument string.
    let ret = unsafe { ff::avfilter_init_str(sink, ptr::null()) };
    if ret < 0 {
        return Err(format!("could not initialise buffersink ({})", errstr(ret)));
    }

    // SAFETY: both filters are live and in the same graph.
    let ret = unsafe { ff::avfilter_link(last, 0, sink, 0) };
    if ret < 0 {
        return Err(format!("could not link into buffersink ({})", errstr(ret)));
    }
    Ok(sink)
}
