//! Clip loading: any supported file -> 48 kHz interleaved stereo f32 in memory.
//!
//! Decoding and resampling happen once at load time so the audio callback only ever copies.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::{CHANNELS, SAMPLE_RATE};

/// Longest file we are willing to decode into RAM. A soundboard clip is seconds, not an album.
pub const MAX_CLIP_SECS: f32 = 600.0;

/// What a file looks like from its headers, without decoding it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClipInfo {
    /// Known when the container reports a frame count (MP3 with Xing header, WAV, FLAC, MP4).
    pub duration_secs: Option<f32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<usize>,
    pub codec: String,
}

/// A fully decoded clip. Cheap to clone, the samples are shared.
#[derive(Debug, Clone)]
pub struct Clip {
    pub name: String,
    pub path: Option<PathBuf>,
    /// Interleaved stereo at [`SAMPLE_RATE`].
    pub samples: Arc<[f32]>,
    pub source_rate: u32,
    pub source_channels: usize,
}

impl Clip {
    /// Read container and codec headers only. Milliseconds, no audio decoded.
    pub fn probe(path: impl AsRef<Path>) -> Result<ClipInfo> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::default();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .context("unrecognised container format")?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("no audio track"))?;
        let params = match &track.codec_params {
            Some(CodecParameters::Audio(p)) => Some(p),
            _ => None,
        };
        let sample_rate = params.and_then(|p| p.sample_rate);
        let channels = params.and_then(|p| p.channels.as_ref().map(|c| c.count()));
        let duration_secs = match (track.num_frames, sample_rate) {
            (Some(n), Some(sr)) if sr > 0 => Some(n as f32 / sr as f32),
            _ => None,
        };
        Ok(ClipInfo {
            duration_secs,
            sample_rate,
            channels,
            codec: params.map(|p| format!("{:?}", p.codec)).unwrap_or_default(),
        })
    }

    /// Decode a file from disk. Format is sniffed from content, the extension is only a hint.
    pub fn load(path: impl AsRef<Path>) -> Result<Clip> {
        let path = path.as_ref();
        if let Ok(info) = Clip::probe(path) {
            if let Some(d) = info.duration_secs {
                if d > MAX_CLIP_SECS {
                    bail!(
                        "{} is {:.0} minutes long, longer than the {:.0} minute clip limit",
                        path.display(),
                        d / 60.0,
                        MAX_CLIP_SECS / 60.0
                    );
                }
            }
        }
        let (raw, rate, channels) = match decode(path) {
            Ok(d) => d,
            #[cfg(windows)]
            Err(e) => {
                log::info!(
                    "{}: symphonia could not decode it ({e:#}), trying Media Foundation",
                    path.display()
                );
                crate::mf::decode(path).with_context(|| {
                    format!("decoding {} (symphonia: {e:#}; Media Foundation also failed)", path.display())
                })?
            }
            #[cfg(not(windows))]
            Err(e) => return Err(e).with_context(|| format!("decoding {}", path.display())),
        };
        if raw.is_empty() {
            bail!("{} contains no audio", path.display());
        }
        let stereo = to_stereo(&raw, channels);
        let samples = if rate == SAMPLE_RATE {
            stereo
        } else {
            resample(&stereo, rate).with_context(|| format!("resampling {}", path.display()))?
        };
        Ok(Clip {
            name: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path: Some(path.to_path_buf()),
            samples: samples.into(),
            source_rate: rate,
            source_channels: channels,
        })
    }

    /// Build a clip from interleaved stereo samples already at 48 kHz.
    pub fn from_samples(name: impl Into<String>, samples: Vec<f32>) -> Clip {
        Clip {
            name: name.into(),
            path: None,
            samples: samples.into(),
            source_rate: SAMPLE_RATE,
            source_channels: CHANNELS,
        }
    }

    /// A decaying sine beep, for testing routing without any files.
    pub fn tone(freq_hz: f32, seconds: f32) -> Clip {
        let frames = (seconds * SAMPLE_RATE as f32) as usize;
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let t = i as f32 / SAMPLE_RATE as f32;
            let s = 0.4 * (2.0 * std::f32::consts::PI * freq_hz * t).sin() * (-3.0 * t).exp();
            samples.push(s);
            samples.push(s);
        }
        Clip::from_samples(format!("tone {freq_hz:.0} Hz"), samples)
    }

    /// Loudness-normalise in place: bring the gated RMS to `target_rms` (linear, e.g. 0.125 for
    /// -18 dBFS) without letting the peak exceed `peak_ceiling`. Silence-ish windows (below
    /// -45 dBFS) are ignored when measuring, so a mostly quiet clip with one loud burst is
    /// judged by the burst, not pulled up into distortion.
    pub fn normalize(&mut self, target_rms: f32, peak_ceiling: f32) -> f32 {
        let gain = normalization_gain(&self.samples, target_rms, peak_ceiling);
        if (gain - 1.0).abs() > 1e-3 {
            let mut v: Vec<f32> = self.samples.to_vec();
            for s in &mut v {
                *s *= gain;
            }
            self.samples = v.into();
        }
        gain
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    pub fn duration_secs(&self) -> f32 {
        self.frames() as f32 / SAMPLE_RATE as f32
    }

    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }
}

/// Default normalisation target: -16 dBFS RMS over the non-silent parts.
pub const DEFAULT_TARGET_RMS: f32 = 0.158;
/// Default peak ceiling after normalisation (-0.5 dBFS). The mixer's soft clipper sits above it.
pub const DEFAULT_PEAK_CEILING: f32 = 0.94;

