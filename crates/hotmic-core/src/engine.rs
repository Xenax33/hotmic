//! The engine: cpal streams plus the ring buffers between them.
//!
//! ```text
//! mic device --input cb--> [resample to 48k, drift control] --ring--> output cb --> cable device
//!                                                         voices ---^   |
//!                                                                       +--ring--> monitor cb --> headphones
//! ```
//!
//! Commands reach the output callback through a bounded channel, events leave the same way.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::clip::Clip;
use crate::devices::{self, DeviceKind};
use crate::mixer::{Mixer, MixerParams, Voice, VoiceId};
use crate::{CHANNELS, SAMPLE_RATE};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Mic device spec (see [`devices::find`]). Empty = system default input.
    pub mic: String,
    /// Device we render the mix into. Normally "CABLE Input".
    pub cable: String,
    /// Optional device that receives clips only, so the user hears them.
    pub monitor: Option<String>,
    pub params: MixerParams,
    /// Mic frames kept in the ring buffer as jitter protection. Adds this much latency.
    pub mic_buffer_ms: f32,
    pub max_voices: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            mic: String::new(),
            cable: devices::VB_CABLE_RENDER.to_string(),
            monitor: None,
            params: MixerParams::default(),
            mic_buffer_ms: 20.0,
            max_voices: 32,
        }
    }
}

