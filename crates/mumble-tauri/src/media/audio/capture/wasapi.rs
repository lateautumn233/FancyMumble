//! WASAPI device and process loopback; no whole-device fallback for windows.
#![allow(
    unsafe_code,
    reason = "COM calls and borrowed WASAPI buffers are confined to their owning capture thread"
)]

use std::mem::ManuallyDrop;
use std::sync::mpsc;
use std::time::Duration;

use windows::core::{implement, Interface, HRESULT};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize,
    StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0},
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::media::audio::encoding::CaptureClock;
use crate::media::capture::CaptureSource;

pub(super) struct Com;
impl Com {
    pub(super) fn new() -> Result<Self, String> {
        // This is a dedicated native thread, initialized once before all COM owners.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| e.to_string())?;
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(super) struct Capture {
    client: IAudioClient,
    reader: IAudioCaptureClient,
    window: Option<(u64, u32)>,
    started: bool,
    clock: CaptureClock,
}

impl Capture {
    pub(super) fn open(source: CaptureSource) -> Result<Self, String> {
        let window = match source {
            CaptureSource::Window { hwnd } => Some((hwnd, window_process(hwnd)?)),
            CaptureSource::Monitor { .. } => None,
        };
        let client = match window {
            Some((_, pid)) => process_client(pid).map_err(|e| {
                format!(
                    "could not capture window audio (requires Windows build 20348 or newer): {e}"
                )
            })?,
            None => device_client()?,
        };
        Self::from_client(client, window)
    }

    fn from_client(client: IAudioClient, window: Option<(u64, u32)>) -> Result<Self, String> {
        let format = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // WASAPI performs sample-rate/channel conversion into the Opus input format.
        unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 200_000, 0, &format, None) }
            .map_err(|e| format!("could not initialize screen-share audio: {e}"))?;
        let reader =
            unsafe { client.GetService::<IAudioCaptureClient>() }.map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            reader,
            window,
            started: false,
            clock: CaptureClock::default(),
        })
    }

    pub(super) fn start(&mut self) -> Result<(), String> {
        unsafe { self.client.Start() }.map_err(|e| e.to_string())?;
        self.started = true;
        Ok(())
    }

    pub(super) fn validate_source(&self) -> Result<(), String> {
        if let Some((hwnd, pid)) = self.window {
            if window_process(hwnd)? != pid {
                return Err("shared window process changed".to_owned());
            }
        }
        Ok(())
    }

    pub(super) fn read(&mut self, samples: &mut Vec<f32>) -> Result<Option<u64>, String> {
        if unsafe { self.reader.GetNextPacketSize() }.map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let mut data = std::ptr::null_mut();
        let (mut frames, mut flags, mut qpc) = (0, 0, 0);
        unsafe {
            self.reader
                .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
        }
        .map_err(|e| e.to_string())?;
        // Every successful GetBuffer is paired, including malformed/silent packets.
        let guard = Buffer {
            reader: &self.reader,
            frames,
        };
        if frames > 48_000 {
            return Err("unexpectedly large WASAPI audio buffer".to_owned());
        }
        let count = frames as usize * 2;
        samples.clear();
        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            samples.resize(count, 0.0);
        } else if count > 0 {
            if data.is_null() {
                return Err("WASAPI returned an empty audio buffer".to_owned());
            }
            // The client was initialized with interleaved stereo IEEE float samples.
            samples.extend_from_slice(unsafe {
                std::slice::from_raw_parts(data.cast::<f32>(), count)
            });
        }
        drop(guard);
        let timestamp =
            (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && qpc != 0).then_some(qpc);
        Ok(Some(self.clock.advance(frames, timestamp)))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.started {
            let _ = unsafe { self.client.Stop() };
        }
    }
}

struct Buffer<'a> {
    reader: &'a IAudioCaptureClient,
    frames: u32,
}
impl Drop for Buffer<'_> {
    fn drop(&mut self) {
        let _ = unsafe { self.reader.ReleaseBuffer(self.frames) };
    }
}

fn window_process(hwnd: u64) -> Result<u32, String> {
    let handle = usize::try_from(hwnd).map_err(|_| "invalid window handle".to_owned())?;
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(handle as _, &mut pid) };
    if thread == 0 || pid == 0 {
        return Err("shared window no longer exists".to_owned());
    }
    Ok(pid)
}

fn device_client() -> Result<IAudioClient, String> {
    // Endpoint and activation objects are created and dropped in the same MTA.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let endpoint = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("no default audio output device: {e}"))?;
        endpoint
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| e.to_string())
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

fn process_client(pid: u32) -> Result<IAudioClient, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let completion: IActivateAudioInterfaceCompletionHandler = Completion(tx).into();
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // PROPVARIANT's Drop calls PropVariantClear. Suppress it for this borrowed
    // stack blob; ActivateAudioInterfaceAsync copies the activation parameters.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&raw mut params).cast(),
                    },
                },
                ..Default::default()
            }),
        },
    });
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &completion,
        )
    }
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|e| format!("window audio activation timed out: {e}"))?;
    let mut result = HRESULT(0);
    let mut interface = None;
    unsafe { operation.GetActivateResult(&mut result, &mut interface) }
        .map_err(|e| e.to_string())?;
    result.ok().map_err(|e| e.to_string())?;
    interface
        .ok_or("window audio activation returned no interface")?
        .cast()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