/// Gain that brings the gated RMS of `samples` to `target_rms`, capped so the peak stays
/// at or below `peak_ceiling`. Returns 1.0 for silent input.
pub fn normalization_gain(samples: &[f32], target_rms: f32, peak_ceiling: f32) -> f32 {
    const WINDOW: usize = 4800; // 100 ms of stereo frames
    const GATE: f32 = 0.0056; // about -45 dBFS RMS
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for w in samples.chunks(WINDOW * CHANNELS) {
        let e: f64 = w.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (e / w.len() as f64).sqrt() as f32;
        if rms > GATE {
            sum += e;
            count += w.len();
        }
    }
    if count == 0 {
        return 1.0;
    }
    let rms = (sum / count as f64).sqrt() as f32;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if rms <= 0.0 || peak <= 0.0 {
        return 1.0;
    }
    (target_rms / rms).min(peak_ceiling / peak)
}

/// Returns (interleaved samples, sample rate, channel count).
fn decode(path: &Path) -> Result<(Vec<f32>, u32, usize)> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::default();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("unrecognised container format")?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => bail!("track has no audio codec parameters"),
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("unsupported codec")?;

    let mut out: Vec<f32> = Vec::new();
    let mut chunk: Vec<f32> = Vec::new();
    let mut rate = None;
    let mut channels = None;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                rate.get_or_insert(spec.rate());
                channels.get_or_insert(spec.channels().count());
                chunk.clear();
                buf.copy_to_vec_interleaved(&mut chunk);
                out.extend_from_slice(&chunk);
            }
            Err(SymError::DecodeError(msg)) => {
                log::warn!("{}: skipped a bad packet ({msg})", path.display());
            }
            Err(SymError::ResetRequired) => decoder.reset(),
            Err(e) => return Err(e.into()),
        }
    }

    let rate = rate.ok_or_else(|| anyhow!("decoder produced no frames"))?;
    let channels = channels.unwrap_or(1).max(1);
    Ok((out, rate, channels))
}

/// Mono is duplicated, anything above stereo keeps the first two channels.
fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        2 => samples.to_vec(),
        1 => samples.iter().flat_map(|&s| [s, s]).collect(),
        n => samples
            .chunks_exact(n)
            .flat_map(|frame| [frame[0], frame[1]])
            .collect(),
    }
}

/// Offline, high quality resample of an interleaved stereo buffer to [`SAMPLE_RATE`].
fn resample(stereo: &[f32], from_rate: u32) -> Result<Vec<f32>> {
    let frames = stereo.len() / CHANNELS;
    let mut resampler = Fft::<f32>::new(
        from_rate as usize,
        SAMPLE_RATE as usize,
        1024,
        CHANNELS,
        FixedSync::Input,
    )?;
    let input = InterleavedSlice::new(stereo, CHANNELS, frames)?;
    let output = resampler.process_all(&input, frames, None)?;
    Ok(output.take_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_is_stereo_48k() {
        let c = Clip::tone(440.0, 0.5);
        assert_eq!(c.frames(), 24_000);
        assert!((c.duration_secs() - 0.5).abs() < 1e-6);
        assert!(c.peak() <= 0.4 && c.peak() > 0.3);
    }

    #[test]
    fn normalize_raises_quiet_clip_and_respects_ceiling() {
        // quiet sine at 0.05 peak -> rms 0.035; target 0.125 wants gain 3.5, peak allows 0.94/0.05
        let mut c = Clip::tone(440.0, 0.2);
        let mut v: Vec<f32> = c.samples.to_vec();
        for s in &mut v {
            *s *= 0.05 / 0.4;
        }
        c.samples = v.into();
        let before = c.peak();
        let g = c.normalize(DEFAULT_TARGET_RMS, DEFAULT_PEAK_CEILING);
        assert!(g > 1.5, "gain {g}");
        assert!(c.peak() > before);
        assert!(c.peak() <= DEFAULT_PEAK_CEILING + 1e-4);

        // loud square-ish clip: peak 0.9, rms 0.9 -> would need gain < 1, ceiling not the limit
        let mut loud = Clip::from_samples("loud", vec![0.9; 9600]);
        let g2 = loud.normalize(DEFAULT_TARGET_RMS, DEFAULT_PEAK_CEILING);
        assert!((g2 - DEFAULT_TARGET_RMS / 0.9).abs() < 1e-3, "gain {g2}");

        // silence stays silence, gain 1
        let mut quiet = Clip::from_samples("quiet", vec![0.0; 9600]);
        assert_eq!(quiet.normalize(DEFAULT_TARGET_RMS, DEFAULT_PEAK_CEILING), 1.0);
    }

    #[test]
    fn mono_becomes_stereo() {
        let s = to_stereo(&[0.1, 0.2], 1);
        assert_eq!(s, vec![0.1, 0.1, 0.2, 0.2]);
    }

    #[test]
    fn resample_keeps_duration() {
        // one second of 44.1 kHz silence with a single spike
        let mut mono = vec![0.0f32; 44_100];
        mono[22_050] = 1.0;
        let stereo = to_stereo(&mono, 1);
        let out = resample(&stereo, 44_100).unwrap();
        let frames = out.len() / CHANNELS;
        assert!((frames as i64 - 48_000).abs() < 64, "got {frames} frames");
        let peak_at = (0..frames)
            .max_by(|&a, &b| out[a * 2].abs().partial_cmp(&out[b * 2].abs()).unwrap())
            .unwrap();
        assert!((peak_at as i64 - 24_000).abs() < 64, "peak at {peak_at}");
    }
}