pub enum Command {
    Play {
        id: VoiceId,
        clip: Arc<Clip>,
        gain: f32,
        looping: bool,
    },
    Stop(VoiceId),
    StopAll,
    SetParams(MixerParams),
    SetMonitor(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    VoiceFinished(VoiceId),
    /// The output callback found fewer mic frames than it needed.
    MicUnderrun { missing_frames: usize },
    /// The mic callback produced frames faster than the output consumed them.
    MicOverrun { dropped_frames: usize },
    StreamError { stream: &'static str, message: String },
}

#[derive(Debug, Default)]
struct Levels {
    mic_peak: AtomicU32,
    out_peak: AtomicU32,
    ring_fill: AtomicU32,
    callbacks: AtomicU64,
    ratio_ppm: AtomicU32,
}

#[derive(Debug, Clone, Copy)]
pub struct LevelSnapshot {
    /// Peak absolute mic sample in the last block, 0..1.
    pub mic_peak: f32,
    /// Peak absolute output sample in the last block, 0..1.
    pub out_peak: f32,
    /// Mic frames waiting in the ring buffer.
    pub ring_fill_frames: u32,
    pub output_callbacks: u64,
    /// Current drift correction, in parts per million away from nominal.
    pub drift_ppm: i32,
}

pub struct EngineInfo {
    pub mic_name: String,
    pub mic_rate: u32,
    pub mic_channels: u16,
    pub cable_name: String,
    pub monitor_name: Option<String>,
}

pub struct Engine {
    cmd_tx: Sender<Command>,
    event_rx: Receiver<Event>,
    next_voice: AtomicU64,
    levels: Arc<Levels>,
    monitor_on: Arc<AtomicBool>,
    params: MixerParams,
    pub info: EngineInfo,
    // Streams stop when dropped; keep them alive for the engine's lifetime.
    _streams: Vec<cpal::Stream>,
}

const RING_SECONDS: f32 = 0.5;
/// Frames fed to the mic resampler per step.
const RESAMPLE_CHUNK: usize = 256;
/// Max drift correction either way (0.5 percent). Real clock drift is a few hundred ppm at most.
const MAX_RATIO_DEVIATION: f64 = 0.005;

impl Engine {
    pub fn start(cfg: EngineConfig) -> Result<Engine> {
        let mic_dev = devices::find(&cfg.mic, DeviceKind::Input).context("mic device")?;
        let cable_dev = devices::find(&cfg.cable, DeviceKind::Output).context("cable device")?;
        let monitor_dev = match &cfg.monitor {
            Some(spec) => Some(devices::find(spec, DeviceKind::Output).context("monitor device")?),
            None => None,
        };

        let mic_cfg = mic_dev.default_input_config().context("mic default config")?;
        let mic_rate = mic_cfg.sample_rate();
        let mic_channels = mic_cfg.channels();

        let name_of = |d: &cpal::Device| {
            d.description()
                .map(|x| x.name().to_string())
                .unwrap_or_else(|_| "?".into())
        };
        let info = EngineInfo {
            mic_name: name_of(&mic_dev),
            mic_rate,
            mic_channels,
            cable_name: name_of(&cable_dev),
            monitor_name: monitor_dev.as_ref().map(name_of),
        };

        let levels = Arc::new(Levels::default());
        let monitor_on = Arc::new(AtomicBool::new(monitor_dev.is_some()));
        let (cmd_tx, cmd_rx) = bounded::<Command>(256);
        let (event_tx, event_rx) = bounded::<Event>(256);

        // Mic ring: interleaved stereo f32 at 48 kHz.
        let ring_frames = (SAMPLE_RATE as f32 * RING_SECONDS) as usize;
        let (mut mic_prod, mut mic_cons) = rtrb::RingBuffer::<f32>::new(ring_frames * CHANNELS);
        let target_fill = ((cfg.mic_buffer_ms / 1000.0) * SAMPLE_RATE as f32) as usize;

        // Monitor ring: clips only.
        let (mut mon_prod, mut mon_cons) = rtrb::RingBuffer::<f32>::new(ring_frames * CHANNELS);

        let mut streams = Vec::new();

        // ---- Input stream -------------------------------------------------------------------
        {
            let levels = levels.clone();
            let event_tx = event_tx.clone();
            let mut stage = MicStage::new(mic_channels as usize, mic_rate, target_fill, levels);
            let stream_cfg: cpal::StreamConfig = mic_cfg.clone().into();
            let err_tx = event_tx.clone();
            let started = std::time::Instant::now();
            let err_fn = move |e: cpal::Error| {
                // WASAPI flags a discontinuity on the very first capture buffer; not a real problem.
                if e.kind() == cpal::ErrorKind::Xrun && started.elapsed() < Duration::from_secs(1) {
                    log::debug!("ignoring startup xrun on mic stream: {e}");
                    return;
                }
                let _ = err_tx.try_send(Event::StreamError {
                    stream: "mic",
                    message: e.to_string(),
                });
            };
            let stream = match mic_cfg.sample_format() {
                cpal::SampleFormat::F32 => build_input::<f32>(
                    &mic_dev,
                    stream_cfg,
                    move |data| stage.push(data, &mut mic_prod, &event_tx),
                    err_fn,
                ),
                cpal::SampleFormat::I16 => build_input::<i16>(
                    &mic_dev,
                    stream_cfg,
                    move |data| stage.push(data, &mut mic_prod, &event_tx),
                    err_fn,
                ),
                cpal::SampleFormat::I32 => build_input::<i32>(
                    &mic_dev,
                    stream_cfg,
                    move |data| stage.push(data, &mut mic_prod, &event_tx),
                    err_fn,
                ),
                cpal::SampleFormat::U16 => build_input::<u16>(
                    &mic_dev,
                    stream_cfg,
                    move |data| stage.push(data, &mut mic_prod, &event_tx),
                    err_fn,
                ),
                other => anyhow::bail!("unsupported mic sample format {other:?}"),
            }
            .context("building mic stream")?;
            streams.push(stream);
        }

        // ---- Output stream (cable) ------------------------------------------------------------
        {
            let levels = levels.clone();
            let monitor_on = monitor_on.clone();
            let event_tx = event_tx.clone();
            let mut mixer = Mixer::new(cfg.max_voices);
            mixer.params = cfg.params;
            let max_block = 4096usize;
            let mut mic_buf = vec![0.0f32; max_block * CHANNELS];
            let mut clip_buf = vec![0.0f32; max_block * CHANNELS];
            let mut warm = false;

            let out_cfg = cpal::StreamConfig {
                channels: CHANNELS as u16,
                sample_rate: SAMPLE_RATE,
                buffer_size: cpal::BufferSize::Default,
            };
            let err_tx = event_tx.clone();
            let stream = cable_dev
                .build_output_stream(
                    out_cfg,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        // Commands first, so a Play lands in this very block.
                        while let Ok(cmd) = cmd_rx.try_recv() {
                            match cmd {
                                Command::Play {
                                    id,
                                    clip,
                                    gain,
                                    looping,
                                } => mixer.play(Voice::new(id, clip, gain, looping)),
                                Command::Stop(id) => mixer.stop(id),
                                Command::StopAll => mixer.stop_all(),
                                Command::SetParams(p) => mixer.params = p,
                                Command::SetMonitor(on) => monitor_on.store(on, Ordering::Relaxed),
                            }
                        }

                        let len = data.len().min(max_block * CHANNELS);
                        let data = &mut data[..len];
                        let mic = &mut mic_buf[..len];
                        let clips = &mut clip_buf[..len];

                        // Pull mic frames; pad with silence if the mic is behind.
                        let (got, rest) = mic_cons.pop_partial_slice(mic);
                        let got_len = got.len();
                        if !rest.is_empty() {
                            rest.fill(0.0);
                            if warm {
                                let _ = event_tx.try_send(Event::MicUnderrun {
                                    missing_frames: rest.len() / CHANNELS,
                                });
                            }
                        } else {
                            warm = true;
                        }
                        let _ = got_len;
                        levels
                            .ring_fill
                            .store((mic_cons.slots() / CHANNELS) as u32, Ordering::Relaxed);

                        mixer.process(mic, data, clips);

                        if monitor_on.load(Ordering::Relaxed) {
                            let _ = mon_prod.push_partial_slice(clips);
                        }

                        for &id in mixer.take_finished() {
                            let _ = event_tx.try_send(Event::VoiceFinished(id));
                        }
                        mixer.clear_finished();

                        levels.mic_peak.store(peak(mic).to_bits(), Ordering::Relaxed);
                        levels.out_peak.store(peak(data).to_bits(), Ordering::Relaxed);
                        levels.callbacks.fetch_add(1, Ordering::Relaxed);
                    },
                    move |e| {
                        let _ = err_tx.try_send(Event::StreamError {
                            stream: "cable",
                            message: e.to_string(),
                        });
                    },
                    None,
                )
                .context("building cable output stream")?;
            streams.push(stream);
        }

        // ---- Monitor stream ------------------------------------------------------------------
        if let Some(mon_dev) = &monitor_dev {
            let out_cfg = cpal::StreamConfig {
                channels: CHANNELS as u16,
                sample_rate: SAMPLE_RATE,
                buffer_size: cpal::BufferSize::Default,
            };
            let err_tx = event_tx.clone();
            let stream = mon_dev
                .build_output_stream(
                    out_cfg,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let (_, rest) = mon_cons.pop_partial_slice(data);
                        rest.fill(0.0);
                    },
                    move |e| {
                        let _ = err_tx.try_send(Event::StreamError {
                            stream: "monitor",
                            message: e.to_string(),
                        });
                    },
                    None,
                )
                .context("building monitor output stream")?;
            streams.push(stream);
        }

        // Start the mic first and let the ring reach its target fill, so the output never starts
        // on an empty buffer and the resampler's warm-up is absorbed before anyone listens.
        streams[0].play().context("starting mic stream")?;
        let deadline = std::time::Instant::now() + Duration::from_millis(1000);
        while (levels.ring_fill.load(Ordering::Relaxed) as usize) < target_fill {
            if std::time::Instant::now() > deadline {
                log::warn!("mic did not deliver {target_fill} frames within 1 s, starting anyway");
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        for s in &streams[1..] {
            s.play().context("starting output stream")?;
        }

        Ok(Engine {
            cmd_tx,
            event_rx,
            next_voice: AtomicU64::new(1),
            levels,
            monitor_on,
            params: cfg.params,
            info,
            _streams: streams,
        })
    }

    pub fn play(&self, clip: Arc<Clip>) -> VoiceId {
        self.play_with(clip, 1.0, false)
    }

    pub fn play_with(&self, clip: Arc<Clip>, gain: f32, looping: bool) -> VoiceId {
        let id = self.next_voice.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Play {
            id,
            clip,
            gain,
            looping,
        });
        id
    }

    pub fn stop(&self, id: VoiceId) {
        self.send(Command::Stop(id));
    }

    pub fn stop_all(&self) {
        self.send(Command::StopAll);
    }

    pub fn params(&self) -> MixerParams {
        self.params
    }

    pub fn set_params(&mut self, params: MixerParams) {
        self.params = params;
        self.send(Command::SetParams(params));
    }

    pub fn set_monitor(&self, on: bool) {
        self.send(Command::SetMonitor(on));
    }

    pub fn monitor_enabled(&self) -> bool {
        self.monitor_on.load(Ordering::Relaxed)
    }

    pub fn try_event(&self) -> Option<Event> {
        self.event_rx.try_recv().ok()
    }

    pub fn levels(&self) -> LevelSnapshot {
        LevelSnapshot {
            mic_peak: f32::from_bits(self.levels.mic_peak.load(Ordering::Relaxed)),
            out_peak: f32::from_bits(self.levels.out_peak.load(Ordering::Relaxed)),
            ring_fill_frames: self.levels.ring_fill.load(Ordering::Relaxed),
            output_callbacks: self.levels.callbacks.load(Ordering::Relaxed),
            drift_ppm: self.levels.ratio_ppm.load(Ordering::Relaxed) as i32,
        }
    }

    fn send(&self, cmd: Command) {
        match self.cmd_tx.try_send(cmd) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log::warn!("engine command queue full, command dropped"),
            Err(TrySendError::Disconnected(_)) => log::error!("engine output stream is gone"),
        }
    }
}

