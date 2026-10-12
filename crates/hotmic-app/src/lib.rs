//! Tauri shell for Hotmic. Thin: all audio work is in `hotmic-core`, this file is state,
//! persistence, and the commands the HTML frontend calls.
//!
//! Rules that keep the window responsive:
//! - Anything that touches files (decode, probe) runs on a blocking thread, never in a sync
//!   command on the UI thread.
//! - Decoded audio lives in a cache with a byte budget; the library itself only stores paths.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use hotmic_core::clip::{DEFAULT_PEAK_CEILING, DEFAULT_TARGET_RMS, MAX_CLIP_SECS};
use hotmic_core::mixer::MixerParams;
use hotmic_core::{devices, Clip, DeviceInfo, Engine, EngineConfig};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub mod ptt;

const AUDIO_EXTS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff", "aif"];
/// Decoded audio kept in RAM at most. 512 MB is about 50 minutes of clips.
const CACHE_BUDGET_BYTES: usize = 512 * 1024 * 1024;
/// Background pre-decoding stops once the cache is this full, leaving room for played clips.
const PREWARM_FRACTION: f64 = 0.6;

/// Mutex lock that survives a panic on another thread instead of poisoning the whole app.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------------------------
// Persistent config
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipEntry {
    pub id: String,
    pub name: String,
    pub path: String,
    pub gain: f32,
    /// Duration after trimming, what the tile shows.
    pub duration_secs: f32,
    /// Duration of the whole file.
    pub full_duration_secs: f32,
    /// Non-destructive trim in seconds of the source file.
    pub trim_start: f32,
    pub trim_end: Option<f32>,
    pub hotkey: Option<String>,
    pub color: Option<String>,
    /// Where it came from, when imported from a URL.
    pub source_url: Option<String>,
    pub thumbnail: Option<String>,
}

impl Default for ClipEntry {
    fn default() -> Self {
        ClipEntry {
            id: String::new(),
            name: String::new(),
            path: String::new(),
            gain: 1.0,
            duration_secs: 0.0,
            full_duration_secs: 0.0,
            trim_start: 0.0,
            trim_end: None,
            hotkey: None,
            color: None,
            source_url: None,
            thumbnail: None,
        }
    }
}

impl ClipEntry {
    fn trimmed_duration(&self) -> f32 {
        let end = self
            .trim_end
            .map(|e| e.min(self.full_duration_secs))
            .unwrap_or(self.full_duration_secs);
        (end - self.trim_start.max(0.0)).max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mic: String,
    pub cable: String,
    pub monitor: Option<String>,
    pub monitor_enabled: bool,
    pub mic_gain: f32,
    pub clip_gain: f32,
    pub duck: f32,
    pub block_voice: bool,
    pub normalize: bool,
    pub autostart_engine: bool,
    /// "replace": a new clip fades out whatever is playing. "layer": clips stack.
    pub playback_mode: String,
    /// Global hotkey that stops every playing clip, e.g. "Ctrl+Alt+Digit0".
    pub stop_all_hotkey: Option<String>,
    /// Browser whose YouTube login yt-dlp may use ("chrome", "edge", "firefox"), or None.
    pub yt_cookies_browser: Option<String>,
    /// Closing the window hides it to the tray and keeps the engine running.
    pub close_to_tray: bool,
    /// When launched by Windows at login, start hidden in the tray.
    pub start_minimized: bool,
    /// The first-run setup check has been completed or dismissed.
    pub setup_done: bool,
    /// Auto push-to-talk: hold `ptt_key` while a clip plays.
    pub ptt_enabled: bool,
    /// KeyboardEvent.code name ("KeyV", "F13", "Numpad0") or "Mouse4" / "Mouse5".
    pub ptt_key: Option<String>,
    pub clips: Vec<ClipEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mic: String::new(),
            cable: devices::VB_CABLE_RENDER.to_string(),
            // Empty string = "the system default output", resolved at engine start so a fresh
            // install hears its clips. None = monitoring off.
            monitor: Some(String::new()),
            monitor_enabled: true,
            mic_gain: 1.0,
            clip_gain: 1.0,
            duck: 1.0,
            block_voice: false,
            normalize: true,
            autostart_engine: true,
            playback_mode: "replace".to_string(),
            stop_all_hotkey: None,
            yt_cookies_browser: None,
            close_to_tray: true,
            start_minimized: true,
            setup_done: false,
            ptt_enabled: false,
            ptt_key: None,
            clips: Vec::new(),
        }
    }
}

