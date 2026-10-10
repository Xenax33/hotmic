//! Command line harness for the Hotmic engine. Not the product, just the fastest way to test it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use hotmic_core::mixer::MixerParams;
use hotmic_core::{devices, Clip, Engine, EngineConfig, Event};

#[derive(Parser)]
#[command(name = "hotmic", about = "Hotmic audio engine test harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List audio devices.
    Devices,
    /// Decode files and print what the engine would play.
    Probe { files: Vec<PathBuf> },
    /// Run the engine: mic + clips into the cable. Keys: 1-9 play, T tone, 0 stop, M monitor, Q quit.
    Run {
        /// Mic device id or name substring. Default: system default input.
        #[arg(long, default_value = "")]
        mic: String,
        /// Output device the mix goes to.
        #[arg(long, default_value = devices::VB_CABLE_RENDER)]
        cable: String,
        /// Also play clips into this output (empty string = default output).
        #[arg(long)]
        monitor: Option<String>,
        /// Folder with up to 9 clips.
        #[arg(long, default_value = "spike/clips")]
        clips: PathBuf,
        #[arg(long, default_value_t = 1.0)]
        mic_gain: f32,
        #[arg(long, default_value_t = 0.8)]
        clip_gain: f32,
        /// Mic multiplier while a clip plays (1.0 = no ducking).
        #[arg(long, default_value_t = 1.0)]
        duck: f32,
        /// Seconds to run without a keyboard (for scripted tests).
        #[arg(long)]
        headless_secs: Option<f32>,
        /// Times (seconds) at which headless mode plays the tone, comma separated.
        #[arg(long, default_value = "0.5,2.5", value_delimiter = ',')]
        tone_times: Vec<f32>,
        /// In headless mode, play this clip (1-based index into the clips folder) instead of the tone.
        #[arg(long)]
        headless_clip: Option<usize>,
    },
}

struct RunOpts {
    mic: String,
    cable: String,
    monitor: Option<String>,
    clips_dir: PathBuf,
    params: MixerParams,
    headless_secs: Option<f32>,
    tone_times: Vec<f32>,
    headless_clip: Option<usize>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match Cli::parse().cmd {
        Cmd::Devices => cmd_devices(),
        Cmd::Probe { files } => cmd_probe(files),
        Cmd::Run {
            mic,
            cable,
            monitor,
            clips,
            mic_gain,
            clip_gain,
            duck,
            headless_secs,
            tone_times,
            headless_clip,
        } => cmd_run(RunOpts {
            mic,
            cable,
            monitor,
            clips_dir: clips,
            params: MixerParams {
                mic_gain,
                clip_gain,
                duck,
                block_voice: false,
            },
            headless_secs,
            tone_times,
            headless_clip,
        }),
    }
}

fn cmd_devices() -> Result<()> {
    for d in devices::list()? {
        let mut tags = Vec::new();
        if d.is_input {
            tags.push(format!(
                "in:{}ch@{}",
                d.input_channels,
                d.input_sample_rate.unwrap_or(0)
            ));
        }
        if d.is_output {
            tags.push(format!(
                "out:{}ch@{}",
                d.output_channels,
                d.output_sample_rate.unwrap_or(0)
            ));
        }
        if d.is_default_input {
            tags.push("default-in".into());
        }
        if d.is_default_output {
            tags.push("default-out".into());
        }
        println!("{:<55} {}", d.name, tags.join("  "));
        println!("    id: {}", d.id);
    }
    match devices::find_vb_cable() {
        Some(d) => println!("\nVB-Cable found: {}", d.name),
        None => println!("\nVB-Cable NOT found. Install it from https://vb-audio.com/Cable/"),
    }
    Ok(())
}

fn cmd_probe(files: Vec<PathBuf>) -> Result<()> {
    for f in files {
        let t = Instant::now();
        match Clip::load(&f) {
            Ok(c) => println!(
                "{}: {:.2}s, peak {:.2}, source {} Hz x{}, decoded in {} ms",
                c.name,
                c.duration_secs(),
                c.peak(),
                c.source_rate,
                c.source_channels,
                t.elapsed().as_millis()
            ),
            Err(e) => println!("{}: FAILED: {e:#}", f.display()),
        }
    }
    Ok(())
}

fn load_folder(dir: &PathBuf) -> Vec<Arc<Clip>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !["mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff"].contains(&ext.as_str()) {
            continue;
        }
        match Clip::load(&p) {
            Ok(mut c) => {
                let gain = c.normalize(
                    hotmic_core::clip::DEFAULT_TARGET_RMS,
                    hotmic_core::clip::DEFAULT_PEAK_CEILING,
                );
                println!(
                    "  [{}] {} ({:.1}s, normalised x{:.2}, peak {:.2})",
                    out.len() + 1,
                    c.name,
                    c.duration_secs(),
                    gain,
                    c.peak()
                );
                out.push(Arc::new(c));
            }
            Err(e) => println!("  skipped {}: {e:#}", p.display()),
        }
        if out.len() == 9 {
            break;
        }
    }
    out
}

