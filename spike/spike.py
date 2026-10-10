"""Hotmic Phase 0 spike.

Captures your real microphone, mixes in audio clips, and plays the result into a
virtual cable ("CABLE Input" from VB-Cable). Point Discord's input device at
"CABLE Output" and the other side hears your voice plus the clips.

This is a throwaway proof of routing, not the app. See README.md in this folder.
"""

from __future__ import annotations

import argparse
import collections
import sys
import threading
import time
from pathlib import Path

import numpy as np
import sounddevice as sd
import soundfile as sf
import soxr

SR = 48_000
AUDIO_EXTS = {".mp3", ".wav", ".ogg", ".flac", ".opus", ".aiff", ".aif"}


# ----------------------------------------------------------------------------
# Devices
# ----------------------------------------------------------------------------
def wasapi_api_indices() -> set[int]:
    return {i for i, api in enumerate(sd.query_hostapis()) if "WASAPI" in api["name"]}


def list_devices() -> None:
    wasapi = wasapi_api_indices()
    print("WASAPI devices (use these names or indices):\n")
    for i, d in enumerate(sd.query_devices()):
        if d["hostapi"] not in wasapi:
            continue
        kind = []
        if d["max_input_channels"] > 0:
            kind.append(f"in:{d['max_input_channels']}")
        if d["max_output_channels"] > 0:
            kind.append(f"out:{d['max_output_channels']}")
        print(f"  [{i:3}] {d['name']:<55} {' '.join(kind):<12} {int(d['default_samplerate'])} Hz")


def find_device(spec: str | None, kind: str) -> int:
    """Resolve a device index from an index string or a case-insensitive name substring.

    Prefers WASAPI devices. kind is "input" or "output".
    """
    devs = sd.query_devices()
    wasapi = wasapi_api_indices()
    if spec is None or spec == "":
        default = sd.default.device[0 if kind == "input" else 1]
        if default < 0:
            sys.exit(f"No default {kind} device. Use --list and pass --{'mic' if kind == 'input' else 'cable'}.")
        return default
    if spec.isdigit():
        return int(spec)
    cands = [
        (i, d)
        for i, d in enumerate(devs)
        if spec.lower() in d["name"].lower() and d[f"max_{kind}_channels"] > 0
    ]
    preferred = [c for c in cands if c[1]["hostapi"] in wasapi]
    pick = preferred or cands
    if not pick:
        sys.exit(f"No {kind} device matching '{spec}'. Run with --list to see names.")
    return pick[0][0]


# ----------------------------------------------------------------------------
# Clips
# ----------------------------------------------------------------------------
def to_stereo_48k(data: np.ndarray, sr: int) -> np.ndarray:
    if sr != SR:
        data = soxr.resample(data, sr, SR)
    if data.ndim == 1:
        data = data[:, None]
    if data.shape[1] == 1:
        data = np.repeat(data, 2, axis=1)
    elif data.shape[1] > 2:
        data = data[:, :2]
    return np.ascontiguousarray(data, dtype=np.float32)


def load_clip(path: Path) -> np.ndarray:
    data, sr = sf.read(str(path), dtype="float32", always_2d=True)
    return to_stereo_48k(data, sr)


def test_tone() -> np.ndarray:
    """One second 440 Hz beep with decay, so routing can be tested with no files."""
    t = np.arange(SR, dtype=np.float32) / SR
    s = 0.4 * np.sin(2 * np.pi * 440 * t) * np.exp(-3 * t)
    return to_stereo_48k(s, SR)


def load_clips(folder: Path) -> list[tuple[str, np.ndarray]]:
    clips: list[tuple[str, np.ndarray]] = []
    if not folder.is_dir():
        return clips
    for p in sorted(folder.iterdir()):
        if p.suffix.lower() not in AUDIO_EXTS:
            continue
        try:
            clips.append((p.name, load_clip(p)))
            print(f"  loaded {p.name} ({len(clips[-1][1]) / SR:.1f}s)")
        except Exception as exc:  # noqa: BLE001 - spike, report and move on
            print(f"  skipped {p.name}: {exc}")
        if len(clips) == 9:
            break
    return clips


# ----------------------------------------------------------------------------
# Engine
# ----------------------------------------------------------------------------
class Engine:
    def __init__(self, mic_channels: int, mic_gain: float, clip_gain: float, monitor: bool):
        self.mic_channels = mic_channels
        self.mic_gain = mic_gain
        self.clip_gain = clip_gain
        self.monitor_on = monitor
        self.lock = threading.Lock()
        self.voices: list[list] = []  # [clip, position]
        self.monitor_q: collections.deque[np.ndarray] = collections.deque(maxlen=8)
        self.xruns = 0
        self.callbacks = 0
        self.peak = 0.0

    def play(self, clip: np.ndarray) -> None:
        with self.lock:
            self.voices.append([clip, 0])

    def stop_all(self) -> None:
        with self.lock:
            self.voices.clear()

    # Duplex callback: mic in, cable out.
    def duplex(self, indata, outdata, frames, time_info, status) -> None:  # noqa: ANN001
        if status:
            self.xruns += 1
        self.callbacks += 1

        mic = indata[:, :2] if self.mic_channels >= 2 else np.repeat(indata[:, :1], 2, axis=1)
        clipmix = np.zeros((frames, 2), dtype=np.float32)

        with self.lock:
            for v in list(self.voices):
                clip, pos = v
                chunk = clip[pos : pos + frames]
                n = len(chunk)
                clipmix[:n] += chunk
                v[1] = pos + n
                if v[1] >= len(clip):
                    self.voices.remove(v)

        clipmix *= self.clip_gain
        out = mic * self.mic_gain + clipmix
        self.peak = max(self.peak, float(np.max(np.abs(out))) if frames else 0.0)
        np.clip(out, -1.0, 1.0, out=outdata)

        if self.monitor_on:
            self.monitor_q.append(clipmix.copy())

    # Monitor callback: clips only, to your headphones.
    def monitor(self, outdata, frames, time_info, status) -> None:  # noqa: ANN001
        if self.monitor_q:
            block = self.monitor_q.popleft()
            n = min(frames, len(block))
            outdata[:n] = block[:n]
            outdata[n:] = 0
        else:
            outdata.fill(0)


# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------
def main() -> None:
    ap = argparse.ArgumentParser(description="Hotmic Phase 0 routing spike")
    ap.add_argument("--list", action="store_true", help="list WASAPI devices and exit")
    ap.add_argument("--mic", default=None, help="mic device index or name substring (default: system default input)")
    ap.add_argument("--cable", default="CABLE Input", help="output device index or name substring")
    ap.add_argument("--monitor", default=None, nargs="?", const="", help="also play clips to this output (default output if no value)")
    ap.add_argument("--clips", default=str(Path(__file__).parent / "clips"), help="folder with up to 9 audio files")
    ap.add_argument("--block", type=int, default=480, help="frames per callback (480 = 10 ms at 48 kHz)")
    ap.add_argument("--mic-gain", type=float, default=1.0)
    ap.add_argument("--clip-gain", type=float, default=0.8)
    ap.add_argument("--global-hotkeys", action="store_true", help="Ctrl+Alt+1..9 play, Ctrl+Alt+0 stop (needs 'keyboard' package)")
    args = ap.parse_args()

    if args.list:
        list_devices()
        return

    mic_idx = find_device(args.mic, "input")
    cable_idx = find_device(args.cable, "output")
    mic_info = sd.query_devices(mic_idx)
    cable_info = sd.query_devices(cable_idx)
    mic_ch = min(2, mic_info["max_input_channels"])

    print(f"mic   : [{mic_idx}] {mic_info['name']} ({mic_ch} ch)")
    print(f"cable : [{cable_idx}] {cable_info['name']}")

    print("clips :")
    clips = load_clips(Path(args.clips))
    if not clips:
        print("  (none found, press T for a test tone)")

    engine = Engine(mic_ch, args.mic_gain, args.clip_gain, monitor=args.monitor is not None)

    wasapi = sd.WasapiSettings(auto_convert=True)  # let Windows resample if a device is not at 48 kHz
    stream = sd.Stream(
        device=(mic_idx, cable_idx),
        samplerate=SR,
        blocksize=args.block,
        dtype="float32",
        channels=(mic_ch, 2),
        callback=engine.duplex,
        latency="low",
        extra_settings=(wasapi, wasapi),
    )

    monitor_stream = None
    if args.monitor is not None:
        mon_idx = find_device(args.monitor, "output")
        print(f"monitor: [{mon_idx}] {sd.query_devices(mon_idx)['name']}")
        monitor_stream = sd.OutputStream(
            device=mon_idx,
            samplerate=SR,
            blocksize=args.block,
            dtype="float32",
            channels=2,
            callback=engine.monitor,
            latency="low",
            extra_settings=wasapi,
        )

    if args.global_hotkeys:
        try:
            import keyboard  # type: ignore

            for n, (_, clip) in enumerate(clips, start=1):
                keyboard.add_hotkey(f"ctrl+alt+{n}", engine.play, args=(clip,))
            keyboard.add_hotkey("ctrl+alt+0", engine.stop_all)
            print("global hotkeys: Ctrl+Alt+1..9 play, Ctrl+Alt+0 stop all")
        except ImportError:
            print("'keyboard' package not installed, global hotkeys disabled")

    tone = test_tone()
    print("\nkeys (this window focused): 1-9 play clip, T test tone, 0 stop all, M toggle monitor, Q quit\n")

    import msvcrt

    with stream:
        if monitor_stream:
            monitor_stream.start()
        lat_in, lat_out = stream.latency
        print(f"reported latency: in {lat_in * 1000:.1f} ms, out {lat_out * 1000:.1f} ms")
        last = time.time()
        while True:
            if msvcrt.kbhit():
                ch = msvcrt.getwch()
                if ch in ("q", "Q"):
                    break
                if ch in ("t", "T"):
                    engine.play(tone)
                elif ch == "0":
                    engine.stop_all()
                elif ch in ("m", "M") and monitor_stream:
                    engine.monitor_on = not engine.monitor_on
                    print(f"monitor {'on' if engine.monitor_on else 'off'}")
                elif ch.isdigit() and 1 <= int(ch) <= len(clips):
                    name, clip = clips[int(ch) - 1]
                    print(f"play {name}")
                    engine.play(clip)
            now = time.time()
            if now - last >= 2.0:
                with engine.lock:
                    active = len(engine.voices)
                print(f"  callbacks={engine.callbacks} xruns={engine.xruns} voices={active} peak={engine.peak:.2f}")
                engine.peak = 0.0
                last = now
            time.sleep(0.01)
        if monitor_stream:
            monitor_stream.stop()
            monitor_stream.close()
    print("bye")


if __name__ == "__main__":
    main()