impl Config {
    fn params(&self) -> MixerParams {
        MixerParams {
            mic_gain: self.mic_gain,
            clip_gain: self.clip_gain,
            duck: self.duck,
            block_voice: self.block_voice,
        }
    }

    fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            // Editors and PowerShell like to prepend a UTF-8 BOM; JSON parsers do not like it.
            Ok(s) => serde_json::from_str(s.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
                log::warn!("config unreadable ({e}), starting fresh");
                let backup = path.with_extension("broken.json");
                let _ = std::fs::copy(path, &backup);
                log::warn!("kept a copy at {}", backup.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}

// ------------------------------------------------------------------------------------------------
// Decoded clip cache (LRU by bytes)
// ------------------------------------------------------------------------------------------------

#[derive(Default)]
struct ClipCache {
    map: HashMap<String, (Arc<Clip>, u64)>,
    bytes: usize,
    tick: u64,
}

impl ClipCache {
    fn get(&mut self, id: &str) -> Option<Arc<Clip>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(id).map(|(c, last)| {
            *last = t;
            c.clone()
        })
    }

    fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    fn insert(&mut self, id: String, clip: Arc<Clip>) {
        let size = clip.samples.len() * std::mem::size_of::<f32>();
        self.remove(&id);
        while self.bytes + size > CACHE_BUDGET_BYTES && !self.map.is_empty() {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
                .expect("non-empty");
            self.remove(&oldest);
        }
        self.tick += 1;
        self.map.insert(id, (clip, self.tick));
        self.bytes += size;
    }

    fn remove(&mut self, id: &str) {
        if let Some((c, _)) = self.map.remove(id) {
            self.bytes -= c.samples.len() * std::mem::size_of::<f32>();
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

// ------------------------------------------------------------------------------------------------
// App state
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum HotkeyAction {
    Play(String),
    StopAll,
}

pub struct AppState {
    config_path: PathBuf,
    config: Mutex<Config>,
    engine: Mutex<Option<Engine>>,
    cache: Mutex<ClipCache>,
    last_error: Mutex<Option<String>>,
    adding: AtomicBool,
    /// Registered global shortcuts, keyed by the shortcut's canonical string.
    hotkeys: Mutex<HashMap<String, HotkeyAction>>,
    /// Where URL imports are saved.
    download_dir: PathBuf,
    ptt: Arc<ptt::PttState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub running: bool,
    pub mic_name: Option<String>,
    pub mic_rate: Option<u32>,
    pub cable_name: Option<String>,
    pub monitor_name: Option<String>,
    pub monitor_on: bool,
    pub error: Option<String>,
    pub vb_cable_installed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Levels {
    pub mic_peak: f32,
    pub out_peak: f32,
    pub ring_fill_frames: u32,
    pub drift_ppm: i32,
    pub callbacks: u64,
    pub active_voices: u32,
    pub ptt_held: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Progress {
    done: usize,
    total: usize,
    name: String,
}

fn clip_id(path: &str) -> String {
    // FNV-1a over the lowercase path: stable, short, no extra dependency.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Decode, trim and normalise. A decoder panic on a hostile file becomes an error, not a crash.
fn load_clip(entry: &ClipEntry, normalize: bool) -> Result<Arc<Clip>, String> {
    let path = entry.path.clone();
    let (trim_start, trim_end) = (entry.trim_start, entry.trim_end);
    let result = catch_unwind(AssertUnwindSafe(move || {
        let mut clip = Clip::load(&path).map_err(|e| format!("{e:#}"))?;
        if trim_start > 0.0 || trim_end.is_some() {
            clip = clip.trimmed(trim_start, trim_end);
        }
        if normalize {
            clip.normalize(DEFAULT_TARGET_RMS, DEFAULT_PEAK_CEILING);
        }
        Ok::<Arc<Clip>, String>(Arc::new(clip))
    }));
    match result {
        Ok(r) => r,
        Err(_) => Err("the decoder crashed on this file".to_string()),
    }
}

/// Duration from headers when possible, otherwise by decoding once.
fn probe_duration(entry: &ClipEntry) -> Result<f32, String> {
    let path = entry.path.clone();
    let result = catch_unwind(AssertUnwindSafe(move || {
        let info = Clip::probe(&path).map_err(|e| format!("{e:#}"))?;
        match info.duration_secs {
            Some(d) if d > MAX_CLIP_SECS => Err(format!(
                "{:.0} minutes long, over the {:.0} minute limit",
                d / 60.0,
                MAX_CLIP_SECS / 60.0
            )),
            Some(d) => Ok(d),
            None => Clip::load(&path)
                .map(|c| c.duration_secs())
                .map_err(|e| format!("{e:#}")),
        }
    }));
    match result {
        Ok(r) => r,
        Err(_) => Err("the decoder crashed on this file".to_string()),
    }
}

/// Turn the monitor setting into a concrete device spec. An empty spec means "default output",
/// unless the default output *is* the cable (VB-Cable's installer makes it the default), in
/// which case the first real output device is used so the user is not fed their own clips twice.
fn resolve_monitor(spec: &str, cable_spec: &str) -> String {
    if !spec.trim().is_empty() {
        return spec.to_string();
    }
    let Ok(list) = devices::list() else {
        return String::new();
    };
    let is_cable = |d: &DeviceInfo| {
        d.name.contains("CABLE") || d.id == cable_spec || d.name.contains(cable_spec)
    };
    if let Some(d) = list.iter().find(|d| d.is_output && d.is_default_output && !is_cable(d)) {
        return d.id.clone();
    }
    list.iter()
        .find(|d| d.is_output && !is_cable(d))
        .map(|d| d.id.clone())
        .unwrap_or_default()
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

impl AppState {
    fn status(&self) -> EngineStatus {
        let engine = lock(&self.engine);
        let error = lock(&self.last_error).clone();
        let vb = devices::find_vb_cable().is_some();
        match engine.as_ref() {
            Some(e) => EngineStatus {
                running: true,
                mic_name: Some(e.info.mic_name.clone()),
                mic_rate: Some(e.info.mic_rate),
                cable_name: Some(e.info.cable_name.clone()),
                monitor_name: e.info.monitor_name.clone(),
                monitor_on: e.monitor_enabled(),
                error,
                vb_cable_installed: vb,
            },
            None => EngineStatus {
                running: false,
                mic_name: None,
                mic_rate: None,
                cable_name: None,
                monitor_name: None,
                monitor_on: false,
                error,
                vb_cable_installed: vb,
            },
        }
    }

    fn start_engine(&self) -> Result<EngineStatus, String> {
        let cfg = lock(&self.config).clone();
        let mut engine = lock(&self.engine);
        *engine = None; // drop any previous streams first
        let ecfg = EngineConfig {
            mic: cfg.mic.clone(),
            cable: cfg.cable.clone(),
            monitor: cfg.monitor.clone().map(|m| resolve_monitor(&m, &cfg.cable)),
            params: cfg.params(),
            ..Default::default()
        };
        match Engine::start(ecfg) {
            Ok(e) => {
                e.set_monitor(cfg.monitor_enabled && cfg.monitor.is_some());
                *engine = Some(e);
                *lock(&self.last_error) = None;
            }
            Err(err) => {
                let msg = format!("{err:#}");
                *lock(&self.last_error) = Some(msg.clone());
                drop(engine);
                return Err(msg);
            }
        }
        drop(engine);
        Ok(self.status())
    }

    /// Get a decoded clip, decoding on a miss. Call from a blocking thread.
    fn clip(&self, id: &str) -> Result<(Arc<Clip>, ClipEntry), String> {
        let (entry, normalize) = {
            let cfg = lock(&self.config);
            let e = cfg
                .clips
                .iter()
                .find(|c| c.id == id)
                .cloned()
                .ok_or_else(|| "unknown clip".to_string())?;
            (e, cfg.normalize)
        };
        if let Some(c) = lock(&self.cache).get(id) {
            return Ok((c, entry));
        }
        let c = load_clip(&entry, normalize)?;
        lock(&self.cache).insert(id.to_string(), c.clone());
        Ok((c, entry))
    }
}

/// Play a clip by id from any thread. Decodes on a cache miss.
fn play_by_id(app: &AppHandle, id: &str) -> Result<u64, String> {
    let state = app.state::<AppState>();
    let exclusive = lock(&state.config).playback_mode != "layer";
    let (clip, entry) = state.clip(id)?;
    let engine = lock(&state.engine);
    let e = engine
        .as_ref()
        .ok_or_else(|| "engine is not running".to_string())?;
    let voice = e.play_with(clip, entry.gain, false, exclusive);
    let _ = app.emit("clip-started", serde_json::json!({ "id": id, "voice": voice, "exclusive": exclusive }));
    Ok(voice)
}

/// Re-register every global hotkey from the config. Returns one message per key that failed
/// (bad syntax or already taken by another app or clip).
fn sync_hotkeys(app: &AppHandle) -> Vec<String> {
    let state = app.state::<AppState>();
    let gs = app.global_shortcut();
    let mut errors = Vec::new();
    let mut map: HashMap<String, HotkeyAction> = HashMap::new();
    if let Err(e) = gs.unregister_all() {
        log::warn!("unregister_all: {e}");
    }

    let cfg = lock(&state.config).clone();
    let mut wanted: Vec<(String, String, HotkeyAction)> = cfg
        .clips
        .iter()
        .filter_map(|c| {
            c.hotkey
                .as_ref()
                .filter(|h| !h.trim().is_empty())
                .map(|h| (h.clone(), c.name.clone(), HotkeyAction::Play(c.id.clone())))
        })
        .collect();
    if let Some(h) = cfg.stop_all_hotkey.as_ref().filter(|h| !h.trim().is_empty()) {
        wanted.push((h.clone(), "Stop all".to_string(), HotkeyAction::StopAll));
    }

    for (text, label, action) in wanted {
        let shortcut: Shortcut = match text.parse() {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{label}: '{text}' is not a valid hotkey ({e})"));
                continue;
            }
        };
        let key = shortcut.into_string();
        if let Some(prev) = map.get(&key) {
            let other = match prev {
                HotkeyAction::Play(id) => cfg
                    .clips
                    .iter()
                    .find(|c| &c.id == id)
                    .map(|c| c.name.clone())
                    .unwrap_or_default(),
                HotkeyAction::StopAll => "Stop all".to_string(),
            };
            errors.push(format!("{label}: '{text}' is already used by {other}"));
            continue;
        }
        match gs.register(shortcut) {
            Ok(()) => {
                map.insert(key, action);
            }
            Err(e) => errors.push(format!("{label}: could not register '{text}' ({e})")),
        }
    }
    *lock(&state.hotkeys) = map;
    errors
}

/// Decode clips in the background until the cache is comfortably full, so first plays are instant.
fn prewarm(app: &AppHandle, ids: Vec<String>) {
    let state = app.state::<AppState>();
    let budget = (CACHE_BUDGET_BYTES as f64 * PREWARM_FRACTION) as usize;
    for id in ids {
        if lock(&state.cache).bytes >= budget {
            break;
        }
        if lock(&state.cache).contains(&id) {
            continue;
        }
        let (entry, normalize) = {
            let cfg = lock(&state.config);
            match cfg.clips.iter().find(|c| c.id == id) {
                Some(e) => (e.clone(), cfg.normalize),
                None => continue,
            }
        };
        match load_clip(&entry, normalize) {
            Ok(c) => lock(&state.cache).insert(id, c),
            Err(e) => log::warn!("prewarm skipped {}: {e}", entry.name),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Commands
// ------------------------------------------------------------------------------------------------

#[tauri::command]
fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    devices::list().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    lock(&state.config).clone()
}

/// Save settings. Gains apply live; device changes need `engine_start` again.
#[tauri::command]
fn save_config(state: State<AppState>, config: Config) -> Result<Config, String> {
    config.save(&state.config_path)?;
    {
        let mut engine = lock(&state.engine);
        if let Some(e) = engine.as_mut() {
            e.set_params(config.params());
            e.set_monitor(config.monitor_enabled && config.monitor.is_some());
        }
    }
    let normalize_changed = lock(&state.config).normalize != config.normalize;
    if normalize_changed {
        lock(&state.cache).clear();
    }
    *lock(&state.config) = config.clone();
    Ok(config)
}

/// Re-register global hotkeys after the library or settings changed. Returns problems, if any.
#[tauri::command]
fn apply_hotkeys(app: AppHandle) -> Vec<String> {
    sync_hotkeys(&app)
}

#[tauri::command]
fn engine_start(state: State<AppState>) -> Result<EngineStatus, String> {
    state.start_engine()
}

#[tauri::command]
fn engine_stop(state: State<AppState>) -> EngineStatus {
    *lock(&state.engine) = None;
    state.status()
}

#[tauri::command]
fn engine_status(state: State<AppState>) -> EngineStatus {
    state.status()
}

#[tauri::command]
fn levels(state: State<AppState>) -> Option<Levels> {
    let engine = lock(&state.engine);
    let held = state.ptt.held.load(Ordering::Relaxed);
    engine.as_ref().map(|e| {
        let l = e.levels();
        Levels {
            mic_peak: l.mic_peak,
            out_peak: l.out_peak,
            ring_fill_frames: l.ring_fill_frames,
            drift_ppm: l.drift_ppm,
            callbacks: l.output_callbacks,
            active_voices: l.active_voices,
            ptt_held: held,
        }
    })
}

/// Add files or folders (scanned one level deep). Runs off the UI thread and reports progress
/// through the `library-progress` event.
#[tauri::command]
async fn add_clips(app: AppHandle, paths: Vec<String>) -> Result<Config, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || add_clips_blocking(&handle, paths))
        .await
        .map_err(|e| format!("add task failed: {e}"))?
}

fn add_clips_blocking(app: &AppHandle, paths: Vec<String>) -> Result<Config, String> {
    let state = app.state::<AppState>();
    if state
        .adding
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("already adding clips, wait for it to finish".to_string());
    }
    struct Done<'a>(&'a AtomicBool);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done(&state.adding);

    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        let path = PathBuf::from(&p);
        if path.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&path) {
                let mut found: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| is_audio(p))
                    .collect();
                found.sort();
                files.extend(found);
            }
        } else if is_audio(&path) {
            files.push(path);
        }
    }

    let total = files.len();
    let mut added: Vec<ClipEntry> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (i, f) in files.into_iter().enumerate() {
        let path_str = f.to_string_lossy().into_owned();
        let id = clip_id(&path_str);
        let already = lock(&state.config).clips.iter().any(|c| c.id == id)
            || added.iter().any(|c| c.id == id);
        if already {
            continue;
        }
        let mut entry = ClipEntry {
            id,
            name: f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.clone()),
            path: path_str,
            ..Default::default()
        };
        let _ = app.emit(
            "library-progress",
            Progress {
                done: i,
                total,
                name: entry.name.clone(),
            },
        );
        match probe_duration(&entry) {
            Ok(d) => {
                entry.full_duration_secs = d;
                entry.duration_secs = d;
                added.push(entry);
            }
            Err(e) => errors.push(format!("{}: {e}", entry.name)),
        }
    }
    let _ = app.emit(
        "library-progress",
        Progress {
            done: total,
            total,
            name: String::new(),
        },
    );

