# Hotmic

A free, unlimited soundboard for Windows. Play any audio file into Discord, WhatsApp, Zoom or any
other call while your microphone stays hot. Your voice and the clips are mixed, not swapped.

Status: **Phase 0**, proving the audio routing. Nothing usable yet.

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

## Planned stack

Rust core (cpal, symphonia, rubato) with a Tauri 2 UI. See PLAN.md for the reasoning and the
C# + NAudio alternative.

## License

MIT, see [LICENSE](LICENSE).
