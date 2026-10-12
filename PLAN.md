# Hotmic: play MP3s into any call while your own voice keeps working

Status: plan only, nothing built yet. Date: 2026-10-11.

## 1. What we are building

A Windows desktop app that:

- Lets you add any number of audio files (MP3 first, then WAV/OGG/FLAC for free) and organise them.
- Plays a clip on click or global hotkey into the microphone that Discord, WhatsApp, Zoom etc. are listening to.
- Keeps your real microphone live the whole time. Voice and clip are mixed, not swapped.
- Optionally also plays the clip into your own headphones so you hear what the others hear.
- Imports clips straight from a YouTube URL: paste a link, optionally pick a time range, get an
  MP3 in your collection. See section 7.
- Binds any key (plain `1`, `2`, `3` or combos) to a clip, with a layer system so bare keys do
  not break typing in other apps. See section 8.
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
- Global hotkeys per clip plus a Stop All key and a Push-To-Play key. Full design in section 8.
- YouTube import box in the library view (section 7). Ships in this phase because it is the
  main way clips get into the collection.
- Settings: mic device, cable device, monitor device, master gains, duck amount, start minimised
  to tray, autostart with Windows.
- First-run wizard: detect VB-Cable, link to download if missing, show the two Discord settings
  to change, run a loopback self-test (play tone into cable, capture from cable, confirm it
  arrives).
- Live meters for mic and output so you can see it working.

### Phase 3: from dev build to daily driver (agreed order, 2026-10-12)

Done so far in Phase 2: devices, grid, gains, meters, loudness normalisation, global hotkeys
(combo mode), replace/layer playback, Media Foundation decoder fallback, YouTube import with
yt-dlp + Deno, non-destructive trim.

1. **UI redesign.** Done 2026-10-12, "Quiet" skin (section 11).
2. **Tray mode and startup.** Done 2026-10-12: close to tray, tray menu, single instance,
   start with Windows with optional start-minimised.
3. **Installer.** NSIS setup exe builds (`cargo tauri build`, 4 MB). Still to do: GitHub
   Releases workflow, code signing (unsigned exe triggers SmartScreen), winget, auto-update.
4. **First-run wizard.** Done 2026-10-12: VB-Cable check, default-output check, engine
   self-test that plays a tone and measures CABLE Output, Discord step. Re-runnable from
   Settings.
5. **Auto push-to-talk.** Done 2026-10-12: Settings toggle plus key capture (keyboard key or
   mouse side button), SendInput hold while any voice is active, 200 ms release tail, key
   released on exit. Verified with GetAsyncKeyState during a hotkey-triggered clip.
   Also done: `.github/workflows/release.yml` builds the NSIS installer on a `v*` tag and
   publishes a GitHub Release with SHA256SUMS; `ci.yml` runs tests on every push.
6. **Layer-key and hot-mode hotkeys.** Bare keys that only fire while a layer key is held or hot
   mode is toggled; low-level keyboard hook.
7. **Clip polish.** Waveform with drag handles for trim, fade in/out, thumbnails on imported
   tiles, folders or tags, search by tag.
8. **Built-in noise suppression.** RNNoise on the mic inside Hotmic so Discord's Krisp can be off
   without a noisy voice; echo cancellation later.
9. Then the rest of section 9 (instant replay, phone remote, sound packs, TTS, profiles).

### Phase 4 (optional, later): own virtual microphone

Remove the VB-Cable dependency by shipping our own virtual mic driver built from the SYSVAD sample.
Only worth it if you want zero third-party installs. Signing paths:

- Personal use only: enable test-signing mode on your PC (`bcdedit /set testsigning on`). Adds a
  desktop watermark and weakens driver security. Fine for yourself, not for sharing.
- Sharing: EV code-signing certificate (hundreds of dollars a year) plus Microsoft Hardware Dev
  Center Attestation signing. WHQL is not required for x64 Windows 10/11 desktop.

## 5. Known gotchas (collected from the research)

- **Discord Krisp noise suppression kills music.** Pure tones vanish completely, speech clips
  mostly survive, music gets thinned. Confirmed 2026-10-11 with real clips.
- **Discord voice activity gating cuts clip parts even with Krisp off.** Discord's VAD uses
  Krisp's voice detector (`vadUseKrisp`), so non-speech sections are muted mid-clip. Fix is
  push-to-talk held during playback (auto-PTT feature) or Hotmic doing the mic cleanup itself.
- **Feedback loop** if monitor output goes to speakers near the mic. Default monitor to off, warn
  when the monitor device is not headphones.
- **Sample-rate mismatch** between the cable, the mic, and Discord causes drift or Windows
  resampling. Set the cable to 48 kHz once and capture at the mic's native rate, resample inside.
- **Two clocks.** Mic hardware and the virtual cable run on different clocks. Over a long call
  the ring buffer slowly fills or drains. Handle with rubato async ratio nudging, or accept a
  rare dropped/duplicated frame. Do not ignore it.
