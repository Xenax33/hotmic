// Hotmic frontend. Plain JS, talks to the Rust side through window.__TAURI__ (withGlobalTauri).

const tauri = window.__TAURI__;
const invoke = tauri.core.invoke;

const $ = (id) => document.getElementById(id);
const COLORS = ["", "#3b2f4a", "#2f3f4a", "#2f4a3a", "#4a3f2f", "#4a2f2f", "#333a4a"];
const AUDIO_EXTS = ["mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff", "aif"];

let config = null;
let devices = [];
let status = null;
let playing = new Map(); // clip id -> { until: timestamp }
let editing = null;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------
function fmtDur(s) {
  if (!s || s < 0) return "";
  const m = Math.floor(s / 60);
  const r = Math.round(s - m * 60);
  return m ? `${m}:${String(r).padStart(2, "0")}` : `${s.toFixed(1)}s`;
}

function showBanner(text, kind) {
  const b = $("banner");
  if (!text) {
    b.classList.add("hidden");
    return;
  }
  b.textContent = text;
  b.className = "banner" + (kind === "err" ? " err" : "");
}

async function saveConfig() {
  config = await invoke("save_config", { config });
}

// ---------------------------------------------------------------------------------------------
// Devices and settings
// ---------------------------------------------------------------------------------------------
const MONITOR_OFF = "__none__";

function fillSelect(sel, list, current, allowNone) {
  sel.innerHTML = "";
  if (allowNone) {
    // monitor select: "" = system default output, MONITOR_OFF = no monitor at all
    const d = document.createElement("option");
    d.value = "";
    d.textContent = "(default output)";
    sel.appendChild(d);
    const o = document.createElement("option");
    o.value = MONITOR_OFF;
    o.textContent = "(none)";
    sel.appendChild(o);
    if (current === null || current === undefined) {
      current = MONITOR_OFF;
    }
  }
  for (const d of list) {
    const o = document.createElement("option");
    o.value = d.id;
    o.textContent = d.name + (d.is_default_input || d.is_default_output ? "  (default)" : "");
    sel.appendChild(o);
  }
  // current may be an id, a name substring, or empty (= system default)
  let match = list.find((d) => d.id === current);
  if (!match && current) {
    const c = current.toLowerCase();
    match = list.find((d) => d.name.toLowerCase().includes(c));
  }
  if (!match && !current && !allowNone) {
    match = list.find((d) => d.is_default_input || d.is_default_output) || list[0];
  }
  if (!match && allowNone) {
    sel.value = current === MONITOR_OFF ? MONITOR_OFF : "";
    return;
  }
  sel.value = match ? match.id : "";
}

async function loadDevices() {
  devices = await invoke("list_devices");
  const ins = devices.filter((d) => d.is_input);
  const outs = devices.filter((d) => d.is_output);
  fillSelect($("sel-mic"), ins, config.mic, false);
  fillSelect($("sel-cable"), outs, config.cable, false);
  fillSelect($("sel-monitor"), outs, config.monitor, true);
  $("chk-monitor").checked = !!config.monitor_enabled;
}

// ---------------------------------------------------------------------------------------------
// Hotkeys
// ---------------------------------------------------------------------------------------------
const MOD_CODES = new Set(["ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"]);

// Turn a keydown into the string the Rust side parses, e.g. "Ctrl+Alt+Digit1".
function hotkeyFromEvent(e) {
  if (MOD_CODES.has(e.code)) return null;
  const parts = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.shiftKey) parts.push("Shift");
  if (e.altKey) parts.push("Alt");
  if (e.metaKey) parts.push("Super");
  parts.push(e.code);
  return parts.join("+");
}

function prettyHotkey(h) {
  if (!h) return "";
  return h
    .replace(/Digit(\d)/, "$1")
    .replace(/Key([A-Z])/, "$1")
    .replace(/Numpad/, "Num ")
    .replace(/Arrow/, "");
}

