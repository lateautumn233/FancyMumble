//! Thread ownership and shutdown for WASAPI screen-share audio.

mod wasapi;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::broadcast;

use super::{encoding::Encoder, Packet};
use crate::media::capture::CaptureSource;

/// Owns the native thread; all COM objects stay on that thread.
pub(crate) struct Handle {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn start(
        source: CaptureSource,
        packets: broadcast::Sender<Arc<Packet>>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_error = Arc::clone(&error);
        let (ready, result) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("screenshare-audio".to_owned())
            .spawn(move || {
                let outcome = run(source, packets, &thread_stop, &ready);
                if let Err(message) = outcome {
                    tracing::error!(error = %message, "screen-share audio stopped");
                    let _ = ready.try_send(Err(message.clone()));
                    if let Ok(mut error) = thread_error.lock() {
                        *error = Some(message);
                    }
                }
            })
            .map_err(|e| format!("could not start audio thread: {e}"))?;
        let handle = Self {
            stop,
            error,
            thread: Some(thread),
        };
        result
            .recv()
            .map_err(|e| format!("audio startup failed: {e}"))??;
        Ok(handle)
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error
            .lock()
            .ok()
            .and_then(|error| error.clone())
            .or_else(|| {
                self.thread
                    .as_ref()
                    .filter(|thread| thread.is_finished())
                    .map(|_| "screen-share audio thread stopped".to_owned())
            })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    source: CaptureSource,
    packets: broadcast::Sender<Arc<Packet>>,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let _com = wasapi::Com::new()?;
    let mut capture = wasapi::Capture::open(source)?;
    let mut encoder = Encoder::new()?;
    capture.start()?;
    ready
        .send(Ok(()))
        .map_err(|_| "audio startup cancelled".to_owned())?;
    tracing::info!(
        ?source,
        sample_rate = 48_000,
        channels = 2,
        bitrate = 128_000,
        "screen-share audio started"
    );
    let mut samples = Vec::new();
    while !stop.load(Ordering::Acquire) {
        capture.validate_source()?;
        if let Some(pts) = capture.read(&mut samples)? {
            encoder.push(&samples, pts, |packet| {
                let _ = packets.send(Arc::new(packet));
            })?;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}
