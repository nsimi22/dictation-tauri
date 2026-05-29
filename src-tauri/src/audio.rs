// Audio capture via cpal with pre-warmed stream.
//
// Earlier version opened the cpal stream inside the hotkey press handler.
// On macOS, the first call to `InputStream::play()` after a cold device
// takes ~100–200ms to actually start delivering samples, which means the
// user's first phoneme or two was never captured — they reported the
// transcript "cuts off my first word or 2 every time".
//
// Fix: open the stream once at arm() time (Start dictation) and keep it
// running. The callback always writes into a Mutex<Vec> when the
// `recording` flag is set, and discards otherwise. start_recording() and
// stop_recording() become instant atomic flag flips with no device I/O,
// so press-to-first-sample latency is one audio buffer (~10ms) rather
// than a device wake-up.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

pub struct AudioCapture {
    /// Joined when disarm() is called. None while disarmed.
    worker: Option<thread::JoinHandle<()>>,
    /// Sent by disarm() to wake the worker and let it drop the cpal stream.
    disarm_tx: Option<mpsc::Sender<()>>,
    /// Buffer the cpal callback writes into when `recording` is true.
    /// Cleared by start_recording(); taken by stop_recording().
    samples: Arc<Mutex<Vec<f32>>>,
    /// Sample rate observed at stream open. 0 while disarmed.
    sample_rate: Arc<AtomicU32>,
    /// Set true between start_recording / stop_recording. The cpal callback
    /// checks this on each invocation and skips writes when false.
    recording: Arc<AtomicBool>,
}

impl AudioCapture {
    pub fn new() -> Self {
        Self {
            worker: None,
            disarm_tx: None,
            samples: Arc::new(Mutex::new(Vec::new())),
            sample_rate: Arc::new(AtomicU32::new(0)),
            recording: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_armed(&self) -> bool {
        self.worker.is_some()
    }

    /// Open an input device and keep its stream running. `device_name` of
    /// None selects the system default; Some("…") picks by exact name. A
    /// device-open failure (mic permission denied, name not found, etc.)
    /// is returned synchronously so the caller can surface it.
    pub fn arm(&mut self, device_name: Option<String>) -> Result<(), String> {
        if self.is_armed() {
            return Ok(());
        }
        self.recording.store(false, Ordering::SeqCst);
        self.samples.lock().clear();

        let samples = Arc::clone(&self.samples);
        let sample_rate = Arc::clone(&self.sample_rate);
        let recording = Arc::clone(&self.recording);
        let (disarm_tx, disarm_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = thread::spawn(move || {
            // cpal::Stream is !Send on macOS — it must live on this thread.
            let host = cpal::default_host();
            let device = match resolve_input_device(&host, device_name.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let supported = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("default_input_config: {e}")));
                    return;
                }
            };
            let channels = supported.channels() as usize;
            sample_rate.store(supported.sample_rate(), Ordering::SeqCst);
            let config: cpal::StreamConfig = supported.clone().into();
            let err_fn = |e| eprintln!("audio stream error: {e}");

            let samples_cb = Arc::clone(&samples);
            let recording_cb = Arc::clone(&recording);
            let on_data = move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Hot path: check the flag first and exit cheaply if not
                // recording. When recording, downmix to mono inline so the
                // buffer is already in the shape whisper-rs expects.
                if !recording_cb.load(Ordering::Relaxed) {
                    return;
                }
                let mut buf = samples_cb.lock();
                if channels <= 1 {
                    buf.extend_from_slice(data);
                } else {
                    for frame in data.chunks_exact(channels) {
                        let sum: f32 = frame.iter().copied().sum();
                        buf.push(sum / channels as f32);
                    }
                }
            };

            let stream = match supported.sample_format() {
                cpal::SampleFormat::F32 => device.build_input_stream(&config, on_data, err_fn, None),
                other => {
                    let _ = ready_tx.send(Err(format!(
                        "Unsupported sample format {other:?}; expected F32",
                    )));
                    return;
                }
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("build_input_stream: {e}")));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(format!("stream.play: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));

            // Park here until disarm; dropping `stream` stops capture.
            let _ = disarm_rx.recv();
            drop(stream);
        });

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.disarm_tx = Some(disarm_tx);
                self.worker = Some(handle);
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err("Audio worker exited without reporting ready".into())
            }
        }
    }

    /// Tear down the cpal stream. Safe to call when already disarmed.
    pub fn disarm(&mut self) {
        self.recording.store(false, Ordering::SeqCst);
        if let Some(tx) = self.disarm_tx.take() {
            let _ = tx.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.samples.lock().clear();
        self.sample_rate.store(0, Ordering::SeqCst);
    }

    /// Begin saving samples. Returns true if the stream was armed and we
    /// actually started capturing; false if disarmed (caller may want to
    /// arm() lazily or surface an error).
    pub fn start_recording(&self) -> bool {
        if !self.is_armed() {
            return false;
        }
        self.samples.lock().clear();
        self.recording.store(true, Ordering::SeqCst);
        true
    }

    /// Stop saving samples and return them with the device sample rate.
    pub fn stop_recording(&self) -> (Vec<f32>, u32) {
        self.recording.store(false, Ordering::SeqCst);
        let samples = std::mem::take(&mut *self.samples.lock());
        let rate = self.sample_rate.load(Ordering::SeqCst);
        (samples, rate)
    }
}

/// Look up an input device by name, falling back to the system default
/// when `name` is None. Returns a descriptive error if `name` is given but
/// no matching device is present (e.g. headphones unplugged since the
/// user picked them in settings).
#[allow(deprecated)] // cpal recommends description()/id() but name() still works
fn resolve_input_device(
    host: &cpal::Host,
    name: Option<&str>,
) -> Result<cpal::Device, String> {
    match name {
        None => host
            .default_input_device()
            .ok_or_else(|| "No default input device".to_string()),
        Some(target) => {
            let devices = host
                .input_devices()
                .map_err(|e| format!("input_devices: {e}"))?;
            for d in devices {
                if d.name().map(|n| n == target).unwrap_or(false) {
                    return Ok(d);
                }
            }
            Err(format!("Input device {target:?} not found; reconnect it or pick another"))
        }
    }
}

/// Enumerate all input devices the OS exposes. Used by the settings UI
/// to populate the device dropdown.
#[allow(deprecated)]
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|iter| iter.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}