// Makes a readonly input capture a hotkey. onChange(stringOrNull) is called on every change.
function bindKeyCapture(input, clearBtn, onChange) {
  input.addEventListener("keydown", (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      input.blur();
      return;
    }
    const h = hotkeyFromEvent(e);
    if (!h) return;
    input.dataset.value = h;
    input.value = prettyHotkey(h);
    input.blur();
    onChange(h);
  });
  input.addEventListener("focus", () => {
    input.placeholder = "press keys now (Esc to cancel)";
  });
  input.addEventListener("blur", () => {
    input.placeholder = "click, then press keys";
  });
  clearBtn.addEventListener("click", () => {
    input.dataset.value = "";
    input.value = "";
    onChange(null);
  });
}

function setKeyInput(input, value) {
  input.dataset.value = value || "";
  input.value = prettyHotkey(value || "");
}

async function applyHotkeys() {
  const errors = await invoke("apply_hotkeys");
  if (errors.length) showBanner("Hotkey problems: " + errors.join(" | "), "err");
  else if ($("banner").textContent.startsWith("Hotkey problems")) showBanner("");
}

function renderSettings() {
  $("mode-replace").checked = config.playback_mode !== "layer";
  $("mode-layer").checked = config.playback_mode === "layer";
  setKeyInput($("key-stopall"), config.stop_all_hotkey);
  $("rng-mic-gain").value = config.mic_gain;
  $("rng-clip-gain").value = config.clip_gain;
  $("rng-duck").value = config.duck;
  $("chk-normalize").checked = !!config.normalize;
  $("val-mic-gain").textContent = config.mic_gain.toFixed(2);
  $("val-clip-gain").textContent = config.clip_gain.toFixed(2);
  $("val-duck").textContent = config.duck >= 0.999 ? "no ducking" : Math.round(config.duck * 100) + "%";
}

function bindSettings() {
  const live = (id, key, fmt) => {
    $(id).addEventListener("input", (e) => {
      config[key] = parseFloat(e.target.value);
      renderSettings();
    });
    $(id).addEventListener("change", saveConfig);
    void fmt;
  };
  live("rng-mic-gain", "mic_gain");
  live("rng-clip-gain", "clip_gain");
  live("rng-duck", "duck");
  $("chk-normalize").addEventListener("change", async (e) => {
    config.normalize = e.target.checked;
    await saveConfig();
  });
  for (const id of ["mode-replace", "mode-layer"]) {
    $(id).addEventListener("change", async (e) => {
      if (e.target.checked) {
        config.playback_mode = e.target.value;
        await saveConfig();
      }
    });
  }
  bindKeyCapture($("key-stopall"), $("key-stopall-clear"), async (h) => {
    config.stop_all_hotkey = h;
    await saveConfig();
    await applyHotkeys();
  });
  $("chk-monitor").addEventListener("change", async (e) => {
    config.monitor_enabled = e.target.checked;
    await saveConfig();
    refreshStatus();
  });
  $("btn-apply-devices").addEventListener("click", async () => {
    config.mic = $("sel-mic").value;
    config.cable = $("sel-cable").value;
    config.monitor = $("sel-monitor").value === MONITOR_OFF ? null : $("sel-monitor").value;
    config.monitor_enabled = $("chk-monitor").checked;
    await saveConfig();
    await startEngine();
  });
}

// ---------------------------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------------------------
async function startEngine() {
  $("btn-engine").disabled = true;
  try {
    status = await invoke("engine_start");
    showBanner("");
  } catch (e) {
    showBanner("Engine failed to start: " + e, "err");
    status = await invoke("engine_status");
  }
  $("btn-engine").disabled = false;
  renderStatus();
}

async function refreshStatus() {
  status = await invoke("engine_status");
  renderStatus();
}

