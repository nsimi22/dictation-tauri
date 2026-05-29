# free-dictation (Tauri)

A **free, local, privacy-first** hold-to-talk dictation app. Hold a hotkey,
speak, release — your words are transcribed locally via
[whisper.cpp](https://github.com/ggerganov/whisper.cpp) and typed straight
into whatever app has focus.

Built on Tauri 2 (Rust + WebView). Successor to the Python version at
[nsimi22/dictation](https://github.com/nsimi22/dictation).

> **Status: alpha.** Working end-to-end on macOS 26 (Apple Silicon).
> Build-from-source only — no signed `.app` distribution yet.
> Cross-platform support is theoretically present but only macOS is tested.

## Why this exists

The Python version stopped working on macOS 26: pynput's keyboard backend
calls Text Input Source APIs from a background thread, and macOS 26
tightened the relevant `dispatch_assert_queue` to crash on that. This
rewrite drops pynput entirely. The Rust core uses Tauri's global-shortcut
plugin (Carbon hotkeys, no Accessibility needed for keypress *detection*),
`whisper-rs` for transcription (Metal-accelerated on Apple Silicon), and
`enigo`'s Quartz-CGEvent-based Unicode injection for typing — none of which
go through TIS.

## How it works

```
   ┌────────┐  hold hotkey   ┌──────────┐  release   ┌─────────────┐  types text
   │  you   │ ─────────────▶ │  record  │ ─────────▶ │ whisper.cpp │ ──────────▶ focused app
   └────────┘  (Tauri shortcut)│ (cpal)  │            │   (Metal)   │
                              └──────────┘            └─────────────┘
```

| Module | Responsibility |
|---|---|
| `src-tauri/src/lib.rs` | App glue, hotkey state machine, Tauri commands |
| `src-tauri/src/audio.rs` | cpal microphone capture, pre-warmed stream |
| `src-tauri/src/transcribe.rs` | whisper-rs wrapper + 16 kHz resampler |
| `src-tauri/src/inject.rs` | enigo Unicode keypress synthesis |
| `src-tauri/src/settings.rs` | Persistent user settings (JSON) |
| `src/` | TypeScript + Vite frontend (status pill, settings UI, activity log) |

## Build & run from source

### Requirements

- **macOS 12+** (tested on 26 / Apple Silicon)
- **Rust** stable — install via [rustup](https://rustup.rs)
- **Node.js 20+** with `npm`
- **Xcode Command Line Tools** (`xcode-select --install`)
- **cmake** (`brew install cmake`) — required by `whisper-rs` to build whisper.cpp

### Dev loop

```bash
git clone https://github.com/nsimi22/dictation-tauri.git
cd dictation-tauri
npm install
npm run tauri dev
```

The first build compiles whisper.cpp from source with Metal enabled
(~5–15 min on cold cache). Subsequent rebuilds are seconds.

### Production build (unsigned)

```bash
npm run tauri build
```

Output lands in `src-tauri/target/release/bundle/`. The unsigned `.app`
can be run after right-click → Open the first time.

## macOS permissions

You'll be prompted on first use:

- **Microphone** — required to capture audio (`cpal` opens a default
  input device).
- **Accessibility** — required for synthetic Cmd+V / Unicode keypress
  injection into other apps. Drag the running binary
  (`src-tauri/target/debug/dictation-tauri` for dev,
  `free-dictation.app` for builds) into
  System Settings → Privacy & Security → Accessibility and toggle it on.

## Whisper model

The default model is `ggml-small.en.bin` (~470 MB). Drop the file into:

```
~/Library/Application Support/com.nicksimi.dictation/models/
```

Or use the URL the in-app banner surfaces when the file is missing.
Other models from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp)
(`ggml-base.en.bin`, `ggml-medium.en.bin`, etc.) can be dropped into the
same folder and picked from the Settings dropdown.

## Settings

Persisted at:

```
~/Library/Application Support/com.nicksimi.dictation/settings.json
```

| Field | Notes |
|---|---|
| `hotkey` | Any string the [global-hotkey crate parses](https://docs.rs/global-hotkey): `F13`, `Ctrl+Shift+R`, `CommandOrControl+Shift+Space`. |
| `model_file` | Filename of a `.bin` in the models folder. |
| `input_device` | Exact device name, or `null` for system default. |

Hotkey changes apply live; model and input-device changes apply on the
next press.

## Roadmap

- [ ] Phase 5: signed `.app` + `.dmg` distribution
- [ ] In-app "Download model" button (currently you bring the model)
- [ ] App icon (currently Tauri default)
- [ ] Window position / size persistence
- [ ] Windows + Linux verification (Tauri makes the shell portable; the
      hotkey, mic, and text-injection paths need real testing)
- [ ] CI for build verification

## Acknowledgements

- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) — Georgi Gerganov
- [whisper-rs](https://github.com/tazz4843/whisper-rs) — tazz4843
- [Tauri](https://tauri.app)
- [cpal](https://github.com/RustAudio/cpal), [enigo](https://github.com/enigo-rs/enigo)

## License

MIT — see [LICENSE](LICENSE).
