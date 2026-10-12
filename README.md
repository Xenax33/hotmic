# Hotmic

A free, unlimited soundboard for Windows. Play any audio file into Discord, WhatsApp, Zoom or any
other call while your microphone stays hot. Your voice and the clips are mixed, not swapped.

Status: **usable daily**. The engine plays clips over the live mic into VB-Cable; the desktop app
has a clip grid with global hotkeys, YouTube import (yt-dlp + Deno, no ffmpeg), loudness
normalisation, non-destructive trim, a mixer rail, Ctrl+K palette, tray mode, start with
Windows, a first-run setup check with a cable self-test, and an NSIS installer.

Planned headline features: unlimited clips, import straight from a YouTube URL with a time
range, bind any key to any clip, loudness normalisation, mic ducking, instant replay of the
last 30 seconds, and a phone remote. See [PLAN.md](PLAN.md) sections 7 to 9.

- [PLAN.md](PLAN.md): research, architecture decision, phases, known gotchas.
- [spike/](spike/): Python proof of routing (mic + clips into a virtual cable).

## How it will work

```
Real mic ----WASAPI capture----+
                               +--> Mixer (48 kHz, stereo, f32) --> WASAPI render --> "CABLE Input"
Decoded clips (RAM) -----------+                                                         |
                               +--> optional monitor --> your headphones                 v
                                                                      Discord mic = "CABLE Output"
```

v1 depends on [VB-Cable](https://vb-audio.com/Cable/) for the virtual microphone device. The
app itself does the mic passthrough and mixing, so no Windows "Listen to this device" hacks.

## Install

Download `Hotmic_<version>_x64-setup.exe` from the
[latest release](https://github.com/Xenax33/hotmic/releases/latest) and run it. The installer is
not code-signed yet, so Windows SmartScreen will ask you to confirm. Then install
[VB-Cable](https://vb-audio.com/Cable/) and follow the setup check that Hotmic opens on first run.

## Releasing

Push a version tag and GitHub Actions builds the installer and publishes the release:

```powershell
git tag -a v0.2.0 -m "Hotmic 0.2.0"; git push origin v0.2.0
```

Bump `version` in `crates/hotmic-app/tauri.conf.json` and the workspace `Cargo.toml` first.

## Building

Rust on Windows targets MSVC, so you need the Visual C++ build tools and the Windows SDK once:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Then:

```powershell
cargo build --workspace
cargo test -p hotmic-core
cargo run -p hotmic-cli -- devices
cargo run -p hotmic-cli -- run --mic "Headset Microphone" --cable "CABLE Input" --monitor ""
```

Build from PowerShell or cmd, not Git Bash: Git ships a GNU `link.exe` that shadows the MSVC
linker on the PATH.

Desktop app (Tauri 2, plain HTML frontend, no Node needed):

```powershell
cargo install tauri-cli --version "^2" --locked
cargo run -p hotmic-app
```

Settings and the clip library live in `%APPDATA%\dev.hotmic.app\config.json`. Downloaded tools
(yt-dlp, Deno) live in `%LOCALAPPDATA%\hotmic\bin`. URL imports are saved to `Music\Hotmic`.

Installer (NSIS, about 4 MB, needs the Tauri CLI):

```powershell
cd crates\hotmic-app; cargo tauri build
```

Output: `target\release\bundle\nsis\Hotmic_0.1.0_x64-setup.exe`.

Crates:

- `crates/hotmic-core`: the engine. Device enumeration, clip decoding (symphonia), real-time
  mixer, cpal streams with a resampling and drift-correcting mic path, loudness normalisation.
- `crates/hotmic-cli`: keyboard driven test harness for the engine.
- `crates/hotmic-app`: the Tauri shell. `src/lib.rs` holds state, persistence and the commands;
  `ui/` is the frontend.

## Planned stack

Rust core (cpal, symphonia, rubato) with a Tauri 2 UI. See PLAN.md for the reasoning and the
C# + NAudio alternative.

## License

MIT, see [LICENSE](LICENSE).
