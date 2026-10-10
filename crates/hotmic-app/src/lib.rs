//! Tauri shell for Hotmic. Thin: all audio work is in `hotmic-core`, this file is state,
//! persistence, and the commands the HTML frontend calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hotmic_core::clip::{DEFAULT_PEAK_CEILING, DEFAULT_TARGET_RMS};
use hotmic_core::mixer::MixerParams;
use hotmic_core::{devices, Clip, DeviceInfo, Engine, EngineConfig};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

const AUDIO_EXTS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff", "aif"];

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
    pub duration_secs: f32,
    pub hotkey: Option<String>,
    pub color: Option<String>,
}

impl Default for ClipEntry {
    fn default() -> Self {
        ClipEntry {
            id: String::new(),
            name: String::new(),
            path: String::new(),
            gain: 1.0,
            duration_secs: 0.0,
            hotkey: None,
            color: None,
        }
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
    pub clips: Vec<ClipEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mic: String::new(),
            cable: devices::VB_CABLE_RENDER.to_string(),
            monitor: None,
            monitor_enabled: true,
            mic_gain: 1.0,
            clip_gain: 0.8,
            duck: 1.0,
            block_voice: false,
            normalize: true,
            autostart_engine: true,
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
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("config unreadable ({e}), starting fresh");
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
// App state
// ------------------------------------------------------------------------------------------------

pub struct AppState {
    config_path: PathBuf,
    config: Mutex<Config>,
    engine: Mutex<Option<Engine>>,
    cache: Mutex<HashMap<String, Arc<Clip>>>,
    last_error: Mutex<Option<String>>,
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

fn load_clip(entry: &ClipEntry, normalize: bool) -> Result<Arc<Clip>, String> {
    let mut clip = Clip::load(&entry.path).map_err(|e| format!("{e:#}"))?;
    if normalize {
        clip.normalize(DEFAULT_TARGET_RMS, DEFAULT_PEAK_CEILING);
    }
    Ok(Arc::new(clip))
}

impl AppState {
    fn status(&self) -> EngineStatus {
        let engine = self.engine.lock().unwrap();
        let error = self.last_error.lock().unwrap().clone();
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
        let cfg = self.config.lock().unwrap().clone();
        let mut engine = self.engine.lock().unwrap();
        *engine = None; // drop any previous streams first
        let ecfg = EngineConfig {
            mic: cfg.mic.clone(),
            cable: cfg.cable.clone(),
            monitor: cfg.monitor.clone(),
            params: cfg.params(),
            ..Default::default()
        };
        match Engine::start(ecfg) {
            Ok(e) => {
                e.set_monitor(cfg.monitor_enabled && cfg.monitor.is_some());
                *engine = Some(e);
                *self.last_error.lock().unwrap() = None;
            }
            Err(err) => {
                let msg = format!("{err:#}");
                *self.last_error.lock().unwrap() = Some(msg.clone());
                drop(engine);
                return Err(msg);
            }
        }
        drop(engine);
        Ok(self.status())
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
    state.config.lock().unwrap().clone()
}

/// Save settings. Gains apply live; device changes need `engine_start` again.
#[tauri::command]
fn save_config(state: State<AppState>, config: Config) -> Result<Config, String> {
    config.save(&state.config_path)?;
    {
        let mut engine = state.engine.lock().unwrap();
        if let Some(e) = engine.as_mut() {
            e.set_params(config.params());
            e.set_monitor(config.monitor_enabled && config.monitor.is_some());
        }
    }
    let normalize_changed = state.config.lock().unwrap().normalize != config.normalize;
    if normalize_changed {
        state.cache.lock().unwrap().clear();
    }
    *state.config.lock().unwrap() = config.clone();
    Ok(config)
}

#[tauri::command]
fn engine_start(state: State<AppState>) -> Result<EngineStatus, String> {
    state.start_engine()
}

#[tauri::command]
fn engine_stop(state: State<AppState>) -> EngineStatus {
    *state.engine.lock().unwrap() = None;
    state.status()
}

#[tauri::command]
fn engine_status(state: State<AppState>) -> EngineStatus {
    state.status()
}

#[tauri::command]
fn levels(state: State<AppState>) -> Option<Levels> {
    let engine = state.engine.lock().unwrap();
    engine.as_ref().map(|e| {
        let l = e.levels();
        Levels {
            mic_peak: l.mic_peak,
            out_peak: l.out_peak,
            ring_fill_frames: l.ring_fill_frames,
            drift_ppm: l.drift_ppm,
            callbacks: l.output_callbacks,
        }
    })
}

/// Add files or folders. Folders are scanned one level deep for audio files.
#[tauri::command]
fn add_clips(state: State<AppState>, paths: Vec<String>) -> Result<Config, String> {
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

    let normalize = state.config.lock().unwrap().normalize;
    let mut added = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        let path_str = f.to_string_lossy().into_owned();
        let id = clip_id(&path_str);
        if state.config.lock().unwrap().clips.iter().any(|c| c.id == id) {
            continue;
        }
        let mut entry = ClipEntry {
            id: id.clone(),
            name: f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.clone()),
            path: path_str,
            ..Default::default()
        };
        match load_clip(&entry, normalize) {
            Ok(clip) => {
                entry.duration_secs = clip.duration_secs();
                state.cache.lock().unwrap().insert(id, clip);
                added.push(entry);
            }
            Err(e) => errors.push(format!("{}: {e}", entry.name)),
        }
    }

    let mut cfg = state.config.lock().unwrap();
    cfg.clips.extend(added);
    cfg.save(&state.config_path)?;
    let out = cfg.clone();
    drop(cfg);
    if !errors.is_empty() {
        log::warn!("some clips failed to load: {}", errors.join("; "));
        if out.clips.is_empty() {
            return Err(errors.join("\n"));
        }
    }
    Ok(out)
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

#[tauri::command]
fn remove_clip(state: State<AppState>, id: String) -> Result<Config, String> {
    let mut cfg = state.config.lock().unwrap();
    cfg.clips.retain(|c| c.id != id);
    cfg.save(&state.config_path)?;
    state.cache.lock().unwrap().remove(&id);
    Ok(cfg.clone())
}

#[tauri::command]
fn update_clip(state: State<AppState>, entry: ClipEntry) -> Result<Config, String> {
    let mut cfg = state.config.lock().unwrap();
    if let Some(c) = cfg.clips.iter_mut().find(|c| c.id == entry.id) {
        c.name = entry.name;
        c.gain = entry.gain.clamp(0.0, 4.0);
        c.hotkey = entry.hotkey;
        c.color = entry.color;
    }
    cfg.save(&state.config_path)?;
    Ok(cfg.clone())
}

#[tauri::command]
fn play(state: State<AppState>, id: String) -> Result<u64, String> {
    let (entry, normalize) = {
        let cfg = state.config.lock().unwrap();
        let e = cfg
            .clips
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| "unknown clip".to_string())?;
        (e, cfg.normalize)
    };
    let clip = {
        let cached = state.cache.lock().unwrap().get(&id).cloned();
        match cached {
            Some(c) => c,
            None => {
                let c = load_clip(&entry, normalize)?;
                state.cache.lock().unwrap().insert(id.clone(), c.clone());
                c
            }
        }
    };
    let engine = state.engine.lock().unwrap();
    let e = engine
        .as_ref()
        .ok_or_else(|| "engine is not running".to_string())?;
    Ok(e.play_with(clip, entry.gain, false))
}

#[tauri::command]
fn stop_all(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.stop_all();
    }
}

#[tauri::command]
fn play_test_tone(state: State<AppState>) -> Result<u64, String> {
    let engine = state.engine.lock().unwrap();
    let e = engine
        .as_ref()
        .ok_or_else(|| "engine is not running".to_string())?;
    Ok(e.play(Arc::new(Clip::tone(440.0, 1.0))))
}

// ------------------------------------------------------------------------------------------------
// Entry
// ------------------------------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_path = app
                .path()
                .app_config_dir()
                .map(|d| d.join("config.json"))
                .unwrap_or_else(|_| PathBuf::from("hotmic-config.json"));
            let config = Config::load(&config_path);
            log::info!("config at {}", config_path.display());
            let state = AppState {
                config_path,
                config: Mutex::new(config.clone()),
                engine: Mutex::new(None),
                cache: Mutex::new(HashMap::new()),
                last_error: Mutex::new(None),
            };
            if config.autostart_engine {
                if let Err(e) = state.start_engine() {
                    log::warn!("engine did not start: {e}");
                }
            }
            app.manage(state);
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
            play,
            stop_all,
            play_test_tone,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hotmic");
}
