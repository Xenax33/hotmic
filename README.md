# Hotmic

A free, unlimited soundboard for Windows. Play any audio file into Discord, WhatsApp, Zoom or any
other call while your microphone stays hot. Your voice and the clips are mixed, not swapped.

Status: **Phase 1**. Routing is proven (see `spike/RESULTS.md`) and the Rust engine plays clips
over the live mic into VB-Cable from a command line harness. No GUI yet.

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

Crates:

- `crates/hotmic-core`: the engine. Device enumeration, clip decoding (symphonia), real-time
  mixer, cpal streams with a resampling and drift-correcting mic path.
- `crates/hotmic-cli`: keyboard driven test harness for the engine.

## Planned stack

Rust core (cpal, symphonia, rubato) with a Tauri 2 UI. See PLAN.md for the reasoning and the
C# + NAudio alternative.

## License

MIT, see [LICENSE](LICENSE).
