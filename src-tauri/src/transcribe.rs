// whisper-rs integration. Phase 2b: take the f32 sample buffer that Phase 2a
// hands us, downsample to 16 kHz (Whisper's input rate), run inference, and
// return the recognised text. The model file lives next to the app's data
// directory; if it's missing we surface a clear error rather than crashing.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Hugging Face URL the UI shows for the default model when the user
/// hasn't downloaded one yet. The actual model file name now comes from
/// settings.json so users can swap to base.en or medium.en.
pub const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";

pub struct Transcriber {
    /// Path the model is expected at. Used for the missing-model error.
    model_path: PathBuf,
    /// Lazily loaded so app startup isn't blocked by ~1s of weight loading.
    context: OnceLock<WhisperContext>,
    /// Whisper state is not Sync; keep a single mutable state behind a
    /// Mutex and serialize transcriptions. For a hold-to-talk UX that fires
    /// once per utterance this is plenty.
    state_lock: Mutex<()>,
}

impl Transcriber {
    pub fn new(model_path: PathBuf) -> Self {
        Self {
            model_path,
            context: OnceLock::new(),
            state_lock: Mutex::new(()),
        }
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    pub fn is_model_present(&self) -> bool {
        self.model_path.is_file()
    }

    /// Run inference on `samples` (mono f32) captured at `sample_rate`.
    /// Downsamples to 16 kHz internally; returns the joined transcript text
    /// already stripped of leading whitespace.
    pub fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String, String> {
        if !self.is_model_present() {
            return Err(format!(
                "Whisper model not found at {}",
                self.model_path.display()
            ));
        }
        if samples.is_empty() || sample_rate == 0 {
            return Ok(String::new());
        }

        let ctx = self.context.get_or_init(|| {
            // OnceLock::get_or_init can't return a Result, so on a real
            // failure here we'd want to log + propagate. Practical workaround:
            // panic, then the surrounding `catch_unwind`-free worker thread
            // returns an error string to the UI via the channel. For now we
            // expect this path only when the file exists (checked above).
            WhisperContext::new_with_params(
                self.model_path.to_str().unwrap_or_default(),
                WhisperContextParameters::default(),
            )
            .expect("failed to initialise WhisperContext")
        });

        // Whisper expects 16 kHz mono. cpal on macOS hands us 48 kHz typically;
        // do a cheap linear-interp resample. Good enough for speech; we can
        // upgrade to `rubato` if quality becomes the bottleneck.
        let resampled = resample_to_16k(samples, sample_rate);

        let _guard = self.state_lock.lock();
        let mut state = ctx
            .create_state()
            .map_err(|e| format!("create_state: {e}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &resampled)
            .map_err(|e| format!("whisper full: {e}"))?;

        // whisper-rs 0.16 returns c_int directly here (not a Result) and
        // exposes segment text via `state.get_segment(i)?.to_str()`.
        let n_segments = state.full_n_segments();
        let mut text = String::new();
        for i in 0..n_segments {
            if let Some(seg) = state.get_segment(i) {
                match seg.to_str() {
                    Ok(s) => text.push_str(s),
                    Err(e) => return Err(format!("segment.to_str: {e}")),
                }
            }
        }
        Ok(text.trim_start().to_string())
    }
}

/// Crude linear-interpolation resampler to 16 kHz. Good enough for Whisper —
/// Whisper's own mel-spectrogram front-end is robust to mild aliasing. Real
/// resampling (with a low-pass filter) goes in a follow-up if we ever serve
/// music or production-quality audio.
fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    const DST_RATE: u32 = 16_000;
    if src_rate == DST_RATE {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return Vec::new();
    }
    let ratio = src_rate as f64 / DST_RATE as f64;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let s0 = samples[idx];
        let s1 = *samples.get(idx + 1).unwrap_or(&s0);
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}