- **No blocking in audio callbacks.** No file I/O, no allocation, no mutex. Decode before play.
- **"MP3" files that are really HE-AAC.** Clips ripped from videos often carry an .mp3 extension
  over an MP4/AAC container, and symphonia only decodes AAC-LC. The engine falls back to Windows
  Media Foundation (`hotmic-core/src/mf.rs`), which decodes HE-AAC, WMA and anything else the OS
  can play. Found 2026-10-11 in the user's own library.
- **Never decode on the UI thread.** The first folder import froze the app for minutes. File work
  runs in blocking tasks, listing uses header probes only, decoded audio sits in a bounded cache.
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

## 7. Feature design: YouTube import

User flow: paste a URL (or drag a link onto the window), see title and duration, optionally set
start and end times, click Import. A progress bar runs, and the clip appears in the library with
the video title as its name and the thumbnail as its button art.

### How (as built, 2026-10-12)

- **yt-dlp** does the download. De facto standard, Unlicense, single `yt-dlp.exe`, `-U` self
  update because YouTube breaks downloaders every few months.
- **No ffmpeg.** We request YouTube's native audio track (`-f bestaudio[ext=m4a]/bestaudio`),
  which the engine decodes itself (symphonia, Media Foundation fallback). No re-encode, no
  100 MB dependency. Time-range trimming is done in Hotmic on the decoded audio and stored as
  `trim_start` / `trim_end` on the clip, so it is non-destructive and editable later.
- **Deno** is required. Since 2025 yt-dlp needs a JavaScript runtime to solve YouTube's player
  challenges; without one YouTube answers "sign in to confirm you're not a bot". Hotmic passes
  `--js-runtimes deno:<path>`.
- Both tools are downloaded on first use into `%LOCALAPPDATA%\hotmic\bin` from their official
  GitHub releases and verified against the published SHA-256 (yt-dlp's `SHA2-256SUMS`, Deno's
  PowerShell-style `.sha256sum`). About 62 MB total. Nothing is bundled in the installer.
- Run as plain child processes with `CREATE_NO_WINDOW`, progress parsed from `--newline`
  output, final path from `--print after_move:filepath`.
- Optional `--cookies-from-browser` (Chrome, Edge, Firefox, Brave) for logged-in access when
  YouTube still rate limits an address.
- Any URL yt-dlp supports works (SoundCloud, Twitter, Twitch clips, direct links).

### Caveats

- YouTube rate limits (HTTP 429) show up after a handful of requests from one address; the
  browser-cookies option is the escape hatch.
- Legal note for the README: YouTube's terms forbid downloading except where they provide a
  button. Personal use of content you have the right to use. Same stance as every other
  open-source tool in this space.
- Legal note for the README: YouTube's terms forbid downloading except where they provide a
  button. This feature is for personal use of content you have the right to use. Same stance
  as every other open-source tool in this space.

## 8. Feature design: hotkeys

Requirements: bind a clip to a plain key like `1`, `2`, `3`, or to combos like `Ctrl+Alt+F5`.
Hotkeys must work while Discord or a game has focus.

The problem with plain keys: a global `1` binding swallows the key everywhere, so you can no
longer type "1" in chat. Three modes, user picks per profile:

| Mode | Behaviour | Good for |
|---|---|---|
| **Combo** (default) | Only modifier combos are global, e.g. `Ctrl+Alt+1`. | Never interferes with typing. |
| **Layer key** | Hold a chosen key (CapsLock, Right Alt, a mouse side button) and bare keys fire clips. Release and the keyboard is normal again. | Fast, no conflicts, one hand. |
| **Hot mode toggle** | A key (default `Scroll Lock`) toggles "hot mode". While on, bare keys `1`..`9`, `0`, `Numpad*` fire clips and a tray icon turns red. | Gaming sessions where you never type. |

Implementation: `tauri-plugin-global-shortcut` handles combo mode. Layer and toggle modes need a
low-level keyboard hook (`SetWindowsHookEx(WH_KEYBOARD_LL)` via the `windows` crate) so the app
can see bare keys and swallow them only while the layer is active. Same hook gives key-up for
"hold to play, release to stop" clips.

Extras in the same system: a Stop All key, a Push-To-Play key, "toggle mic" key, and an optional
"press again to stop" behaviour per clip. Record a hotkey by clicking the clip's key field and
pressing the keys, with conflict warnings. Numpad keys are first-class and show as `Num1`.

## 9. Feature ideas (ranked)

Tier 1, do these, they are cheap and make it feel finished:

- **Auto push-to-talk**: while a clip plays, Hotmic holds the user's Discord PTT key (SendInput).
  Sidesteps Discord's voice-activity gating, which otherwise cuts non-speech parts of clips
  even with noise suppression off. Soundpad has this; users expect it.
- **Built-in mic cleanup**: RNNoise (pure Rust port `nnnoiseless`) on the mic path before the
  mix, later echo cancellation and gain control via WebRTC's audio processing module. Lets
  users turn Discord's Krisp off without losing a clean voice, so clips pass untouched.
- **Loudness normalisation** on import (EBU R128 to about -16 LUFS) so one clip is not ten
  times louder than the next. Fixes the number one complaint about soundboards.
