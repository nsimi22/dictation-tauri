// Text injection into whatever app currently has focus.
//
// On macOS we deliberately *don't* take the "copy to clipboard + simulate
// Cmd+V" route used by the old Python build. That path uses TIS-backed
// keycode lookups (`Cmd` and `V` translation), which on macOS 26 abort if
// they ever run off the main dispatch queue. enigo's `text(...)` route uses
// `CGEventKeyboardSetUnicodeString` to inject the literal characters as a
// synthetic event, sidestepping TIS entirely, sidestepping the clipboard,
// and incidentally handling emoji / non-ANSI characters for free.

use enigo::{Enigo, Keyboard, Settings};
use parking_lot::Mutex;

pub struct Injector {
    /// enigo is lazily constructed because building it touches OS subsystems
    /// (Accessibility on macOS) — we want any failure to surface at first
    /// dictation, not at app startup, so the user can see the connection.
    inner: Mutex<Option<Enigo>>,
}

impl Injector {
    pub fn new() -> Self {
        Self { inner: Mutex::new(None) }
    }

    /// Type `text` into the focused window. Returns a UI-friendly string
    /// error so callers can forward it straight to the activity log.
    pub fn send(&self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let mut guard = self.inner.lock();
        if guard.is_none() {
            *guard = Some(
                Enigo::new(&Settings::default())
                    .map_err(|e| format!("Enigo init failed: {e}"))?,
            );
        }
        let enigo = guard.as_mut().expect("just populated");
        enigo
            .text(text)
            .map_err(|e| format!("Enigo text failed: {e}"))
    }
}