fn report(engine: &Engine, underruns: usize, overruns: usize) {
    let l = engine.levels();
    println!(
        "  cb={} ring={}f drift={}ppm mic={:.2} out={:.2} under={} over={}\r",
        l.output_callbacks,
        l.ring_fill_frames,
        l.drift_ppm,
        l.mic_peak,
        l.out_peak,
        underruns,
        overruns
    );
}

fn cmd_run(opts: RunOpts) -> Result<()> {
    println!("clips from {}:", opts.clips_dir.display());
    let clips = load_folder(&opts.clips_dir);
    if clips.is_empty() {
        println!("  (none, press T for a test tone)");
    }
    let tone = Arc::new(Clip::tone(440.0, 1.0));

    let cfg = EngineConfig {
        mic: opts.mic,
        cable: opts.cable,
        monitor: opts.monitor,
        params: opts.params,
        ..Default::default()
    };
    let mut engine = Engine::start(cfg).context("starting engine")?;
    println!(
        "mic    : {} ({} ch @ {} Hz)\ncable  : {}\nmonitor: {}",
        engine.info.mic_name,
        engine.info.mic_channels,
        engine.info.mic_rate,
        engine.info.cable_name,
        engine.info.monitor_name.as_deref().unwrap_or("off")
    );

    let mut underruns = 0usize;
    let mut overruns = 0usize;
    let mut last_report = Instant::now();
    let started = Instant::now();

    if let Some(secs) = opts.headless_secs {
        let mut tone_times: Vec<Duration> = opts
            .tone_times
            .iter()
            .map(|&s| Duration::from_secs_f32(s.max(0.0)))
            .collect();
        tone_times.sort();
        let what: Arc<Clip> = match opts.headless_clip {
            Some(n) if n >= 1 && n <= clips.len() => clips[n - 1].clone(),
            Some(n) => anyhow::bail!("no clip number {n}, folder has {}", clips.len()),
            None => tone.clone(),
        };
        while started.elapsed().as_secs_f32() < secs {
            while tone_times.first().is_some_and(|&t| started.elapsed() > t) {
                tone_times.remove(0);
                let id = engine.play(what.clone());
                println!(
                    "  t={:.2}s play {} -> voice {id}",
                    started.elapsed().as_secs_f32(),
                    what.name
                );
            }
            drain_events(&engine, &mut underruns, &mut overruns);
            if last_report.elapsed() > Duration::from_secs(1) {
                report(&engine, underruns, overruns);
                last_report = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        report(&engine, underruns, overruns);
        return Ok(());
    }

    println!("\nkeys: 1-9 play clip, T tone, 0 stop all, M toggle monitor, [ ] mic gain, Q or Ctrl+C quit");
    println!("status line every 5 s: cb = output callbacks, ring = mic frames buffered, under/over = dropped mic frames\n");
    crossterm::terminal::enable_raw_mode()?;
    let result = interactive_loop(&mut engine, &clips, &tone, &mut underruns, &mut overruns, &mut last_report);
    crossterm::terminal::disable_raw_mode()?;
    result
}

fn interactive_loop(
    engine: &mut Engine,
    clips: &[Arc<Clip>],
    tone: &Arc<Clip>,
    underruns: &mut usize,
    overruns: &mut usize,
    last_report: &mut Instant,
) -> Result<()> {
    loop {
        if event::poll(Duration::from_millis(20))? {
            if let TermEvent::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
                {
                    break;
                }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        let id = engine.play(tone.clone());
                        println!("tone -> voice {id}\r");
                    }
                    KeyCode::Char('0') => engine.stop_all(),
                    KeyCode::Char('m') | KeyCode::Char('M') => {
                        let on = !engine.monitor_enabled();
                        engine.set_monitor(on);
                        println!("monitor {}\r", if on { "on" } else { "off" });
                    }
                    KeyCode::Char('[') | KeyCode::Char(']') => {
                        let mut p = engine.params();
                        p.mic_gain = if k.code == KeyCode::Char('[') {
                            (p.mic_gain - 0.1).max(0.0)
                        } else {
                            (p.mic_gain + 0.1).min(4.0)
                        };
                        engine.set_params(p);
                        println!("mic gain {:.1}\r", p.mic_gain);
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        let n = c.to_digit(10).unwrap_or(0) as usize;
                        if n >= 1 && n <= clips.len() {
                            println!("play {}\r", clips[n - 1].name);
                            engine.play(clips[n - 1].clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        drain_events(engine, underruns, overruns);
        if last_report.elapsed() > Duration::from_secs(5) {
            report(engine, *underruns, *overruns);
            *last_report = Instant::now();
        }
    }
    println!("stopped\r");
    Ok(())
}

fn drain_events(engine: &Engine, underruns: &mut usize, overruns: &mut usize) {
    while let Some(ev) = engine.try_event() {
        match ev {
            Event::VoiceFinished(_) => {}
            Event::MicUnderrun { missing_frames } => *underruns += missing_frames,
            Event::MicOverrun { dropped_frames } => *overruns += dropped_frames,
            Event::StreamError { stream, message } => {
                println!("stream error ({stream}): {message}\r")
            }
        }
    }
}
