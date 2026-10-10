# Phase 0 spike: prove the routing

Goal: a second Discord account hears your voice **and** a clip at the same time, with no Windows
"Listen to this device" hack. If this works, the real app is just this pipeline done properly.

## 1. One-time setup

1. Install [VB-Cable](https://vb-audio.com/Cable/) (free, donationware). Run the installer as admin,
   reboot if it asks.
2. Windows Settings > System > Sound > More sound settings (or `mmsys.cpl`):
   - Playback tab > **CABLE Input** > Properties > Advanced: set to **2 channel, 24 bit, 48000 Hz**.
   - Recording tab > **CABLE Output** > Properties > Advanced: same, **48000 Hz**.
   - Recording tab > CABLE Output > Listen tab: make sure "Listen to this device" is **unchecked**.
3. Discord > User Settings > Voice & Video:
   - Input Device: **CABLE Output (VB-Audio Virtual Cable)**
   - Output Device: your headphones (unchanged)
   - **Noise Suppression: None** (Krisp treats music as noise and will mute the clips)
   - **Echo Cancellation: off**, Automatic Gain Control: off (optional, but cleaner for testing)
4. Python deps, from this folder:

```powershell
python -m venv .venv
.venv\Scripts\pip install -r requirements.txt
```

5. Drop up to nine MP3/WAV/OGG/FLAC files into `clips/`. Or skip this and use the built-in test tone.

## 2. Run

```powershell
.venv\Scripts\python spike.py --list
```

Find your real microphone in the list, then:

```powershell
.venv\Scripts\python spike.py --mic "Your Mic Name" --cable "CABLE Input" --monitor
```

- `1`..`9` play a clip, `T` plays a test tone, `0` stops everything, `M` toggles the monitor
  (clips only, into your default output so you hear what others hear), `Q` quits.
- `--global-hotkeys` adds Ctrl+Alt+1..9 / Ctrl+Alt+0 that work while Discord is focused.
- Every two seconds it prints callback count, xruns (dropouts), active voices, and peak level.

## 3. What to check

| Check | Pass |
|---|---|
| Other person hears you talking normally | voice is continuous, no robot or chop |
| Press T or a number while talking | they hear the clip **and** your voice together |
| Clap near the mic, note the delay they report | under about 60 ms feels instant in a call |
| Leave it running 10+ minutes | xruns stays near 0, no slow pitch drift or growing delay |
| Toggle Discord noise suppression back on | clips get mangled (expected, documents the gotcha) |

Write the results into `RESULTS.md` in this folder so the engine design in Phase 1 can use them.

## 4. If something fails

- `No output device matching 'CABLE Input'`: VB-Cable not installed or not rebooted.
- Crackle or xruns climbing: raise `--block 960` (20 ms). If that fixes it, note it.
- Error opening the duplex stream: PortAudio refused to pair the two devices. Pass `--block 0` to
  let it choose, or confirm both devices are at 48 kHz in Windows.
- Voice sounds doubled: you still have "Listen to this device" enabled somewhere.