    let new_ids: Vec<String> = added.iter().map(|c| c.id.clone()).collect();
    let out = {
        let mut cfg = lock(&state.config);
        cfg.clips.extend(added);
        cfg.save(&state.config_path)?;
        cfg.clone()
    };

    if !errors.is_empty() {
        log::warn!("{} clips failed: {}", errors.len(), errors.join("; "));
        if new_ids.is_empty() {
            return Err(errors.join("\n"));
        }
    }

    // Decode the new clips in the background so the first click does not wait.
    let handle = app.clone();
    std::thread::spawn(move || prewarm(&handle, new_ids));

    Ok(out)
}

#[tauri::command]
fn remove_clip(state: State<AppState>, id: String) -> Result<Config, String> {
    let mut cfg = lock(&state.config);
    cfg.clips.retain(|c| c.id != id);
    cfg.save(&state.config_path)?;
    lock(&state.cache).remove(&id);
    Ok(cfg.clone())
}

#[tauri::command]
fn update_clip(state: State<AppState>, entry: ClipEntry) -> Result<Config, String> {
    let mut cfg = lock(&state.config);
    let mut trim_changed = false;
    if let Some(c) = cfg.clips.iter_mut().find(|c| c.id == entry.id) {
        c.name = entry.name;
        c.gain = entry.gain.clamp(0.0, 4.0);
        c.hotkey = entry.hotkey;
        c.color = entry.color;
        let start = entry.trim_start.max(0.0);
        let end = entry.trim_end.filter(|&e| e > start);
        if (c.trim_start - start).abs() > 1e-3 || c.trim_end != end {
            trim_changed = true;
            c.trim_start = start;
            c.trim_end = end;
            if c.full_duration_secs <= 0.0 {
                c.full_duration_secs = probe_duration(c).unwrap_or(c.duration_secs);
            }
            c.duration_secs = c.trimmed_duration();
        }
    }
    cfg.save(&state.config_path)?;
    if trim_changed {
        lock(&state.cache).remove(&entry.id);
    }
    Ok(cfg.clone())
}