function renderStatus() {
  const dot = $("engine-dot");
  const txt = $("engine-status");
  if (!status) return;
  if (status.running) {
    dot.className = "dot on";
    const short = (n) => (n || "").replace(/\s*\(.*\)\s*$/, "");
    txt.textContent = `${short(status.mic_name)} to ${short(status.cable_name)}` + (status.monitor_on ? ", monitor on" : "");
    txt.title = `${status.mic_name} to ${status.cable_name}`;
    $("btn-engine").textContent = "Stop engine";
  } else {
    dot.className = status.error ? "dot err" : "dot";
    txt.textContent = status.error ? "stopped (error)" : "stopped";
    $("btn-engine").textContent = "Start engine";
  }
  if (!status.vb_cable_installed) {
    showBanner("VB-Cable is not installed. Hotmic needs it to create the virtual microphone. Get it from vb-audio.com/Cable (free).", "err");
  } else if (status.error) {
    showBanner("Engine error: " + status.error, "err");
  }
}

async function pollLevels() {
  try {
    const l = await invoke("levels");
    const mic = l ? Math.min(1, l.mic_peak) : 0;
    const out = l ? Math.min(1, l.out_peak) : 0;
    $("meter-mic").style.width = Math.round(Math.sqrt(mic) * 100) + "%";
    $("meter-out").style.width = Math.round(Math.sqrt(out) * 100) + "%";
  } catch (_) {
    /* engine gone */
  }
  // progress bars
  const now = performance.now();
  for (const [id, p] of playing) {
    const el = document.querySelector(`.tile[data-id="${id}"]`);
    if (!el) continue;
    const frac = Math.min(1, (now - p.start) / (p.dur || 1));
    el.querySelector(".progress").style.width = frac * 100 + "%";
    if (frac >= 1) {
      playing.delete(id);
      el.classList.remove("playing");
      el.querySelector(".progress").style.width = "0";
    }
  }
}

// ---------------------------------------------------------------------------------------------
// Library grid
// ---------------------------------------------------------------------------------------------
function renderGrid() {
  const q = $("search").value.trim().toLowerCase();
  const grid = $("grid");
  grid.innerHTML = "";
  const clips = config.clips.filter((c) => !q || c.name.toLowerCase().includes(q));
  $("clip-count").textContent = `${clips.length} of ${config.clips.length} clips`;
  $("empty").classList.toggle("hidden", config.clips.length > 0);
  for (const c of clips) {
    const t = document.createElement("div");
    t.className = "tile" + (playing.has(c.id) ? " playing" : "");
    t.dataset.id = c.id;
    if (c.color) t.style.setProperty("--tile", c.color);
    t.innerHTML = `
      <div class="title"></div>
      <div class="meta"><span class="dur"></span><span class="key"></span></div>
      <button class="edit" title="Edit">...</button>
      <div class="progress"></div>`;
    t.querySelector(".title").textContent = c.name;
    t.querySelector(".dur").textContent = fmtDur(c.duration_secs);
    const key = t.querySelector(".key");
    if (c.hotkey) key.textContent = prettyHotkey(c.hotkey);
    else key.remove();
    t.addEventListener("click", (e) => {
      if (e.target.closest(".edit")) return;
      playClip(c);
    });
    t.querySelector(".edit").addEventListener("click", (e) => {
      e.stopPropagation();
      openEditor(c);
    });
    grid.appendChild(t);
  }
}

function clearPlayingTiles() {
  for (const id of playing.keys()) {
    const el = document.querySelector(`.tile[data-id="${id}"]`);
    if (el) {
      el.classList.remove("playing");
      el.querySelector(".progress").style.width = "0";
    }
  }
  playing.clear();
}

// Marks a tile as playing. Called for clicks and for hotkeys (via the clip-started event).
function markStarted(id, exclusive) {
  const c = config.clips.find((x) => x.id === id);
  if (!c) return;
  if (exclusive) clearPlayingTiles();
  playing.set(c.id, { start: performance.now(), dur: c.duration_secs * 1000 });
  const el = document.querySelector(`.tile[data-id="${c.id}"]`);
  if (el) el.classList.add("playing");
}

