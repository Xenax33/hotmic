// Hotmic frontend. Plain JS, talks to the Rust side through window.__TAURI__ (withGlobalTauri).

const tauri = window.__TAURI__;
const invoke = tauri.core.invoke;
const appWindow = tauri.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);
const COLORS = ["", "#5b3a8a", "#2f6f8a", "#3a7a5a", "#7a6a2a", "#8a3a3a", "#3a4a7a"];
const AUDIO_EXTS = ["mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff", "aif"];
const MONITOR_OFF = "__none__";

let config = null;
let devices = [];
let status = null;
let playing = new Map(); // clip id -> { start, dur }
let editing = null;
let filter = "all";
let ytInfo = null;
let adding = false;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------
function fmtDur(s) {
  if (!s || s < 0) return "";
  const m = Math.floor(s / 60);
  const r = Math.round(s - m * 60);
  return `${m}:${String(r).padStart(2, "0")}`;
}
function fmtTime(s) {
  if (s == null) return "";
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}:${r < 10 ? "0" : ""}${r.toFixed(r % 1 ? 1 : 0)}`;
}
// "1:23", "83", "1:02:03" -> seconds; empty -> null
function parseTime(s) {
  s = (s || "").trim();
  if (!s) return null;
  const parts = s.split(":").map((p) => parseFloat(p));
  if (parts.some((p) => Number.isNaN(p))) return null;
  return parts.reduce((acc, p) => acc * 60 + p, 0);
}
function db(gain) {
  if (gain <= 0.0001) return "off";
  const d = 20 * Math.log10(gain);
  return (d >= 0 ? "+" : "") + d.toFixed(1) + " dB";
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
function short(n) {
  return (n || "").replace(/\s*\(.*\)\s*$/, "");
}

// ---------------------------------------------------------------------------------------------
// Window chrome and views
// ---------------------------------------------------------------------------------------------
function bindWindow() {
  $("wc-min").addEventListener("click", () => appWindow.minimize());
  $("wc-max").addEventListener("click", () => appWindow.toggleMaximize());
  $("wc-close").addEventListener("click", () => appWindow.close());
}

function showView(name) {
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== "view-" + name));
  document.querySelectorAll(".rail button").forEach((b) => {
    if (b.dataset.view === name) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  });
  if (name === "hotkeys") renderHotkeys();
  if (name === "settings") loadDevices();
}

function bindViews() {
  document.querySelectorAll(".rail button").forEach((b) => b.addEventListener("click", () => showView(b.dataset.view)));
  $("btn-go-import").addEventListener("click", () => {
    showView("import");
    $("yt-url").focus();
  });
}

// ---------------------------------------------------------------------------------------------
// Hotkey capture
// ---------------------------------------------------------------------------------------------
const MOD_CODES = new Set(["ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"]);
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
  return h.replace(/Digit(\d)/, "$1").replace(/Key([A-Z])/, "$1").replace(/Numpad/, "Num ").replace(/Arrow/, "").replace(/\+/g, " ");
}
function bindKeyCapture(input, clearBtn, onChange) {
  input.addEventListener("keydown", (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return input.blur();
    const h = hotkeyFromEvent(e);
    if (!h) return;
    input.dataset.value = h;
    input.value = prettyHotkey(h);
    input.blur();
    onChange(h);
  });
  input.addEventListener("focus", () => (input.placeholder = "press keys now, Esc to cancel"));
  input.addEventListener("blur", () => (input.placeholder = "click, then press keys"));
  if (clearBtn)
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
  $("stop-kbd").textContent = prettyHotkey(config.stop_all_hotkey || "");
}

// ---------------------------------------------------------------------------------------------
// Devices and settings
// ---------------------------------------------------------------------------------------------
function fillSelect(sel, list, current, allowNone) {
  sel.innerHTML = "";
  if (allowNone) {
    const d = document.createElement("option");
    d.value = "";
    d.textContent = "(default output)";
    sel.appendChild(d);
    const o = document.createElement("option");
    o.value = MONITOR_OFF;
    o.textContent = "(none)";
    sel.appendChild(o);
    if (current === null || current === undefined) current = MONITOR_OFF;
  }
  for (const d of list) {
    const o = document.createElement("option");
    o.value = d.id;
    o.textContent = d.name + (d.is_default_input || d.is_default_output ? "  (default)" : "");
    sel.appendChild(o);
  }
  let match = list.find((d) => d.id === current);
  if (!match && current && current !== MONITOR_OFF) {
    const c = current.toLowerCase();
    match = list.find((d) => d.name.toLowerCase().includes(c));
  }
  if (!match && !current && !allowNone) match = list.find((d) => d.is_default_input || d.is_default_output) || list[0];
  if (!match && allowNone) {
    sel.value = current === MONITOR_OFF ? MONITOR_OFF : "";
    return;
  }
  sel.value = match ? match.id : "";
}

async function loadDevices() {
  try {
    devices = await invoke("list_devices");
  } catch (e) {
    showBanner("Could not list devices: " + e, "err");
    return;
  }
  fillSelect($("sel-mic"), devices.filter((d) => d.is_input), config.mic, false);
  fillSelect($("sel-cable"), devices.filter((d) => d.is_output), config.cable, false);
  fillSelect($("sel-monitor"), devices.filter((d) => d.is_output), config.monitor, true);
  $("chk-autostart").checked = !!config.autostart_engine;
  $("chk-block-voice").checked = !!config.block_voice;
  $("chk-tray").checked = !!config.close_to_tray;
  $("chk-minimized").checked = !!config.start_minimized;
  try {
    $("chk-login").checked = await invoke("autostart_enabled");
  } catch (_) {
    $("chk-login").disabled = true;
  }
}

function renderMixer() {
  $("rng-mic-gain").value = config.mic_gain;
  $("rng-clip-gain").value = config.clip_gain;
  $("rng-duck").value = config.duck;
  $("val-mic-gain").textContent = db(config.mic_gain);
  $("val-clip-gain").textContent = db(config.clip_gain);
  $("val-duck").textContent = config.duck >= 0.999 ? "no ducking" : db(config.duck);
  $("sw-monitor").setAttribute("aria-checked", String(!!config.monitor_enabled && config.monitor !== null));
  $("sw-replace").setAttribute("aria-checked", String(config.playback_mode !== "layer"));
  $("sw-normalize").setAttribute("aria-checked", String(!!config.normalize));
  setKeyInput($("key-stopall"), config.stop_all_hotkey);
  $("stop-kbd").textContent = prettyHotkey(config.stop_all_hotkey || "");
  $("yt-cookies").value = config.yt_cookies_browser || "";
}

function bindMixer() {
  const live = (id, key) => {
    $(id).addEventListener("input", (e) => {
      config[key] = parseFloat(e.target.value);
      renderMixer();
    });
    $(id).addEventListener("change", saveConfig);
  };
  live("rng-mic-gain", "mic_gain");
  live("rng-clip-gain", "clip_gain");
  live("rng-duck", "duck");

  $("sw-monitor").addEventListener("click", async () => {
    if (config.monitor === null) config.monitor = ""; // turning on with no device picks the default output
    config.monitor_enabled = !config.monitor_enabled;
    renderMixer();
    await saveConfig();
    refreshStatus();
  });
  $("sw-replace").addEventListener("click", async () => {
    config.playback_mode = config.playback_mode === "layer" ? "replace" : "layer";
    renderMixer();
    await saveConfig();
  });
  $("sw-normalize").addEventListener("click", async () => {
    config.normalize = !config.normalize;
    renderMixer();
    await saveConfig();
  });

  $("btn-apply-devices").addEventListener("click", async () => {
    config.mic = $("sel-mic").value;
    config.cable = $("sel-cable").value;
    config.monitor = $("sel-monitor").value === MONITOR_OFF ? null : $("sel-monitor").value;
    await saveConfig();
    await startEngine();
    renderMixer();
  });
  $("chk-autostart").addEventListener("change", async (e) => {
    config.autostart_engine = e.target.checked;
    await saveConfig();
  });
  $("chk-block-voice").addEventListener("change", async (e) => {
    config.block_voice = e.target.checked;
    await saveConfig();
  });
  $("chk-tray").addEventListener("change", async (e) => {
    config.close_to_tray = e.target.checked;
    await saveConfig();
  });
  $("chk-minimized").addEventListener("change", async (e) => {
    config.start_minimized = e.target.checked;
    await saveConfig();
  });
  $("chk-login").addEventListener("change", async (e) => {
    try {
      e.target.checked = await invoke("set_autostart", { enabled: e.target.checked });
    } catch (err) {
      showBanner("Could not change start-at-sign-in: " + err, "err");
      e.target.checked = !e.target.checked;
    }
  });
  $("btn-hide").addEventListener("click", () => invoke("hide_to_tray"));
  $("btn-quit").addEventListener("click", () => invoke("quit_app"));
  bindKeyCapture($("key-stopall"), $("key-stopall-clear"), async (h) => {
    config.stop_all_hotkey = h;
    await saveConfig();
    await applyHotkeys();
  });
  $("yt-cookies").addEventListener("change", async (e) => {
    config.yt_cookies_browser = e.target.value || null;
    await saveConfig();
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
  if (!status) return;
  const dot = $("engine-dot");
  const live = $("status-live");
  if (status.running) {
    dot.className = "dot on";
    live.className = "live on";
    live.textContent = "Engine running";
    $("engine-route").textContent = `${short(status.mic_name)} to ${short(status.cable_name)}` + (status.monitor_on ? `, monitor ${short(status.monitor_name)}` : "");
    $("status-rate").textContent = `${status.mic_rate ? status.mic_rate / 1000 : 48} kHz mic, 48 kHz out`;
    $("btn-engine").textContent = "Stop engine";
  } else {
    dot.className = status.error ? "dot err" : "dot";
    live.className = status.error ? "live err" : "live";
    live.textContent = status.error ? "Engine error" : "Engine stopped";
    $("engine-route").textContent = status.error ? "stopped" : "stopped";
    $("status-rate").textContent = "";
    $("btn-engine").textContent = "Start engine";
  }
  if (!status.vb_cable_installed) showBanner("VB-Cable is not installed. Hotmic needs it for the virtual microphone. Get it free from vb-audio.com/Cable, then restart Hotmic.", "err");
  else if (status.error) showBanner("Engine error: " + status.error, "err");
}

async function pollLevels() {
  try {
    const l = await invoke("levels");
    const mic = l ? Math.min(1, l.mic_peak) : 0;
    const out = l ? Math.min(1, l.out_peak) : 0;
    $("meter-mic").style.width = Math.round(Math.sqrt(mic) * 100) + "%";
    $("meter-out").style.width = Math.round(Math.sqrt(out) * 100) + "%";
    $("meter-mic-v").textContent = l ? (mic > 0.001 ? Math.round(20 * Math.log10(mic)) : "-inf") : "off";
    $("meter-out-v").textContent = l ? (out > 0.001 ? Math.round(20 * Math.log10(out)) : "-inf") : "off";
    $("status-drift").textContent = l ? `drift ${l.drift_ppm} ppm` : "";
  } catch (_) {
    /* engine gone */
  }
  const now = performance.now();
  for (const [id, p] of playing) {
    const el = document.querySelector(`.tile[data-id="${id}"]`);
    const frac = Math.min(1, (now - p.start) / (p.dur || 1));
    if (el) el.querySelector(".bar").style.width = frac * 100 + "%";
    if (frac >= 1) {
      playing.delete(id);
      if (el) {
        el.classList.remove("playing");
        el.querySelector(".bar").style.width = "0";
      }
      if (playing.size === 0) setNow(null);
    }
  }
}

function setNow(clip) {
  const n = $("now");
  if (!clip) {
    n.innerHTML = '<span class="l">Now playing</span><span class="n">Nothing</span><span class="d">Press a tile or its hotkey</span>';
    return;
  }
  n.innerHTML = '<span class="l">Now playing</span><span class="n"></span><span class="d"></span>';
  n.querySelector(".n").textContent = clip.name;
  n.querySelector(".d").textContent = fmtDur(clip.duration_secs) + (clip.hotkey ? "   " + prettyHotkey(clip.hotkey) : "");
}

// ---------------------------------------------------------------------------------------------
// Library grid
// ---------------------------------------------------------------------------------------------
function visibleClips() {
  const q = $("search").value.trim().toLowerCase();
  return config.clips.filter((c) => {
    if (q && !c.name.toLowerCase().includes(q)) return false;
    if (filter === "imported") return !!c.source_url;
    if (filter === "hotkeys") return !!c.hotkey;
    if (filter.startsWith("color:")) return (c.color || "") === filter.slice(6);
    return true;
  });
}

function renderChips() {
  const chips = $("chips");
  chips.innerHTML = "";
  const defs = [["all", "All"]];
  if (config.clips.some((c) => c.source_url)) defs.push(["imported", "Imported"]);
  if (config.clips.some((c) => c.hotkey)) defs.push(["hotkeys", "With hotkey"]);
  for (const col of COLORS.slice(1)) if (config.clips.some((c) => c.color === col)) defs.push(["color:" + col, ""]);
  for (const [key, label] of defs) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "chip";
    b.setAttribute("aria-pressed", String(filter === key));
    if (label) b.textContent = label;
    else {
      b.innerHTML = '<span style="display:inline-block;width:10px;height:10px;border-radius:50%;background:' + key.slice(6) + '"></span>';
      b.title = "Colour";
    }
    b.addEventListener("click", () => {
      filter = key;
      renderChips();
      renderGrid();
    });
    chips.appendChild(b);
  }
}

function renderGrid() {
  const grid = $("grid");
  grid.innerHTML = "";
  const clips = visibleClips();
  $("empty").classList.toggle("hidden", config.clips.length > 0);
  $("search").placeholder = `Search ${config.clips.length} clips`;
  for (const c of clips) {
    const t = document.createElement("button");
    t.type = "button";
    t.className = "tile" + (playing.has(c.id) ? " playing" : "");
    t.dataset.id = c.id;
    t.setAttribute("role", "listitem");
    if (c.color) t.style.setProperty("--c", c.color);
    t.innerHTML =
      '<span class="tint"></span><span class="stripe"></span>' +
      (c.thumbnail ? '<span class="thumb"></span>' : "") +
      '<span class="t"></span><span class="m"><span class="dur"></span><span class="k"></span></span>' +
      '<span class="edit" title="Edit"><svg viewBox="0 0 24 24"><circle cx="5" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="19" cy="12" r="1.5"/></svg></span>' +
      '<span class="bar"></span>';
    if (c.thumbnail) t.querySelector(".thumb").style.backgroundImage = `url("${c.thumbnail}")`;
    t.querySelector(".t").textContent = c.name;
    t.querySelector(".dur").textContent = fmtDur(c.duration_secs);
    const k = t.querySelector(".k");
    if (c.hotkey) k.textContent = prettyHotkey(c.hotkey);
    else k.remove();
    t.addEventListener("click", (e) => {
      if (e.target.closest(".edit")) {
        e.stopPropagation();
        openEditor(c);
        return;
      }
      playClip(c);
    });
    t.addEventListener("contextmenu", (e) => {
      e.preventDefault();
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
      el.querySelector(".bar").style.width = "0";
    }
  }
  playing.clear();
  setNow(null);
}
function markStarted(id, exclusive) {
  const c = config.clips.find((x) => x.id === id);
  if (!c) return;
  if (exclusive) clearPlayingTiles();
  playing.set(c.id, { start: performance.now(), dur: c.duration_secs * 1000 });
  const el = document.querySelector(`.tile[data-id="${c.id}"]`);
  if (el) el.classList.add("playing");
  setNow(c);
}
async function playClip(c) {
  try {
    await invoke("play", { id: c.id });
  } catch (e) {
    showBanner(String(e), "err");
  }
}

async function addPaths(paths) {
  if (!paths || paths.length === 0) return;
  if (adding) return showBanner("Still adding the previous batch, one moment.", "");
  adding = true;
  $("btn-add-files").disabled = true;
  $("btn-add-folder").disabled = true;
  showBanner("Adding clips...", "");
  try {
    config = await invoke("add_clips", { paths });
    showBanner("");
    renderChips();
    renderGrid();
  } catch (e) {
    showBanner("Could not add clips: " + e, "err");
  } finally {
    adding = false;
    $("btn-add-files").disabled = false;
    $("btn-add-folder").disabled = false;
  }
}

function bindLibrary() {
  $("btn-add-files").addEventListener("click", async () => {
    const sel = await tauri.dialog.open({ multiple: true, directory: false, title: "Add clips", filters: [{ name: "Audio", extensions: AUDIO_EXTS }] });
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
    } else await startEngine();
  });

  tauri.event.listen("tauri://drag-drop", (ev) => {
    const paths = ev.payload && ev.payload.paths;
    if (paths) addPaths(paths);
  });
  tauri.event.listen("library-progress", (ev) => {
    const p = ev.payload;
    if (!p || !adding) return;
    if (p.done >= p.total) showBanner(`Added ${p.total} clips, decoding in the background.`, "");
    else showBanner(`Adding clips: ${p.done + 1} of ${p.total}  (${p.name})`, "");
  });
  tauri.event.listen("clip-started", (ev) => {
    const p = ev.payload || {};
    markStarted(p.id, !!p.exclusive);
  });
  tauri.event.listen("clips-stopped", () => clearPlayingTiles());

  // Keyboard: digits 1-9 play the first nine visible tiles, Space stops all, Ctrl+K palette, Esc closes.
  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      openPalette();
      return;
    }
    if (e.key === "Escape") {
      closePalette();
      $("modal").classList.add("hidden");
      return;
    }
    if (e.target.matches("input, select, textarea")) return;
    if (!$("modal").classList.contains("hidden") || !$("palette").classList.contains("hidden")) return;
    if (e.key >= "1" && e.key <= "9") {
      const tiles = [...document.querySelectorAll("#view-board .tile")];
      const t = tiles[parseInt(e.key, 10) - 1];
      if (t) t.click();
    } else if (e.key === " ") {
      e.preventDefault();
      $("btn-stop").click();
    }
  });
}

// ---------------------------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------------------------
let palSel = 0;
function openPalette() {
  $("palette").classList.remove("hidden");
  const input = $("palette-input");
  input.value = "";
  palSel = 0;
  fillPalette("");
  input.focus();
}
function closePalette() {
  $("palette").classList.add("hidden");
}
function fillPalette(q) {
  const list = $("palette-list");
  list.innerHTML = "";
  const items = config.clips.filter((c) => !q || c.name.toLowerCase().includes(q)).slice(0, 8);
  items.forEach((c, i) => {
    const r = document.createElement("div");
    r.className = "row";
    if (i === palSel) r.setAttribute("aria-selected", "true");
    r.innerHTML = '<span class="n"></span><span class="k"></span>';
    r.querySelector(".n").textContent = c.name;
    r.querySelector(".k").textContent = c.hotkey ? prettyHotkey(c.hotkey) : fmtDur(c.duration_secs);
    r.addEventListener("click", () => {
      closePalette();
      playClip(c);
    });
    list.appendChild(r);
  });
}
function bindPalette() {
  const input = $("palette-input");
  input.addEventListener("input", () => {
    palSel = 0;
    fillPalette(input.value.trim().toLowerCase());
  });
  input.addEventListener("keydown", (e) => {
    const rows = $("palette-list").children;
    if (e.key === "ArrowDown") {
      palSel = Math.min(rows.length - 1, palSel + 1);
      fillPalette(input.value.trim().toLowerCase());
      e.preventDefault();
    } else if (e.key === "ArrowUp") {
      palSel = Math.max(0, palSel - 1);
      fillPalette(input.value.trim().toLowerCase());
      e.preventDefault();
    } else if (e.key === "Enter") {
      const r = rows[palSel];
      if (r) r.click();
    } else if (e.key === "Escape") closePalette();
  });
  $("palette").addEventListener("click", (e) => {
    if (e.target === $("palette")) closePalette();
  });
}

// ---------------------------------------------------------------------------------------------
// Hotkeys page
// ---------------------------------------------------------------------------------------------
function renderHotkeys() {
  const list = $("hklist");
  list.innerHTML = '<div class="hkrow hkhead"><span>Clip</span><span>Hotkey</span><span></span></div>';
  for (const c of config.clips) {
    const r = document.createElement("div");
    r.className = "hkrow";
    r.innerHTML = '<span class="n"></span><input type="text" class="keycap" readonly placeholder="click, then press keys" /><button type="button" class="btn small">Clear</button>';
    r.querySelector(".n").textContent = c.name;
    const input = r.querySelector("input");
    setKeyInput(input, c.hotkey);
    bindKeyCapture(input, r.querySelector("button"), async (h) => {
      config = await invoke("update_clip", { entry: { ...c, hotkey: h } });
      await applyHotkeys();
      renderChips();
      renderGrid();
    });
    list.appendChild(r);
  }
}

// ---------------------------------------------------------------------------------------------
// URL import
// ---------------------------------------------------------------------------------------------
async function refreshTools() {
  const t = await invoke("tools_status");
  const text = t.ready ? `yt-dlp ${t.ytdlp_version || ""} and ${t.deno_version || "deno"} ready.` : "";
  for (const id of ["yt-tools", "yt-tools-2"]) {
    const el = $(id);
    el.innerHTML = "";
    if (t.ready) {
      el.textContent = text + " ";
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
    } else {
      const missing = [!t.ytdlp_path && "yt-dlp (the downloader)", !t.deno_path && "Deno (the JavaScript runtime YouTube requires)"].filter(Boolean);
      el.textContent = `Needs ${missing.join(" and ")}. Fetched from their official GitHub releases and checked against the published SHA-256.`;
    }
  }
  $("yt-install").classList.toggle("hidden", t.ready);
  $("yt-form").classList.toggle("hidden", !t.ready);
}

function bindYt() {
  $("yt-install").addEventListener("click", async () => {
    $("yt-install").disabled = true;
    showBanner("Downloading yt-dlp and Deno, this takes a minute...", "");
    try {
      await invoke("tools_install");
      showBanner("");
      await refreshTools();
    } catch (e) {
      showBanner("Could not install: " + e, "err");
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
      $("yt-meta").textContent = [ytInfo.uploader, ytInfo.duration_secs != null ? fmtTime(ytInfo.duration_secs) : null].filter(Boolean).join("   ");
      $("yt-thumb").src = ytInfo.thumbnail || "";
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
      $("yt-fetch").textContent = "Fetch";
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
      const name = $("yt-name").value;
      config = await invoke("yt_import", { url, name, start, end });
      renderChips();
      renderGrid();
      showBanner(`Imported "${name || (ytInfo && ytInfo.title) || "clip"}".`, "");
      $("yt-url").value = "";
      $("yt-info").classList.add("hidden");
      ytInfo = null;
      showView("board");
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
  $("edit-gain-val").textContent = db(c.gain);
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
    s.style.background = col || "var(--layer-3)";
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
    $("edit-gain-val").textContent = db(editing.gain);
  });
  bindKeyCapture($("edit-hotkey"), $("edit-hotkey-clear"), (h) => (editing.hotkey = h));
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
    renderChips();
    renderGrid();
    await applyHotkeys();
  });
  $("edit-remove").addEventListener("click", async () => {
    config = await invoke("remove_clip", { id: editing.id });
    $("modal").classList.add("hidden");
    renderChips();
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
  bindWindow();
  config = await invoke("get_config");
  renderMixer();
  bindViews();
  bindMixer();
  bindLibrary();
  bindPalette();
  bindEditor();
  bindYt();
  renderChips();
  renderGrid();
  setNow(null);
  await refreshStatus();
  if (!status.running && config.autostart_engine && status.vb_cable_installed && !status.error) await startEngine();
  await applyHotkeys();
  refreshTools().catch(() => {});
  setInterval(pollLevels, 100);
  setInterval(refreshStatus, 3000);
}

main().catch((e) => showBanner("Startup failed: " + e, "err"));
