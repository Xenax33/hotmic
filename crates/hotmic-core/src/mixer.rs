//! Real-time safe mixer. Runs inside the output audio callback.
//!
//! Rules in here: no allocation after construction, no locks, no I/O, no panics on bad input.

use std::sync::Arc;

use crate::clip::Clip;
use crate::CHANNELS;

pub type VoiceId = u64;

/// Frames over which a stopped voice fades out instead of cutting.
pub const STOP_FADE_FRAMES: usize = 480; // 10 ms

#[derive(Debug)]
pub struct Voice {
    pub id: VoiceId,
    clip: Arc<Clip>,
    pos: usize,
    gain: f32,
    looping: bool,
    /// Current fade multiplier and per-frame decrement, set when stopping.
    fade: Option<(f32, f32)>,
}

impl Voice {
    pub fn new(id: VoiceId, clip: Arc<Clip>, gain: f32, looping: bool) -> Voice {
        Voice {
            id,
            clip,
            pos: 0,
            gain,
            looping,
            fade: None,
        }
    }

    pub fn progress(&self) -> f32 {
        let frames = self.clip.frames().max(1);
        self.pos as f32 / frames as f32
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MixerParams {
    pub mic_gain: f32,
    pub clip_gain: f32,
    /// Multiplier applied to the mic while any voice is playing. 1.0 = no ducking.
    pub duck: f32,
    /// If true the mic is muted while a voice plays ("block voice" mode).
    pub block_voice: bool,
}

impl Default for MixerParams {
    fn default() -> Self {
        MixerParams {
            mic_gain: 1.0,
            clip_gain: 0.8,
            duck: 1.0,
            block_voice: false,
        }
    }
}

pub struct Mixer {
    pub params: MixerParams,
    voices: Vec<Voice>,
    finished: Vec<VoiceId>,
    max_voices: usize,
}

impl Mixer {
    pub fn new(max_voices: usize) -> Mixer {
        Mixer {
            params: MixerParams::default(),
            voices: Vec::with_capacity(max_voices),
            finished: Vec::with_capacity(max_voices),
            max_voices,
        }
    }

    /// Start a voice. If the voice table is full the oldest voice is replaced.
    pub fn play(&mut self, voice: Voice) {
        if self.voices.len() >= self.max_voices {
            self.voices.remove(0);
        }
        self.voices.push(voice);
    }

    pub fn stop(&mut self, id: VoiceId) {
        if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
            v.fade.get_or_insert((1.0, 1.0 / STOP_FADE_FRAMES as f32));
        }
    }

    pub fn stop_all(&mut self) {
        for v in &mut self.voices {
            v.fade.get_or_insert((1.0, 1.0 / STOP_FADE_FRAMES as f32));
        }
    }

    pub fn active(&self) -> usize {
        self.voices.len()
    }

    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    /// Voices that ended since the last call. Cleared on read.
    pub fn take_finished(&mut self) -> &[VoiceId] {
        // Caller copies these out before the next process(); we just expose the slice.
        &self.finished
    }

    pub fn clear_finished(&mut self) {
        self.finished.clear();
    }

    /// Mix one block.
    ///
    /// - `mic`: interleaved stereo frames from the microphone, already at 48 kHz.
    /// - `out`: interleaved stereo frames going to the cable. Same length as `mic`.
    /// - `clips_only`: receives just the mixed clips (for the monitor output). Same length.
    pub fn process(&mut self, mic: &[f32], out: &mut [f32], clips_only: &mut [f32]) {
        let frames = out.len() / CHANNELS;
        debug_assert_eq!(mic.len(), out.len());
        debug_assert_eq!(clips_only.len(), out.len());

        clips_only.fill(0.0);
        let had_voices = !self.voices.is_empty();

        // Sum voices into clips_only.
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            let mut done = false;
            let mut written = 0;
            while written < frames {
                let remaining = v.clip.frames().saturating_sub(v.pos);
                if remaining == 0 {
                    if v.looping && v.fade.is_none() && v.clip.frames() > 0 {
                        v.pos = 0;
                        continue;
                    }
                    done = true;
                    break;
                }
                let n = remaining.min(frames - written);
                let src = &v.clip.samples[v.pos * CHANNELS..(v.pos + n) * CHANNELS];
                let dst = &mut clips_only[written * CHANNELS..(written + n) * CHANNELS];
                match &mut v.fade {
                    None => {
                        for (d, s) in dst.iter_mut().zip(src) {
                            *d += s * v.gain;
                        }
                    }
                    Some((level, step)) => {
                        for (frame_dst, frame_src) in
                            dst.chunks_exact_mut(CHANNELS).zip(src.chunks_exact(CHANNELS))
                        {
                            let g = v.gain * *level;
                            for (d, s) in frame_dst.iter_mut().zip(frame_src) {
                                *d += s * g;
                            }
                            *level -= *step;
                            if *level <= 0.0 {
                                *level = 0.0;
                                done = true;
                            }
                        }
                        if done {
                            // Fade finished mid-block; stop writing this voice.
                            v.pos += n;
                            written += n;
                            break;
                        }
                    }
                }
                v.pos += n;
                written += n;
            }
            if done {
                let v = self.voices.remove(i);
                if self.finished.len() < self.finished.capacity() {
                    self.finished.push(v.id);
                }
            } else {
                i += 1;
            }
        }

        // Mic path.
        let p = self.params;
        let voices_active = had_voices || !self.voices.is_empty();
        let mic_mult = if voices_active {
            if p.block_voice {
                0.0
            } else {
                p.mic_gain * p.duck
            }
        } else {
            p.mic_gain
        };

        for ((o, &m), &c) in out.iter_mut().zip(mic).zip(clips_only.iter()) {
            *o = soft_clip(m * mic_mult + c * p.clip_gain);
        }
    }
}