async function playClip(c) {
  try {
    await invoke("play", { id: c.id });
  } catch (e) {
    showBanner(String(e), "err");
  }
}

let adding = false;

async function addPaths(paths) {
  if (!paths || paths.length === 0) return;
  if (adding) {
    showBanner("Still adding the previous batch, one moment.", "");
    return;
  }
  adding = true;
  $("btn-add-files").disabled = true;
  $("btn-add-folder").disabled = true;
  showBanner("Adding clips...", "");
  try {
    config = await invoke("add_clips", { paths });
    showBanner("");
    renderGrid();
  } catch (e) {
    showBanner("Could not add clips: " + e, "err");
  } finally {
    adding = false;
    $("btn-add-files").disabled = false;
    $("btn-add-folder").disabled = false;
  }
}

function bindProgress() {
  tauri.event.listen("library-progress", (ev) => {
    const p = ev.payload;
    if (!p || !adding) return;
    if (p.done >= p.total) showBanner(`Added ${p.total} clips, decoding in the background.`, "");
    else showBanner(`Adding clips: ${p.done + 1} of ${p.total}  (${p.name})`, "");
  });
}

function bindLibrary() {
  $("btn-add-files").addEventListener("click", async () => {
    const sel = await tauri.dialog.open({
      multiple: true,
      directory: false,
      title: "Add clips",
      filters: [{ name: "Audio", extensions: AUDIO_EXTS }],
    });
    if (sel) await addPaths(Array.isArray(sel) ? sel : [sel]);
  });
  $("btn-add-folder").addEventListener("click", async () => {
    const sel = await tauri.dialog.open({ directory: true, multiple: false, title: "Add a folder of clips" });
    if (sel) await addPaths([sel]);
  });
  $("search").addEventListener("input", renderGrid);
  $("btn-stop").addEventListener("click", async () => {
    await invoke("stop_all");
    clearPlayingTiles();
  });
  tauri.event.listen("clip-started", (ev) => {
    const p = ev.payload || {};
    markStarted(p.id, !!p.exclusive);
  });
  tauri.event.listen("clips-stopped", () => clearPlayingTiles());
  $("btn-tone").addEventListener("click", async () => {
    try {
      await invoke("play_test_tone");
    } catch (e) {
      showBanner(String(e), "err");
    }
  });
  $("btn-engine").addEventListener("click", async () => {
    if (status && status.running) {
      status = await invoke("engine_stop");
      renderStatus();
    } else {
      await startEngine();
    }
  });

  // Drag and drop from Explorer (Tauri delivers real paths through this event).
  tauri.event.listen("tauri://drag-drop", (ev) => {
    const paths = ev.payload && ev.payload.paths;
    if (paths) addPaths(paths);
  });

  // Keyboard: digits 1-9 play the first nine visible tiles, Space stops all.
  document.addEventListener("keydown", (e) => {
    if (e.target.matches("input, select, textarea")) return;
    if (!$("modal").classList.contains("hidden")) return;
    if (e.key >= "1" && e.key <= "9") {
      const tiles = [...document.querySelectorAll(".tile")];
      const t = tiles[parseInt(e.key, 10) - 1];
      if (t) t.click();
    } else if (e.key === " ") {
      e.preventDefault();
      $("btn-stop").click();
    }
  });
}

// ---------------------------------------------------------------------------------------------
// Time helpers and URL import
// ---------------------------------------------------------------------------------------------
// "1:23", "83", "1:02:03" -> seconds; empty -> null
function parseTime(s) {
  s = (s || "").trim();
  if (!s) return null;
  const parts = s.split(":").map((p) => parseFloat(p));
  if (parts.some((p) => Number.isNaN(p))) return null;
  return parts.reduce((acc, p) => acc * 60 + p, 0);
}

