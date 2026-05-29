// dictation-tauri Phase 1: prove the hotkey loop works on macOS 26 without
// pulling in a transcription engine yet. The Rust core registers a global
// hotkey on demand and emits Tauri events to the frontend on press/release.

mod audio;
mod inject;
mod settings;
mod transcribe;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;

use parking_lot::Mutex as PlMutex;
use tauri::{Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::audio::AudioCapture;
use crate::inject::Injector;
use crate::settings::Settings;
use crate::transcribe::{Transcriber, DEFAULT_MODEL_URL};

/// Default hotkey when settings.hotkey can't be parsed (corrupt config or
/// user typo). F13 sidesteps modifier-only edge cases and is unlikely to be
/// pressed by accident during typing.
const FALLBACK_HOTKEY: &str = "F13";

fn parse_hotkey(s: &str) -> Shortcut {
    Shortcut::from_str(s)
        .or_else(|_| Shortcut::from_str(FALLBACK_HOTKEY))
        .expect("F13 always parses")
}

struct AppState {
    /// Whether the dictation hotkey is currently armed (registered with the OS).
    /// Toggled by the JS-facing start/stop commands.
    active: Mutex<bool>,
    /// Microphone capture. Owned by the AppState so press/release handlers
    /// on the OS thread can drive start/stop without channels.
    audio: PlMutex<AudioCapture>,
    /// Whisper model wrapper. PlMutex<Arc> so we can atomically swap when
    /// the user picks a different model in settings; readers (worker
    /// threads) clone the Arc.
    transcriber: PlMutex<Arc<Transcriber>>,
    /// Synthetic input. Arc'd alongside the transcriber so the same worker
    /// thread that received the transcript can immediately type it.
    injector: Arc<Injector>,
    /// User-editable settings, live-mirrored from settings.json.
    settings: PlMutex<Settings>,
    /// Where settings.json lives (set by setup()).
    settings_path: PathBuf,
    /// App data directory. models/ lives under it.
    data_dir: PathBuf,
    /// The Shortcut currently registered with the OS, so we can unregister
    /// it precisely even after the user changes the hotkey string.
    registered_shortcut: PlMutex<Option<Shortcut>>,
}

#[tauri::command]
fn hotkey_label(state: State<'_, AppState>) -> String {
    state.settings.lock().hotkey.clone()
}

/// Returns "ready" if the listener is currently armed, else "stopped".
/// JS calls this on page load to align the UI with the real backend state,
/// since Vite HMR or a window refresh can re-init the frontend while the
/// Rust core keeps the hotkey registered.
#[tauri::command]
fn listening_state(state: State<'_, AppState>) -> &'static str {
    match state.active.lock() {
        Ok(g) if *g => "ready",
        _ => "stopped",
    }
}

#[tauri::command]
fn model_info(state: State<'_, AppState>) -> serde_json::Value {
    let transcriber = state.transcriber.lock().clone();
    let settings = state.settings.lock();
    serde_json::json!({
        "path": transcriber.model_path().display().to_string(),
        "present": transcriber.is_model_present(),
        "url": DEFAULT_MODEL_URL,
        "filename": settings.model_file.clone(),
    })
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
fn list_input_devices() -> Vec<String> {
    audio::list_input_devices()
}

#[tauri::command]
fn list_models(state: State<'_, AppState>) -> Vec<String> {
    let models_dir = state.data_dir.join("models");
    let mut found: Vec<String> = std::fs::read_dir(&models_dir)
        .map(|iter| {
            iter.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".bin") {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    new_settings: Settings,
) -> Result<(), String> {
    // Validate hotkey parses before we commit anything to disk; otherwise
    // a typo would brick the app on next launch.
    Shortcut::from_str(&new_settings.hotkey)
        .map_err(|e| format!("invalid hotkey {:?}: {e}", new_settings.hotkey))?;

    settings::save(&state.settings_path, &new_settings)?;
    let old = std::mem::replace(&mut *state.settings.lock(), new_settings.clone());

    let active = *state.active.lock().map_err(|e| e.to_string())?;

    // Apply hotkey change live so users see it take effect without a restart.
    if old.hotkey != new_settings.hotkey && active {
        let new_shortcut = parse_hotkey(&new_settings.hotkey);
        let mut registered = state.registered_shortcut.lock();
        if let Some(old_shortcut) = *registered {
            let _ = app.global_shortcut().unregister(old_shortcut);
        }
        app.global_shortcut()
            .register(new_shortcut)
            .map_err(|e| format!("register new hotkey: {e}"))?;
        *registered = Some(new_shortcut);
    }

    // Mic device change: disarm + rearm with the new device so the next
    // press uses the right input. If rearm fails (device gone), surface the
    // error but leave the app armed-with-default rather than dead.
    if old.input_device != new_settings.input_device && active {
        let mut audio = state.audio.lock();
        audio.disarm();
        if let Err(e) = audio.arm(new_settings.input_device.clone()) {
            // Try falling back to default so the listener stays usable.
            let _ = audio.arm(None);
            return Err(format!("Mic change failed, fell back to default: {e}"));
        }
    }

    // Model change: rebuild the Transcriber so the next utterance loads the
    // new weights. Cheap structurally; the actual file load happens lazily
    // on first transcribe.
    if old.model_file != new_settings.model_file {
        let model_path = state
            .data_dir
            .join("models")
            .join(&new_settings.model_file);
        *state.transcriber.lock() = Arc::new(Transcriber::new(model_path));
        let _ = app.emit(
            "model:state",
            if state.transcriber.lock().is_model_present() {
                "ready"
            } else {
                "missing"
            },
        );
    }

    Ok(())
}

#[tauri::command]
fn start_listening(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut active = state.active.lock().map_err(|e| e.to_string())?;
    if *active {
        return Ok(());
    }
    // Pre-warm the cpal stream so the first hotkey press doesn't wait for
    // the audio device to wake up (~100–200ms on a cold CoreAudio device).
    // This is the difference between the user losing their first word and
    // having clean leading audio.
    let (hotkey_str, device_name) = {
        let s = state.settings.lock();
        (s.hotkey.clone(), s.input_device.clone())
    };
    {
        let mut audio = state.audio.lock();
        audio
            .arm(device_name)
            .map_err(|e| format!("Could not arm audio: {e}"))?;
    }
    let shortcut = parse_hotkey(&hotkey_str);
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| format!("Could not register hotkey: {e}"))?;
    *state.registered_shortcut.lock() = Some(shortcut);
    *active = true;
    let _ = app.emit("listener:state", "ready");
    Ok(())
}

#[tauri::command]
fn stop_listening(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut active = state.active.lock().map_err(|e| e.to_string())?;
    if !*active {
        return Ok(());
    }
    if let Some(s) = state.registered_shortcut.lock().take() {
        let _ = app.global_shortcut().unregister(s);
    }
    {
        let mut audio = state.audio.lock();
        audio.disarm();
    }
    *active = false;
    let _ = app.emit("listener:state", "stopped");
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // App data dir is per-bundle on macOS (under Application Support).
            // The model + settings live there so they survive reinstalls and
            // aren't tucked inside the .app bundle itself.
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("app_data_dir should be available");
            let settings_path = data_dir.join("settings.json");
            let loaded = settings::load(&settings_path);
            let model_path = data_dir.join("models").join(&loaded.model_file);
            app.manage(AppState {
                active: Mutex::new(false),
                audio: PlMutex::new(AudioCapture::new()),
                transcriber: PlMutex::new(Arc::new(Transcriber::new(model_path))),
                injector: Arc::new(Injector::new()),
                settings: PlMutex::new(loaded),
                settings_path,
                data_dir,
                registered_shortcut: PlMutex::new(None),
            });
            // Tell the UI whether the model is already on disk; the frontend
            // hides the model-missing banner accordingly.
            let state = app.state::<AppState>();
            let _ = app.emit(
                "model:state",
                if state.transcriber.lock().is_model_present() {
                    "ready"
                } else {
                    "missing"
                },
            );
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Handler runs on the OS event thread. Audio start/stop
                    // is fast (worker spawn / channel send), so doing them
                    // inline here is fine. Anything heavy (transcription)
                    // moves to a Tauri async task in Phase 2b.
                    let state = app.state::<AppState>();
                    match event.state() {
                        ShortcutState::Pressed => {
                            // Audio stream is already running (armed at
                            // start_listening time). Just flip the flag.
                            let started = {
                                let audio = state.audio.lock();
                                audio.start_recording()
                            };
                            if started {
                                let _ = app.emit("hotkey:pressed", ());
                            } else {
                                let _ = app.emit(
                                    "capture:error",
                                    "Audio not armed — click Start dictation first".to_string(),
                                );
                            }
                        }
                        ShortcutState::Released => {
                            let (samples, sample_rate) = {
                                let audio = state.audio.lock();
                                audio.stop_recording()
                            };
                            let duration_ms = if sample_rate > 0 {
                                ((samples.len() as f64 / sample_rate as f64) * 1000.0) as u64
                            } else {
                                0
                            };
                            let _ = app.emit("hotkey:released", ());
                            let _ = app.emit(
                                "capture:done",
                                serde_json::json!({
                                    "samples": samples.len(),
                                    "sample_rate": sample_rate,
                                    "duration_ms": duration_ms,
                                }),
                            );

                            // Transcribe off the OS event thread — whisper
                            // inference can take 100ms–several seconds and
                            // must not block hotkey delivery.
                            let app_clone = app.clone();
                            let transcriber = state.transcriber.lock().clone();
                            let injector = Arc::clone(&state.injector);
                            thread::spawn(move || {
                                if !transcriber.is_model_present() {
                                    let _ = app_clone.emit(
                                        "transcribe:error",
                                        format!(
                                            "Model file missing: {}",
                                            transcriber.model_path().display()
                                        ),
                                    );
                                    return;
                                }
                                let _ = app_clone.emit("transcribe:start", ());
                                match transcriber.transcribe(&samples, sample_rate) {
                                    Ok(text) => {
                                        let _ = app_clone.emit("transcript", text.clone());
                                        // Type the transcript into the focused
                                        // app. A failure here is almost always
                                        // missing Accessibility permission —
                                        // surface the exact error so the user
                                        // can act on it.
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            if let Err(e) = injector.send(trimmed) {
                                                let _ = app_clone.emit("inject:error", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = app_clone.emit("transcribe:error", e);
                                    }
                                }
                            });
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            hotkey_label,
            listening_state,
            model_info,
            get_settings,
            update_settings,
            list_input_devices,
            list_models,
            start_listening,
            stop_listening,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
