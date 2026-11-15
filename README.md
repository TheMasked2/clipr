# Clipr

Our own screen recorder and clipper (think Medal or OBS), grown out of two projects. Nothing here is wired
together yet: this is a starting snapshot, copied unchanged, to shape the idea from.

## Where everything came from

| Folder | Source | What it is |
|---|---|---|
| `ultron/` | [TheMasked2/ultron](https://github.com/TheMasked2/ultron) at `5590c12` (every tracked file) | Voice clipping on top of OBS: game detection from Discord's detectable-games list, a local wake word and speech recognition, OBS replay-buffer control over WebSocket, one-click OBS setup, the Inno Setup installer and the self-updater. MIT licensed. |
| `pumpkin/` | [TheMasked2/pumpkin](https://github.com/TheMasked2/pumpkin) at `bb70d3e` (selected files) | The recording side of the Pumpkin Discord bot (below). |

From Pumpkin, paths as they were there:

- `agent/pumpkin_agent.py`, `agent/pumpkin-agent.cmd`, `agent/agent.ps1`: the PC agent. Windows Graphics
  Capture (Desktop Duplication fallback) into a 2-minute ring buffer of segments, GPU encoding (AMF, NVENC,
  Quick Sync; libx264 fallback), 1080p 16:9 on a GPU and 720p on the CPU with stretched 4:3 filling 16:9,
  restart on display-mode changes, PC sound (WASAPI loopback) and the mic recorded in step with the screen,
  saved clips kept on disk until no game has run for 30 s (Discord's game list), then encoded and uploaded at
  full speed. It also handles the SSH tunnel, single instance, autostart and self-update.
- `pumpkin/agent_link.py`, `pumpkin/agent_commands.py`: the server side of the agent: websocket, pairing with
  tunnel-only SSH keys, record on/off, save and receive clips, and the `/agent` commands.
- `pumpkin/voice.py`, `pumpkin/stt.py`: listening in a Discord call (wake word "pumpkin, clip that",
  speech to text), the call audio buffer, mixing the call into a clip, and `gate_mic`, which keeps the PC mic
  only where Discord heard the person (muted or push-to-talk talk stays out).
- `pumpkin/r2.py`: Cloudflare R2 uploads (hand-signed S3 SigV4).
- `pumpkin/clips.py`: the clips archive (index, keepers, cleanup).
- `pumpkin/gallery.py`, `pumpkin/gallery.html`: saved clips on shelves (funny sayings, aimer, BS, random) and
  the YouTube-style web gallery with personal links.
- `tests/test_voice.py`, `tests/test_gallery.py`, `tests/test_clips.py`: their tests. They import Pumpkin's
  package layout, so they don't run here as they are.

Pumpkin and Ultron keep running from their own repos; this copy changes nothing there.