function fmtTime(s) {
  if (s == null) return "";
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}:${r < 10 ? "0" : ""}${r.toFixed(r % 1 ? 1 : 0)}`;
}

let ytInfo = null;

async function refreshTools() {
  const t = await invoke("tools_status");
  const el = $("yt-tools");
  if (t.ready) {
    el.innerHTML = "";
    el.textContent = `yt-dlp ${t.ytdlp_version || ""} and ${t.deno_version || "deno"} ready. `;
    const upd = document.createElement("a");
    upd.href = "#";
    upd.textContent = "Check for update";
    upd.addEventListener("click", async (e) => {
      e.preventDefault();
      showBanner("Updating yt-dlp...", "");
      try {
        const out = await invoke("tools_update");
        showBanner(out.split("\n").pop(), "");
        refreshTools();
      } catch (err) {
        showBanner("Update failed: " + err, "err");
      }
    });
    el.appendChild(upd);
    $("yt-install").classList.add("hidden");
    $("yt-form").classList.remove("hidden");
  } else {
    const missing = [!t.ytdlp_path && "yt-dlp (downloader)", !t.deno_path && "Deno (JavaScript runtime YouTube requires)"].filter(Boolean);
    el.textContent = `Needs ${missing.join(" and ")}. Fetched from their official GitHub releases and checked against the published SHA-256.`;
    $("yt-install").classList.remove("hidden");
    $("yt-form").classList.add("hidden");
  }
}

function bindYt() {
  $("yt-cookies").value = config.yt_cookies_browser || "";
  $("yt-cookies").addEventListener("change", async (e) => {
    config.yt_cookies_browser = e.target.value || null;
    await saveConfig();
  });
  $("yt-install").addEventListener("click", async () => {
    $("yt-install").disabled = true;
    showBanner("Downloading yt-dlp and Deno, this takes a minute...", "");
    try {
      await invoke("tools_install");
      showBanner("");
      await refreshTools();
    } catch (e) {
      showBanner("Could not install yt-dlp: " + e, "err");
    } finally {
      $("yt-install").disabled = false;
    }
  });

  $("yt-fetch").addEventListener("click", async () => {
    const url = $("yt-url").value.trim();
    if (!url) return;
    $("yt-fetch").disabled = true;
    $("yt-fetch").textContent = "Fetching...";
    try {
      ytInfo = await invoke("yt_probe", { url });
      $("yt-title").textContent = ytInfo.title;
      $("yt-meta").textContent = [ytInfo.uploader, ytInfo.duration_secs != null ? fmtTime(ytInfo.duration_secs) : null].filter(Boolean).join("  |  ");
      $("yt-name").value = ytInfo.title;
      $("yt-start").value = "";
      $("yt-end").value = "";
      $("yt-info").classList.remove("hidden");
      showBanner("");
    } catch (e) {
      ytInfo = null;
      $("yt-info").classList.add("hidden");
      showBanner("Could not read that URL: " + e, "err");
    } finally {
      $("yt-fetch").disabled = false;
      $("yt-fetch").textContent = "Fetch info";
    }
  });
  $("yt-url").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("yt-fetch").click();
  });

  $("yt-import").addEventListener("click", async () => {
    const url = $("yt-url").value.trim();
    if (!url) return;
    const start = parseTime($("yt-start").value);
    const end = parseTime($("yt-end").value);
    if ($("yt-start").value.trim() && start == null) return showBanner("Start time is not valid", "err");
    if ($("yt-end").value.trim() && end == null) return showBanner("End time is not valid", "err");
    $("yt-import").disabled = true;
    showBanner("Starting download...", "");
    try {
      config = await invoke("yt_import", { url, name: $("yt-name").value, start, end });
      renderGrid();
      showBanner(`Imported "${$("yt-name").value || (ytInfo && ytInfo.title) || "clip"}".`, "");
      $("yt-url").value = "";
      $("yt-info").classList.add("hidden");
      ytInfo = null;
    } catch (e) {
      showBanner("Import failed: " + e, "err");
    } finally {
      $("yt-import").disabled = false;
    }
  });

  tauri.event.listen("yt-progress", (ev) => {
    const p = ev.payload || {};
    if (p.percent >= 100) showBanner(p.text, "");
    else showBanner(`${p.text}  ${p.percent ? p.percent.toFixed(0) + "%" : ""}`, "");
  });
}

// ---------------------------------------------------------------------------------------------
// Clip editor
// ---------------------------------------------------------------------------------------------
function openEditor(c) {
  editing = { ...c };
  $("edit-name").value = c.name;
  $("edit-gain").value = c.gain;
  $("edit-gain-val").textContent = c.gain.toFixed(2);
  setKeyInput($("edit-hotkey"), c.hotkey);
  $("edit-start").value = c.trim_start ? fmtTime(c.trim_start) : "";
  $("edit-end").value = c.trim_end != null ? fmtTime(c.trim_end) : "";
  $("edit-full").textContent = c.full_duration_secs ? `Full file: ${fmtTime(c.full_duration_secs)}` : "";
  $("edit-path").textContent = c.source_url ? `${c.path}\n${c.source_url}` : c.path;
  const sw = $("edit-colors");
  sw.innerHTML = "";
  for (const col of COLORS) {
    const s = document.createElement("span");
    s.className = "swatch" + ((c.color || "") === col ? " sel" : "");
    s.style.background = col || "var(--panel-2)";
    s.addEventListener("click", () => {
      editing.color = col || null;
      [...sw.children].forEach((x) => x.classList.remove("sel"));
      s.classList.add("sel");
    });
    sw.appendChild(s);
  }
  $("modal").classList.remove("hidden");
  $("edit-name").focus();
}

function bindEditor() {
  $("edit-gain").addEventListener("input", (e) => {
    editing.gain = parseFloat(e.target.value);
    $("edit-gain-val").textContent = editing.gain.toFixed(2);
  });
  bindKeyCapture($("edit-hotkey"), $("edit-hotkey-clear"), (h) => {
    editing.hotkey = h;
  });
  $("edit-cancel").addEventListener("click", () => $("modal").classList.add("hidden"));
  $("edit-save").addEventListener("click", async () => {
    editing.name = $("edit-name").value.trim() || editing.name;
    const s = parseTime($("edit-start").value);
    const e = parseTime($("edit-end").value);
    if ($("edit-start").value.trim() && s == null) return showBanner("Start time is not valid", "err");
    if ($("edit-end").value.trim() && e == null) return showBanner("End time is not valid", "err");
    editing.trim_start = s || 0;
    editing.trim_end = e;
    config = await invoke("update_clip", { entry: editing });
    $("modal").classList.add("hidden");
    renderGrid();
    await applyHotkeys();
  });
  $("edit-remove").addEventListener("click", async () => {
    config = await invoke("remove_clip", { id: editing.id });
    $("modal").classList.add("hidden");
    renderGrid();
    await applyHotkeys();
  });
  $("modal").addEventListener("click", (e) => {
    if (e.target === $("modal")) $("modal").classList.add("hidden");
  });
}

// ---------------------------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------------------------
async function main() {
  config = await invoke("get_config");
  renderSettings();
  bindSettings();
  bindLibrary();
  bindProgress();
  bindEditor();
  bindYt();
  refreshTools().catch(() => {});
  await loadDevices();
  renderGrid();
  await refreshStatus();
  if (!status.running && config.autostart_engine && status.vb_cable_installed && !status.error) {
    await startEngine();
  }
  await applyHotkeys();
  setInterval(pollLevels, 100);
  setInterval(refreshStatus, 3000);
}

main().catch((e) => showBanner("Startup failed: " + e, "err"));