/// Gentle limiter: linear below 0.8, then a smooth knee so stacked clips never hard clip.
#[inline]
fn soft_clip(x: f32) -> f32 {
    const T: f32 = 0.8;
    let a = x.abs();
    if a <= T {
        x
    } else {
        let over = a - T;
        let y = T + (1.0 - T) * (over / ((1.0 - T) + over));
        y.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(frames: usize, value: f32) -> Arc<Clip> {
        Arc::new(Clip::from_samples("c", vec![value; frames * CHANNELS]))
    }

    #[test]
    fn mic_passes_through_when_idle() {
        let mut m = Mixer::new(4);
        let mic = vec![0.25f32; 8];
        let mut out = vec![0.0; 8];
        let mut clips = vec![0.0; 8];
        m.process(&mic, &mut out, &mut clips);
        assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-6));
        assert!(clips.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn voice_mixes_and_finishes() {
        let mut m = Mixer::new(4);
        m.params.clip_gain = 1.0;
        m.play(Voice::new(7, clip(3, 0.5), 1.0, false));
        let mic = vec![0.0f32; 8]; // 4 frames
        let mut out = vec![0.0; 8];
        let mut clips = vec![0.0; 8];
        m.process(&mic, &mut out, &mut clips);
        assert_eq!(&out[..6], &[0.5; 6]);
        assert_eq!(&out[6..], &[0.0; 2]);
        assert_eq!(m.active(), 0);
        assert_eq!(m.take_finished(), &[7]);
    }

    #[test]
    fn looping_voice_wraps() {
        let mut m = Mixer::new(4);
        m.params.clip_gain = 1.0;
        m.play(Voice::new(1, clip(2, 0.5), 1.0, true));
        let mic = vec![0.0f32; 10];
        let mut out = vec![0.0; 10];
        let mut clips = vec![0.0; 10];
        m.process(&mic, &mut out, &mut clips);
        assert!(out.iter().all(|&s| s == 0.5));
        assert_eq!(m.active(), 1);
    }

    #[test]
    fn stop_fades_and_removes() {
        let mut m = Mixer::new(4);
        m.params.clip_gain = 1.0;
        m.play(Voice::new(1, clip(10_000, 0.5), 1.0, false));
        m.stop(1);
        let mic = vec![0.0f32; STOP_FADE_FRAMES * CHANNELS * 2];
        let mut out = mic.clone();
        let mut clips = mic.clone();
        m.process(&mic, &mut out, &mut clips);
        assert!(out[0] > 0.49 && out[0] <= 0.5);
        assert_eq!(m.active(), 0);
        assert!(out.last().unwrap().abs() < 1e-6);
    }

    #[test]
    fn ducking_lowers_mic_while_playing() {
        let mut m = Mixer::new(4);
        m.params.duck = 0.5;
        m.params.clip_gain = 0.0;
        m.play(Voice::new(1, clip(100, 0.0), 1.0, false));
        let mic = vec![0.4f32; 8];
        let mut out = vec![0.0; 8];
        let mut clips = vec![0.0; 8];
        m.process(&mic, &mut out, &mut clips);
        assert!(out.iter().all(|&s| (s - 0.2).abs() < 1e-6));
    }

    #[test]
    fn soft_clip_is_bounded_and_monotonic() {
        let mut last = -2.0;
        for i in -40..=40 {
            let x = i as f32 / 10.0;
            let y = soft_clip(x);
            assert!(y.abs() < 1.0);
            assert!(y >= last);
            last = y;
        }
        assert_eq!(soft_clip(0.5), 0.5);
    }
}
