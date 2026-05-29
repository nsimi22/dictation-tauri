// User-editable settings, persisted to disk.
//
// Lives next to the model file in the app data directory so a clean
// reinstall preserves user preferences:
//
//   ~/Library/Application Support/com.nicksimi.dictation/settings.json
//
// On load failure (file missing or malformed) we fall back to defaults
// silently — nothing here should ever block the app from starting.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Tauri shortcut string: "F13", "F14", "CommandOrControl+Shift+Space".
    /// Parsed by tauri-plugin-global-shortcut at apply time.
    pub hotkey: String,
    /// Whisper model file name (in the models/ dir).
    pub model_file: String,
    /// Optional input device name (e.g. "MacBook Pro Microphone").
    /// None / null => system default mic.
    pub input_device: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "F13".into(),
            model_file: "ggml-small.en.bin".into(),
            input_device: None,
        }
    }
}

pub fn load(path: &Path) -> Settings {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}
