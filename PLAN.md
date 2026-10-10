# Hotmic: play MP3s into any call while your own voice keeps working

Status: plan only, nothing built yet. Date: 2026-10-11.

## 1. What we are building

A Windows desktop app that:

- Lets you add any number of audio files (MP3 first, then WAV/OGG/FLAC for free) and organise them.
- Plays a clip on click or global hotkey into the microphone that Discord, WhatsApp, Zoom etc. are listening to.
- Keeps your real microphone live the whole time. Voice and clip are mixed, not swapped.
- Optionally also plays the clip into your own headphones so you hear what the others hear.
- Has no file limit and no subscription. Local only, open source friendly.

## 2. How "playing through the mic" actually works on Windows

Windows does not let an ordinary program write audio into a real microphone. Every soundboard
does one of three things:

| Approach | How it works | Used by | Cost to us |
|---|---|---|---|
| A. Virtual audio cable + user-mode mixer | Install a virtual sound device. Our app captures your real mic, mixes in clips, and plays the result into the cable's *input*. Discord picks the cable's *output* as its mic. | Soundux, Resanance, CommonMik, most free boards | Low. Pure application code. Depends on one third-party driver (VB-Cable). |
| B. Our own virtual microphone driver | Kernel-mode audio driver (Microsoft SYSVAD sample) that shows up as a mic. | Virtual Audio Cable, VoiceMeeter, Micser | High. Windows 10 1607+ refuses unsigned kernel drivers. Needs an EV code-signing cert plus Microsoft Attestation signing, or test-signing mode on your own PC. |
| C. Audio Processing Object (APO) on the real mic | A DLL that Windows inserts into the mic's capture pipeline and that mixes our audio in. | Soundpad (installs a "driver extension", needs a reboot) | High. Still ships as a signed driver package, poorly documented for this use, fragile across Windows updates. |

Decision: **A for v1**. It is how every working open-source soundboard does it. B stays on the
roadmap as an optional v4 to remove the VB-Cable dependency.

Virtual cable choice: **VB-Cable** by VB-Audio. Donationware, free to use with all functions for
personal use, driver already Microsoft-signed, one cable is all we need (our app to cable to Discord).
Do not bundle its installer: distribution over 10 units needs an agreement with VB-Audio. Detect
whether it is installed and link to the download instead.

### Signal path (v1)

```
Real mic ----WASAPI capture----+
                               +--> Mixer (48 kHz, stereo, f32) --> WASAPI render --> "CABLE Input"
Decoded clips (RAM) -----------+                                                         |
                               +--> optional monitor --> your headphones                 v
                                                                      Discord mic = "CABLE Output"
```

Important: we do the mic passthrough ourselves inside the app. We do **not** use the Windows
"Listen to this device" trick that the free boards tell users to set up. That adds latency, goes
through the Windows mixer, and keeps running when the app is closed.

## 3. Tech stack

Recommended: **Rust core + Tauri 2 UI**.