fn build_input<T>(
    device: &cpal::Device,
    cfg: cpal::StreamConfig,
    mut on_data: impl FnMut(&[T]) + Send + 'static,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    device.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| on_data(data),
        err_fn,
        Some(Duration::from_secs(2)),
    )
}

#[inline]
fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Runs in the mic callback: native format -> stereo f32 -> 48 kHz -> ring buffer.
struct MicStage {
    channels: usize,
    nominal_ratio: f64,
    resampler: Async<f32>,
    /// Stereo frames waiting to be resampled.
    stage: Vec<f32>,
    stage_len: usize,
    out: Vec<f32>,
    target_fill: usize,
    levels: Arc<Levels>,
    steps: u32,
    /// Integral term of the drift controller, in normalised fill error.
    integral: f64,
    /// Smoothed ring fill in frames. The raw value jumps by a whole callback block each time
    /// either side runs, which would otherwise feed noise into the controller.
    fill_avg: f64,
}

/// Drift controller gains. Error is (fill - target) / target, evaluated every 16 resample chunks.
const DRIFT_KP: f64 = 0.002;
const DRIFT_KI: f64 = 0.0002;

impl MicStage {
    fn new(channels: usize, rate: u32, target_fill: usize, levels: Arc<Levels>) -> MicStage {
        let nominal_ratio = SAMPLE_RATE as f64 / rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = Async::<f32>::new_sinc(
            nominal_ratio,
            1.0 + MAX_RATIO_DEVIATION * 2.0,
            &params,
            RESAMPLE_CHUNK,
            CHANNELS,
            FixedAsync::Input,
        )
        .expect("resampler construction");
        let out_frames = resampler.output_frames_max();
        MicStage {
            channels,
            nominal_ratio,
            resampler,
            stage: vec![0.0; (RESAMPLE_CHUNK + 4096) * CHANNELS],
            stage_len: 0,
            out: vec![0.0; out_frames * CHANNELS],
            target_fill,
            levels,
            steps: 0,
            integral: 0.0,
            fill_avg: target_fill as f64,
        }
    }

