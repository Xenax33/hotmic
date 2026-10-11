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
function fillSelect(sel, list, current, allowNone) {
  sel.innerHTML = "";
  if (allowNone) {
    const o = document.createElement("option");
    o.value = "";
    o.textContent = "(none)";
    sel.appendChild(o);
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
  sel.value = match ? match.id : "";
}

async function loadDevices() {
  devices = await invoke("list_devices");
  const ins = devices.filter((d) => d.is_input);
  const outs = devices.filter((d) => d.is_output);
  fillSelect($("sel-mic"), ins, config.mic, false);
  fillSelect($("sel-cable"), outs, config.cable, false);
  fillSelect($("sel-monitor"), outs, config.monitor || "", true);
  $("chk-monitor").checked = !!config.monitor_enabled;
}

function renderSettings() {
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
  $("chk-monitor").addEventListener("change", async (e) => {
    config.monitor_enabled = e.target.checked;
    await saveConfig();
    refreshStatus();
  });
  $("btn-apply-devices").addEventListener("click", async () => {
    config.mic = $("sel-mic").value;
    config.cable = $("sel-cable").value;
    config.monitor = $("sel-monitor").value || null;
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
    if (c.hotkey) key.textContent = c.hotkey;
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

async function playClip(c) {
  try {
    await invoke("play", { id: c.id });
    playing.set(c.id, { start: performance.now(), dur: c.duration_secs * 1000 });
    const el = document.querySelector(`.tile[data-id="${c.id}"]`);
    if (el) el.classList.add("playing");
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
    for (const id of playing.keys()) {
      const el = document.querySelector(`.tile[data-id="${id}"]`);
      if (el) {
        el.classList.remove("playing");
        el.querySelector(".progress").style.width = "0";
      }
    }
    playing.clear();
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
// Clip editor
// ---------------------------------------------------------------------------------------------
function openEditor(c) {
  editing = { ...c };
  $("edit-name").value = c.name;
  $("edit-gain").value = c.gain;
  $("edit-gain-val").textContent = c.gain.toFixed(2);
  $("edit-path").textContent = c.path;
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
  $("edit-cancel").addEventListener("click", () => $("modal").classList.add("hidden"));
  $("edit-save").addEventListener("click", async () => {
    editing.name = $("edit-name").value.trim() || editing.name;
    config = await invoke("update_clip", { entry: editing });
    $("modal").classList.add("hidden");
    renderGrid();
  });
  $("edit-remove").addEventListener("click", async () => {
    config = await invoke("remove_clip", { id: editing.id });
    $("modal").classList.add("hidden");
    renderGrid();
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
  await loadDevices();
  renderGrid();
  await refreshStatus();
  if (!status.running && config.autostart_engine && status.vb_cable_installed && !status.error) {
    await startEngine();
  }
  setInterval(pollLevels, 100);
  setInterval(refreshStatus, 3000);
}

main().catch((e) => showBanner("Startup failed: " + e, "err"));
