import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Phase 1 UI: prove that hotkey press/release events make it from the OS,
// through the Rust core, into the WebView reliably. No audio / transcription
// yet — those land in Phase 2.

type ListenerState = "stopped" | "ready" | "recording";

const statusPill = document.getElementById("status-pill")!;
const statusText = document.getElementById("status-text")!;
const toggleBtn = document.getElementById("toggle-btn") as HTMLButtonElement;
const hotkeyLabel = document.getElementById("hotkey-label")!;
const logList = document.getElementById("log-list") as HTMLOListElement;

let state: ListenerState = "stopped";

function setState(next: ListenerState): void {
  state = next;
  statusPill.dataset.state = next;
  statusText.textContent = labelFor(next);
  toggleBtn.textContent = next === "stopped" ? "Start dictation" : "Stop dictation";
  // Visual: blue when the action is "Start" (positive), red when it's "Stop"
  // (destructive — disarming the listener / cutting recording).
  toggleBtn.classList.toggle("is-stop", next !== "stopped");
}

function labelFor(s: ListenerState): string {
  switch (s) {
    case "stopped": return "Stopped";
    case "ready": return "Ready — hold the hotkey";
    case "recording": return "Recording…";
  }
}

function log(line: string): void {
  const li = document.createElement("li");
  const time = new Date().toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
  const timeEl = document.createElement("span");
  timeEl.className = "log-time";
  timeEl.textContent = time;
  const msgEl = document.createElement("span");
  msgEl.className = "log-msg";
  msgEl.textContent = line;
  li.append(timeEl, msgEl);
  logList.prepend(li);
  while (logList.children.length > 100) {
    logList.lastElementChild?.remove();
  }
}

async function toggle(): Promise<void> {
  toggleBtn.disabled = true;
  try {
    if (state === "stopped") {
      await invoke("start_listening");
      // Set state directly here rather than waiting for the listener:state
      // event — the user can (and did) click Start during init() before
      // the event listener was registered, which silently dropped the
      // "ready" event and left the UI stuck on "Stopped".
      setState("ready");
      log("Hotkey armed.");
    } else {
      await invoke("stop_listening");
      setState("stopped");
      log("Hotkey disarmed.");
    }
  } catch (err) {
    log(`error: ${String(err)}`);
  } finally {
    toggleBtn.disabled = false;
  }
}

type Settings = {
  hotkey: string;
  model_file: string;
  input_device: string | null;
};

async function refreshModelBanner(): Promise<void> {
  const info = await invoke<{
    path: string;
    present: boolean;
    url: string;
    filename: string;
  }>("model_info");
  const banner = document.getElementById("model-banner") as HTMLElement;
  const pathEl = document.getElementById("model-path") as HTMLElement;
  const urlEl = document.getElementById("model-url") as HTMLAnchorElement;
  pathEl.textContent = info.path;
  urlEl.href = info.url;
  banner.hidden = info.present;
}

async function refreshSettings(): Promise<void> {
  const [settings, devices, models] = await Promise.all([
    invoke<Settings>("get_settings"),
    invoke<string[]>("list_input_devices"),
    invoke<string[]>("list_models"),
  ]);

  const hotkeyInput = document.getElementById("setting-hotkey") as HTMLInputElement;
  hotkeyInput.value = settings.hotkey;
  hotkeyLabel.textContent = settings.hotkey;

  const deviceSelect = document.getElementById("setting-device") as HTMLSelectElement;
  deviceSelect.innerHTML = "";
  const defaultOption = document.createElement("option");
  defaultOption.value = "";
  defaultOption.textContent = "System default";
  deviceSelect.appendChild(defaultOption);
  for (const name of devices) {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    deviceSelect.appendChild(opt);
  }
  deviceSelect.value = settings.input_device ?? "";

  const modelSelect = document.getElementById("setting-model") as HTMLSelectElement;
  modelSelect.innerHTML = "";
  // Always include the currently-selected model even if it isn't in the
  // models folder yet — otherwise saving would silently switch it.
  const seen = new Set<string>();
  const ordered = [settings.model_file, ...models].filter((n) => {
    if (seen.has(n)) return false;
    seen.add(n);
    return true;
  });
  for (const name of ordered) {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    modelSelect.appendChild(opt);
  }
  modelSelect.value = settings.model_file;
}