    fn push<T>(&mut self, data: &[T], prod: &mut rtrb::Producer<f32>, events: &Sender<Event>)
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        // Native frames -> stereo f32 into the staging buffer.
        for frame in data.chunks_exact(self.channels) {
            if self.stage_len + CHANNELS > self.stage.len() {
                break; // staging full, drop (should not happen with sane callback sizes)
            }
            let l = f32::from_sample(frame[0]);
            let r = if self.channels >= 2 {
                f32::from_sample(frame[1])
            } else {
                l
            };
            self.stage[self.stage_len] = l;
            self.stage[self.stage_len + 1] = r;
            self.stage_len += CHANNELS;
        }

        // Resample whole chunks.
        let chunk_samples = RESAMPLE_CHUNK * CHANNELS;
        let mut consumed_total = 0;
        while self.stage_len - consumed_total >= chunk_samples {
            let input = &self.stage[consumed_total..consumed_total + chunk_samples];
            let Ok(input) = InterleavedSlice::new(input, CHANNELS, RESAMPLE_CHUNK) else {
                break;
            };
            let out_frames = self.out.len() / CHANNELS;
            let Ok(mut output) = InterleavedSlice::new_mut(&mut self.out, CHANNELS, out_frames)
            else {
                break;
            };
            match self.resampler.process_into_buffer(&input, &mut output, None) {
                Ok((_, produced)) => {
                    let (_, rest) = prod.push_partial_slice(&self.out[..produced * CHANNELS]);
                    if !rest.is_empty() {
                        let _ = events.try_send(Event::MicOverrun {
                            dropped_frames: rest.len() / CHANNELS,
                        });
                    }
                }
                Err(_) => {}
            }
            consumed_total += chunk_samples;
        }
        if consumed_total > 0 {
            self.stage.copy_within(consumed_total..self.stage_len, 0);
            self.stage_len -= consumed_total;
        }

        // Publish the fill level from this side too, so start-up can wait for it.
        let fill_frames = (prod.buffer().capacity() - prod.slots()) / CHANNELS;
        self.levels
            .ring_fill
            .store(fill_frames as u32, Ordering::Relaxed);

        // Drift control (PI): nudge the ratio so the ring buffer settles at target_fill.
        self.fill_avg += 0.05 * (fill_frames as f64 - self.fill_avg);
        self.steps += 1;
        if self.steps % 16 == 0 {
            let target = self.target_fill.max(1) as f64;
            let error = (self.fill_avg - target) / target; // positive = too much queued
            let i_limit = MAX_RATIO_DEVIATION / DRIFT_KI;
            self.integral = (self.integral + error).clamp(-i_limit, i_limit);
            let rel = (1.0 - error * DRIFT_KP - self.integral * DRIFT_KI)
                .clamp(1.0 - MAX_RATIO_DEVIATION, 1.0 + MAX_RATIO_DEVIATION);
            let _ = self.resampler.set_resample_ratio(self.nominal_ratio * rel, true);
            self.levels
                .ratio_ppm
                .store((((rel - 1.0) * 1_000_000.0) as i32) as u32, Ordering::Relaxed);
        }
    }
}
