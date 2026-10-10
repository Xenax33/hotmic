# Phase 0 results

Date: 2026-10-11. Machine: Windows 11 Pro, HyperX headset (USB, 48 kHz, 2 ch in/out), VB-Cable 4.5.

## Setup

- VB-Cable installed from the official zip (installer signed by Vincent Burel / VB-Audio).
- Cable endpoints came up at 48000 Hz by default, no manual change needed.
- No reboot was needed for the endpoints to appear (the installer asks for one anyway).

## Loopback self-test (`selftest.py`)

Mic + 440 Hz tone rendered into "CABLE Input", recorded from "CABLE Output" (the device Discord
would use as its mic).

| Measure | Value |
|---|---|
| Duplex callbacks in 2 s at 10 ms blocks | 200 |
| xruns | 0 |
| Mic-only peak before tone (room noise, headset) | 0.079 |
| Peak after tone started | 0.322 |
| Dominant frequency in the recording | 445 Hz |
| Tone first seen on CABLE Output after `play()` | 115 ms (Python sleep granularity included, not a latency figure) |

Verdict: **PASS**. A user-mode mixer writing into VB-Cable is enough; the other side of the cable
sees voice plus clip together.

## Earlier engine-only run (headset in, headset out)

150 callbacks in 1.5 s, 0 xruns, reported latency 22 ms in / 32 ms out.

## Still to do by hand

- Real Discord call test with a second account: confirm voice and clip together, confirm Krisp
  noise suppression must be off.
- 10+ minute run to watch for drift between mic and cable clocks.

## Decisions carried into Phase 1

- Capture the mic at its native format, resample inside the engine, render to the cable at
  48 kHz stereo f32.
- Keep a small mic ring buffer (about 20 ms) and correct drift by nudging the resample ratio.
- Monitor output is clips only, so the user never hears their own voice delayed.
