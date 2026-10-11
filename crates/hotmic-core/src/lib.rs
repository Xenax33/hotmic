//! Hotmic audio engine.
//!
//! Captures the real microphone, mixes clips on top of it, and renders the result to an output
//! device (normally the virtual cable that the call app uses as its microphone). Optionally also
//! renders the clips alone to a monitor device so the user hears what the other side hears.
//!
//! Layout:
//! - [`devices`]: enumerate and pick audio devices.
//! - [`clip`]: decode any audio file to 48 kHz stereo f32 in memory.
//! - [`mixer`]: real-time safe mixing of mic + voices. No allocation, no locks.
//! - [`engine`]: owns the cpal streams and the ring buffers that connect them.

pub mod clip;
pub mod devices;
pub mod engine;
#[cfg(windows)]
pub mod mf;
pub mod mixer;

pub use clip::Clip;
pub use devices::{DeviceInfo, DeviceKind};
pub use engine::{Command, Engine, EngineConfig, Event, LevelSnapshot};
pub use mixer::VoiceId;

/// Everything inside the engine runs at this rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// Everything inside the engine is interleaved stereo.
pub const CHANNELS: usize = 2;