- **Mic ducking**: lower your voice by N dB while a clip plays, so clips cut through.
- **Instant replay**: keep the last 30 seconds of desktop audio and of your mic in a ring buffer.
  Press a key, and "that thing someone just said" becomes a clip. Needs WASAPI loopback capture,
  which is already in cpal. This is the killer feature nobody free has.
- **Record a clip** directly from the mic, with trim afterwards.
- **Search, tags, favourites, colours, emoji icons** on buttons. Folders map to tabs.
- **Fade out on stop** instead of a hard cut, and **crossfade** when one clip replaces another.
- **Random from folder**: one hotkey, random clip from a folder. Meme boards love this.
- **Import and export sound packs** as zip with a manifest, so packs can be shared on GitHub.

Tier 2, strong differentiators:

- **Phone remote**: the app runs a tiny local HTTP server, your phone opens a page on the LAN
  and shows the button grid. A free Stream Deck. Protect with a pairing code.
- **Stream Deck and MIDI pad support** via the same local API.
- **Local text-to-speech clips** with Piper (fully offline, fits the local ethos): type a line,
  get a clip.
- **Desktop audio to call**: send Spotify or a browser tab into the call with per-app WASAPI
  process loopback, like CommonMik. Fits the mixer naturally.
- **Pitch and speed knobs** per clip, with a "chipmunk" and "slowed" preset. Cheap with rubato.
- **Clip editor**: waveform, trim, fade, gain, loop region, split one long file into many clips.
- **Profiles**: different grids and hotkeys per game or app, auto-switch on foreground window.
- **Portable mode**: everything next to the exe on a USB stick.
- **Discord bot mode**: Hotmic joins the voice channel as a local bot and streams clips as a
  separate participant, like Discord's own soundboard. Bypasses all mic processing, keeps the
  user's Discord settings untouched. Only works in servers where the bot can be added.

Tier 3, later or community-driven:

- Linux support via PipeWire (the virtual mic is free there, no VB-Cable needed). Keep the core
  platform-agnostic from day one so this stays possible.
- Plugin or scripting hooks (a clip triggers an HTTP call, OBS scene change, etc.).
- Playlists and sequences (play A then B), timed loops.
- Own virtual mic driver (Phase 4) to remove the VB-Cable install.
- Localisation and full keyboard accessibility.

Deliberately not doing: cloud accounts, telemetry, built-in sound store, AI voice changer.

## 11. UI direction (research, 2026-10-12)

Findings:

- Competitors: Soundpad is a dated list-style WinForms UI; Voicemod is neon purple/teal gamer
  tiles; Resanance is flat dark grey. None feels like a native Windows 11 app. That is the gap.
- Windows 11 conventions worth adopting: Mica backdrop (window takes the desktop tint), Segoe UI
  Variable, 4 and 8 px radii, hairline layers instead of boxes, restrained colour with one accent,
  dark mode designed first. Tauri 2 gets Mica through the `window-vibrancy` crate with
  `transparent: true` and `decorations: false`, which means a custom title bar (drag region,
  min/max/close, Snap Layouts need a bit of extra work).
- Icons: Lucide (ISC, inline SVG, no npm). Fonts: Segoe UI Variable from the OS for UI text;
  Manrope (OFL) for display; JetBrains Mono for hotkeys and times.

Proposed layout (mockup at https://claude.ai/artifact/JVxjLimRykhacTHfV4yA5N):

- Custom title bar with the engine route in it.
- Icon rail on the left: Board, Import, Hotkeys, Settings. Devices and tools leave the board.
- Board: search with Ctrl+K command palette, filter chips (folders or tags), tile grid with
  tint or thumbnail, hotkey badge, duration, progress bar and glow while playing.
- Collapsible mixer rail on the right: meters, now playing, mic/clip/duck gains, monitor,
  replace mode, normalise, Stop all.
- Status bar: engine health, Discord input reminder, tray hint.
- Three skins on the same components: Fluent (native), Studio (pro-audio), Quiet (minimal).
  Decision pending the user's pick.

## 10. Sources consulted

- Soundpad product page and Steam thread on its driver extension and recording-device requirement.
- Soundux GitHub (GPL-3, plays to VB-Cable on Windows, installer bundles VB-Cable).
- Resanance and other free boards: all rely on a virtual cable plus "Listen to this device".
- CommonMik GitHub: WASAPI process loopback + mic mixed into VB-Cable.
- VB-Audio licensing and terms pages (donationware, 10+ unit distribution needs agreement).
- Microsoft docs: kernel-mode code signing, Hardware Dev Center attestation, APO overview.
- cpal 0.18, symphonia 0.6, rubato 5 docs; Tauri 2 global-shortcut plugin docs; NAudio author's
  blog on MixingSampleProvider and WASAPI resampling.
- yt-dlp audio extraction and `--download-sections` docs (ffmpeg required for both).
- Tauri 2 sidecar docs (`bundle.externalBin`, shell plugin permissions).
- rustypipe crate and CLI (pure-Rust YouTube client, needs botguard helper, last release 2025-04).