async function saveSettings(): Promise<void> {
  const status = document.getElementById("settings-status")!;
  const saveBtn = document.getElementById("settings-save") as HTMLButtonElement;
  const next: Settings = {
    hotkey: (document.getElementById("setting-hotkey") as HTMLInputElement).value.trim(),
    model_file: (document.getElementById("setting-model") as HTMLSelectElement).value,
    input_device: (document.getElementById("setting-device") as HTMLSelectElement).value || null,
  };
  saveBtn.disabled = true;
  status.textContent = "Saving…";
  try {
    await invoke("update_settings", { newSettings: next });
    status.textContent = "Saved.";
    log(`settings updated: hotkey=${next.hotkey}, model=${next.model_file}`);
    await refreshModelBanner();
    hotkeyLabel.textContent = next.hotkey;
  } catch (err) {
    status.textContent = `Error: ${String(err)}`;
    log(`settings error: ${String(err)}`);
  } finally {
    saveBtn.disabled = false;
    setTimeout(() => {
      if (status.textContent === "Saved.") status.textContent = "";
    }, 2500);
  }
}

async function init(): Promise<void> {
  // Register all Tauri event listeners FIRST so we never miss an event that
  // Rust emits while the UI is still booting (race that bit us on the
  // start-dictation click during init).
  await listen<unknown>("hotkey:pressed", () => {
    if (state === "ready") {
      setState("recording");
      log("press");
    }
  });
  await listen<unknown>("hotkey:released", () => {
    if (state === "recording") {
      setState("ready");
      log("release");
    }
  });
  // Backstop: Rust pushes the canonical listener state on start/stop so
  // we recover even if toggle()'s direct setState ever drifts.
  await listen<string>("listener:state", (event) => {
    if (event.payload === "ready" || event.payload === "stopped") {
      setState(event.payload);
    }
  });
  await listen<{ samples: number; sample_rate: number; duration_ms: number }>(
    "capture:done",
    (event) => {
      const { samples, sample_rate, duration_ms } = event.payload;
      log(`captured ${samples} samples @ ${sample_rate} Hz (${duration_ms} ms)`);
    },
  );
  await listen<string>("capture:error", (event) => {
    log(`capture error: ${event.payload}`);
  });
  await listen<unknown>("transcribe:start", () => {
    log("transcribing…");
  });
  await listen<string>("transcript", (event) => {
    const text = (event.payload ?? "").trim();
    log(`📝 ${text || "(no speech detected)"}`);
  });
  await listen<string>("transcribe:error", (event) => {
    log(`transcribe error: ${event.payload}`);
  });
  await listen<string>("inject:error", (event) => {
    log(
      `inject error: ${event.payload} — on macOS grant Accessibility to this binary in System Settings.`,
    );
  });
  await listen<string>("model:state", async () => {
    await refreshModelBanner();
  });

  // Now populate the UI and wire user controls.
  hotkeyLabel.textContent = await invoke<string>("hotkey_label");
  toggleBtn.addEventListener("click", toggle);
  document.getElementById("settings-save")?.addEventListener("click", saveSettings);
  await refreshSettings();
  await refreshModelBanner();

  // Sync UI state with the Rust backend — Rust might already be armed if
  // the frontend reloaded (Vite HMR, manual refresh) while the listener
  // stayed live.
  const backendState = await invoke<string>("listening_state");
  if (backendState === "ready") setState("ready");
}

init().catch((err) => {
  log(`init error: ${String(err)}`);
});