// ---- URL import (yt-dlp) ---------------------------------------------------------------------

#[tauri::command]
fn tools_status() -> hotmic_core::ytdlp::ToolStatus {
    hotmic_core::ytdlp::status()
}

#[tauri::command]
async fn tools_install() -> Result<hotmic_core::ytdlp::ToolStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        hotmic_core::ytdlp::install().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn tools_update() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let p = hotmic_core::ytdlp::find().ok_or("yt-dlp is not installed")?;
        hotmic_core::ytdlp::update(&p).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn yt_options(state: &AppState) -> hotmic_core::ytdlp::Options {
    hotmic_core::ytdlp::Options {
        cookies_from_browser: lock(&state.config)
            .yt_cookies_browser
            .clone()
            .filter(|b| !b.trim().is_empty()),
    }
}

#[tauri::command]
async fn yt_probe(app: AppHandle, url: String) -> Result<hotmic_core::ytdlp::VideoInfo, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let p = hotmic_core::ytdlp::find().ok_or("yt-dlp is not installed")?;
        hotmic_core::ytdlp::probe(&p, url.trim(), &yt_options(&state)).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Serialize)]
struct YtProgress {
    percent: f32,
    text: String,
}

/// Download the audio of `url`, store it in the download folder, add it to the library with the
/// given trim. Progress arrives on the `yt-progress` event.
#[tauri::command]
async fn yt_import(
    app: AppHandle,
    url: String,
    name: Option<String>,
    start: Option<f32>,
    end: Option<f32>,
) -> Result<Config, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let tool = hotmic_core::ytdlp::find().ok_or("yt-dlp is not installed")?;
        let url = url.trim().to_string();
        let opts = yt_options(&state);

        let info = hotmic_core::ytdlp::probe(&tool, &url, &opts).map_err(|e| format!("{e:#}"))?;
        let _ = handle.emit(
            "yt-progress",
            YtProgress {
                percent: 0.0,
                text: format!("Downloading {}", info.title),
            },
        );
        let file = hotmic_core::ytdlp::download(&tool, &url, &state.download_dir, &opts, |pct, line| {
            let _ = handle.emit(
                "yt-progress",
                YtProgress {
                    percent: pct,
                    text: line.to_string(),
                },
            );
        })
        .map_err(|e| format!("{e:#}"))?;

        let path_str = file.to_string_lossy().into_owned();
        let id = clip_id(&path_str);
        let mut entry = ClipEntry {
            id: id.clone(),
            name: name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| info.title.clone()),
            path: path_str,
            source_url: info.webpage_url.clone().or(Some(url.clone())),
            thumbnail: info.thumbnail.clone(),
            ..Default::default()
        };
        entry.full_duration_secs = probe_duration(&entry)?;
        let start = start.unwrap_or(0.0).max(0.0);
        entry.trim_start = start;
        entry.trim_end = end.filter(|&e| e > start && e < entry.full_duration_secs);
        entry.duration_secs = entry.trimmed_duration();

        let out = {
            let mut cfg = lock(&state.config);
            cfg.clips.retain(|c| c.id != id);
            cfg.clips.push(entry);
            cfg.save(&state.config_path)?;
            cfg.clone()
        };
        lock(&state.cache).remove(&id);
        let _ = handle.emit(
            "yt-progress",
            YtProgress {
                percent: 100.0,
                text: "Added to library".to_string(),
            },
        );
        let h2 = handle.clone();
        std::thread::spawn(move || prewarm(&h2, vec![id]));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn play(app: AppHandle, id: String) -> Result<u64, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || play_by_id(&handle, &id))
        .await
        .map_err(|e| format!("play task failed: {e}"))?
}