| Piece | Crate / tool | Why |
|---|---|---|
| Audio I/O | `cpal` 0.18 (WASAPI backend) | Device enumeration, callback-based input and output streams. |
| Decoding | `symphonia` 0.6 with the `mp3` feature enabled (MP3 is not on by default) | Pure Rust, gapless MP3, also gives WAV/OGG/FLAC/AAC for free. Decode fully to f32 PCM at load time, keep clips in RAM. |
| Resampling | `rubato` 5 | Sync resampler to bring clips to 48 kHz at load. Async resampler for mic clock drift (mic hardware clock vs. cable clock drift over long calls). |
| Lock-free buffers | `ringbuf` | Mic callback writes, output callback reads. No locks or allocation in audio callbacks. |
| UI shell | Tauri 2 | Small binary, web UI (any framework you like), Rust backend in the same process as the audio engine. |
| Global hotkeys | `tauri-plugin-global-shortcut` | Works while Discord or a game has focus. Needs Rust >= 1.77.2. |
| Config | `serde` + JSON in `%APPDATA%\hotmic\` | Library, hotkeys, device choices, volumes. |

Pragmatic alternative if you would rather ship fast than learn Rust audio: **C# + NAudio** (WPF or
WinUI 3). NAudio's `WasapiCapture` into `BufferedWaveProvider` into `MixingSampleProvider` into
`WasapiOut` is almost exactly this pipeline out of the box, and `AudioFileReader` handles MP3 via
Media Foundation. Everything in sections 4 to 6 applies unchanged.

Spike language: **Python** (`sounddevice` + `numpy` + `pydub`/`ffmpeg`). Only for Phase 0.

## 4. Phases

### Phase 0: prove the routing (half a day)

Goal: hear your voice plus an MP3 in a Discord call from a second account or phone.

1. Install VB-Cable. In Windows Sound settings set both "CABLE Input" and "CABLE Output" to
   48000 Hz, 2 channels (Discord runs at 48 kHz, matching avoids a resample hop).
2. Python script: open mic input stream and "CABLE Input" output stream with `sounddevice`
   at 48 kHz. In the callback add mic frames to the current position of a pre-decoded MP3 array,
   clip to [-1, 1], write out. Space bar triggers a clip.
3. Discord: Input Device = "CABLE Output". **Turn off Noise Suppression (Krisp) and Echo
   Cancellation** in Voice settings. Krisp treats music as noise and will mangle or mute clips.
   Same for any other client with "noise suppression".
4. Measure: speak and clap, note the delay. Target under 60 ms mic-to-cable. Note any drift or
   crackle after 10 minutes.

Exit criterion: the second account hears both voice and clip cleanly. If this fails, nothing
downstream matters.

### Phase 1: audio engine as a Rust library crate (1 to 2 weeks)

`hotmic-core`, no UI, driven by a tiny CLI for testing.

- `devices`: list capture and render devices with stable IDs, find VB-Cable by name, remember
  user picks, handle device unplug/replug.
- `clip`: load file, symphonia decode, rubato resample to 48 kHz stereo f32, store as `Arc<[f32]>`.
  Keep duration, peak level, optional trim start/end.
- `voice`: a playing instance of a clip (position, gain, fade-out flag). Multiple voices at once.
- `mixer`: real-time safe. Input: mic ring buffer + active voices. Output: one f32 frame buffer.
  Soft clip or limiter so stacked clips do not hard-clip. Commands (play, stop, stop-all, set
  gain) arrive through a lock-free channel, never via mutex in the callback.
- `engine`: owns the cpal streams. Two render streams if monitoring is on (cable + headphones).
  Mic drift handling: watch ring buffer fill level, nudge rubato ratio by a few ppm.
- Options: mic gain, clip gain, "duck mic while clip plays" (lower, not mute), "block voice" mode
  (clip replaces voice, like Soundpad), monitor on/off.
- Tests: offline mixer tests with synthetic signals, decode tests on a handful of real MP3s
  (VBR, mono, 44.1 kHz, ID3-heavy files).

### Phase 2: Tauri app (1 to 2 weeks)

- Library: add files or whole folders, drag and drop, folders/tabs, search, rename, colour tag.
  Watch folders for new files.
- Buttons grid with play progress, per-clip volume, loop toggle, right-click to assign hotkey.
- Global hotkeys per clip plus a Stop All key and a Push-To-Play key.
- Settings: mic device, cable device, monitor device, master gains, duck amount, start minimised
  to tray, autostart with Windows.
- First-run wizard: detect VB-Cable, link to download if missing, show the two Discord settings
  to change, run a loopback self-test (play tone into cable, capture from cable, confirm it
  arrives).
- Live meters for mic and output so you can see it working.

### Phase 3: polish (ongoing)

- Waveform view with trim handles, fade in/out per clip.
- Import from the folders of the paid app you use today.
- Profiles (one library for Discord, one for streaming).
- Optional: capture another app's audio with WASAPI process loopback (Windows 10 2004+) so
  YouTube or Spotify can be sent into the call too. CommonMik does exactly this.
- Installer via `tauri-bundler` (MSI/NSIS), winget manifest.

### Phase 4 (optional, later): own virtual microphone

Remove the VB-Cable dependency by shipping our own virtual mic driver built from the SYSVAD sample.
Only worth it if you want zero third-party installs. Signing paths:

- Personal use only: enable test-signing mode on your PC (`bcdedit /set testsigning on`). Adds a
  desktop watermark and weakens driver security. Fine for yourself, not for sharing.
- Sharing: EV code-signing certificate (hundreds of dollars a year) plus Microsoft Hardware Dev
  Center Attestation signing. WHQL is not required for x64 Windows 10/11 desktop.

## 5. Known gotchas (collected from the research)

- **Discord Krisp noise suppression kills music.** Must be off. Document it in the wizard.
- **Feedback loop** if monitor output goes to speakers near the mic. Default monitor to off, warn
  when the monitor device is not headphones.
- **Sample-rate mismatch** between the cable, the mic, and Discord causes drift or Windows
  resampling. Set the cable to 48 kHz once and capture at the mic's native rate, resample inside.
- **Two clocks.** Mic hardware and the virtual cable run on different clocks. Over a long call
  the ring buffer slowly fills or drains. Handle with rubato async ratio nudging, or accept a
  rare dropped/duplicated frame. Do not ignore it.
- **No blocking in audio callbacks.** No file I/O, no allocation, no mutex. Decode before play.
- **VB-Cable is "two apps only".** That is exactly our topology (us to Discord), so fine. If you
  later want more routing, that is what VoiceMeeter is for, or Phase 4.
- **WhatsApp desktop** uses the Windows default communications mic. Either set "CABLE Output" as
  default communications device or pick it inside the app if it allows.
- **Exclusive mode.** Stay in WASAPI shared mode. Exclusive mode blocks other apps and gains little
  for voice chat.

## 6. Suggested first steps

1. Install VB-Cable, set 48 kHz on both cable endpoints.
2. Run the Phase 0 Python spike and test in a real call.
3. `cargo new hotmic-core --lib`, implement clip loading and a CLI that plays one MP3 into
   "CABLE Input" while passing the mic through.
4. Only then start the Tauri UI.

## 7. Sources consulted

- Soundpad product page and Steam thread on its driver extension and recording-device requirement.
- Soundux GitHub (GPL-3, plays to VB-Cable on Windows, installer bundles VB-Cable).
- Resanance and other free boards: all rely on a virtual cable plus "Listen to this device".
- CommonMik GitHub: WASAPI process loopback + mic mixed into VB-Cable.
- VB-Audio licensing and terms pages (donationware, 10+ unit distribution needs agreement).
- Microsoft docs: kernel-mode code signing, Hardware Dev Center attestation, APO overview.
- cpal 0.18, symphonia 0.6, rubato 5 docs; Tauri 2 global-shortcut plugin docs; NAudio author's
  blog on MixingSampleProvider and WASAPI resampling.
