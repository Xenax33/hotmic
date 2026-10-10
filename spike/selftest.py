"""Loopback self-test: proves audio sent to CABLE Input comes back out of CABLE Output.

Runs the spike engine (mic + clips -> CABLE Input) and at the same time records from
CABLE Output, the device Discord would use as its microphone. Plays a 440 Hz tone and
checks that the recording contains it. No Discord or second person needed.
"""

from __future__ import annotations

import sys
import time

import numpy as np
import sounddevice as sd

import spike

SR = spike.SR


def main() -> int:
    mic = spike.find_device(sys.argv[1] if len(sys.argv) > 1 else None, "input")
    cable_in = spike.find_device("CABLE Input", "output")
    cable_out = spike.find_device("CABLE Output", "input")
    mic_info = sd.query_devices(mic)
    mic_ch = min(2, mic_info["max_input_channels"])
    print(f"mic        : [{mic}] {mic_info['name']}")
    print(f"cable in   : [{cable_in}] {sd.query_devices(cable_in)['name']}")
    print(f"cable out  : [{cable_out}] {sd.query_devices(cable_out)['name']}")

    engine = spike.Engine(mic_ch, mic_gain=1.0, clip_gain=0.8, monitor=False)
    recorded: list[np.ndarray] = []

    def rec_cb(indata, frames, time_info, status):  # noqa: ANN001
        recorded.append(indata.copy())

    w = sd.WasapiSettings(auto_convert=True)
    duplex = sd.Stream(device=(mic, cable_in), samplerate=SR, blocksize=480, dtype="float32",
                       channels=(mic_ch, 2), callback=engine.duplex, latency="low",
                       extra_settings=(w, w))
    rec = sd.InputStream(device=cable_out, samplerate=SR, blocksize=480, dtype="float32",
                         channels=2, callback=rec_cb, latency="low", extra_settings=w)

    tone = spike.test_tone()
    with rec, duplex:
        time.sleep(0.5)
        quiet_blocks = len(recorded)
        t0 = time.time()
        engine.play(tone)
        time.sleep(1.5)
    print(f"engine callbacks={engine.callbacks} xruns={engine.xruns}")

    audio = np.concatenate(recorded, axis=0)
    before = audio[: quiet_blocks * 480]
    after = audio[quiet_blocks * 480 :]
    noise = float(np.abs(before).max()) if len(before) else 0.0
    peak = float(np.abs(after).max())

    # Find the dominant frequency in the loudest 4096-sample window after the tone started.
    mono = after[:, 0]
    if len(mono) >= 4096:
        idx = int(np.argmax(np.abs(mono)))
        start = max(0, min(idx, len(mono) - 4096))
        win = mono[start : start + 4096] * np.hanning(4096)
        spec = np.abs(np.fft.rfft(win))
        freq = float(np.fft.rfftfreq(4096, 1 / SR)[int(np.argmax(spec))])
    else:
        freq = 0.0

    # Rough latency: first sample above threshold after play() was called.
    thresh = max(0.02, noise * 4)
    hits = np.flatnonzero(np.abs(mono) > thresh)
    latency_ms = (hits[0] / SR * 1000) if len(hits) else float("nan")

    print(f"mic-only peak before tone : {noise:.3f}")
    print(f"peak after tone           : {peak:.3f}")
    print(f"dominant frequency        : {freq:.0f} Hz (expect ~440)")
    print(f"tone arrived after        : {latency_ms:.0f} ms (includes 1 block of scheduling)")

    ok = peak > 0.1 and 400 < freq < 480
    print("\nRESULT:", "PASS, the cable carries the mix" if ok else "FAIL, see numbers above")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