#[tauri::command]
fn stop_all(state: State<AppState>) {
    if let Some(e) = lock(&state.engine).as_ref() {
        e.stop_all();
    }
}

#[tauri::command]
fn play_test_tone(state: State<AppState>) -> Result<u64, String> {
    let engine = lock(&state.engine);
    let e = engine
        .as_ref()
        .ok_or_else(|| "engine is not running".to_string())?;
    Ok(e.play(Arc::new(Clip::tone(440.0, 1.0))))
}

// ---- First-run setup checks ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SetupChecks {
    pub vb_cable_installed: bool,
    pub default_output_name: Option<String>,
    pub default_output_is_cable: bool,
    pub default_input_name: Option<String>,
    pub engine_running: bool,
    pub engine_error: Option<String>,
}

#[tauri::command]
fn setup_checks(state: State<AppState>) -> SetupChecks {
    let list = devices::list().unwrap_or_default();
    let out = list.iter().find(|d| d.is_output && d.is_default_output);
    let inp = list.iter().find(|d| d.is_input && d.is_default_input);
    let status = state.status();
    SetupChecks {
        vb_cable_installed: status.vb_cable_installed,
        default_output_name: out.map(|d| d.name.clone()),
        default_output_is_cable: out.map(|d| d.name.contains("CABLE")).unwrap_or(false),
        default_input_name: inp.map(|d| d.name.clone()),
        engine_running: status.running,
        engine_error: status.error,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SelfTest {
    pub peak: f32,
    pub pass: bool,
    pub message: String,
}

/// Play a tone through the engine into the cable and listen on CABLE Output, the device the
/// call app will use. Proves the whole path without needing a second person.
#[tauri::command]
async fn setup_selftest(app: AppHandle) -> Result<SelfTest, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        {
            let engine = lock(&state.engine);
            let e = engine
                .as_ref()
                .ok_or_else(|| "engine is not running".to_string())?;
            e.play(Arc::new(Clip::tone(440.0, 1.2)));
        }
        let peak = devices::measure_peak("CABLE Output", 1.5).map_err(|e| format!("{e:#}"))?;
        let pass = peak > 0.05;
        Ok(SelfTest {
            peak,
            pass,
            message: if pass {
                format!("The tone arrived on CABLE Output (peak {:.0}%). Discord will hear Hotmic.", peak * 100.0)
            } else {
                format!("Nothing arrived on CABLE Output (peak {:.1}%). Check that Send to is CABLE Input.", peak * 100.0)
            },
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---- Tray, autostart, lifecycle ---------------------------------------------------------------

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "Show Hotmic", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop all clips", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &stop, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Hotmic")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "stop" => {
                let state = app.state::<AppState>();
                if let Some(e) = lock(&state.engine).as_ref() {
                    e.stop_all();
                }
                let _ = app.emit("clips-stopped", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

#[tauri::command]
fn autostart_enabled(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    let r = if enabled { al.enable() } else { al.disable() };
    r.map_err(|e| e.to_string())?;
    Ok(al.is_enabled().unwrap_or(enabled))
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn hide_to_tray(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

// ------------------------------------------------------------------------------------------------
// Entry
// ------------------------------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch just brings the running one forward.
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.app_handle().state::<AppState>();
                if lock(&state.config).close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    let state = app.state::<AppState>();
                    let action = lock(&state.hotkeys).get(&shortcut.into_string()).cloned();
                    match action {
                        Some(HotkeyAction::Play(id)) => {
                            let handle = app.clone();
                            std::thread::spawn(move || {
                                if let Err(e) = play_by_id(&handle, &id) {
                                    log::warn!("hotkey play failed: {e}");
                                }
                            });
                        }
                        Some(HotkeyAction::StopAll) => {
                            if let Some(e) = lock(&state.engine).as_ref() {
                                e.stop_all();
                            }
                            let _ = app.emit("clips-stopped", ());
                        }
                        None => {}
                    }
                })
                .build(),
        )
        .setup(|app| {
            let config_path = app
                .path()
                .app_config_dir()
                .map(|d| d.join("config.json"))
                .unwrap_or_else(|_| PathBuf::from("hotmic-config.json"));
            let config = Config::load(&config_path);
            log::info!("config at {}", config_path.display());
            let download_dir = app
                .path()
                .audio_dir()
                .map(|d| d.join("Hotmic"))
                .unwrap_or_else(|_| config_path.parent().unwrap().join("downloads"));
            let state = AppState {
                download_dir,
                config_path,
                config: Mutex::new(config.clone()),
                engine: Mutex::new(None),
                cache: Mutex::new(ClipCache::default()),
                last_error: Mutex::new(None),
                adding: AtomicBool::new(false),
                hotkeys: Mutex::new(HashMap::new()),
                ptt: Arc::new(ptt::PttState::default()),
            };
            if config.autostart_engine {
                if let Err(e) = state.start_engine() {
                    log::warn!("engine did not start: {e}");
                }
            }
            app.manage(state);
            for e in sync_hotkeys(app.handle()) {
                log::warn!("hotkey: {e}");
            }
            if let Err(e) = build_tray(app.handle()) {
                log::warn!("tray icon unavailable: {e}");
            }

            // Auto push-to-talk watcher: holds the configured key while any clip plays.
            {
                let handle = app.handle().clone();
                let ptt_state = handle.state::<AppState>().ptt.clone();
                std::thread::spawn(move || {
                    let h1 = handle.clone();
                    let h2 = handle.clone();
                    ptt::run_loop(
                        ptt_state,
                        move || {
                            let st = h1.state::<AppState>();
                            let cfg = lock(&st.config);
                            if !cfg.ptt_enabled {
                                return None;
                            }
                            cfg.ptt_key.as_deref().and_then(ptt::parse)
                        },
                        move || {
                            let st = h2.state::<AppState>();
                            let engine = lock(&st.engine);
                            engine.as_ref().map(|e| e.levels().active_voices).unwrap_or(0)
                        },
                        std::time::Duration::from_millis(200),
                    );
                });
            }
            let minimized = std::env::args().any(|a| a == "--minimized");
            if minimized && config.start_minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            // Warm the cache with the saved library, off the UI thread.
            let ids: Vec<String> = config.clips.iter().map(|c| c.id.clone()).collect();
            let handle = app.handle().clone();
            std::thread::spawn(move || prewarm(&handle, ids));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_devices,
            get_config,
            save_config,
            engine_start,
            engine_stop,
            engine_status,
            levels,
            add_clips,
            remove_clip,
            update_clip,
            apply_hotkeys,
            play,
            stop_all,
            play_test_tone,
            tools_status,
            tools_install,
            tools_update,
            yt_probe,
            yt_import,
            autostart_enabled,
            set_autostart,
            quit_app,
            hide_to_tray,
            setup_checks,
            setup_selftest,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Hotmic")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Never leave the user's push-to-talk key stuck down.
                let state = app.state::<AppState>();
                if state.ptt.held.load(Ordering::Relaxed) {
                    if let Some(k) = lock(&state.config).ptt_key.as_deref().and_then(ptt::parse) {
                        ptt::send(k, false);
                    }
                }
            }
        });
}
