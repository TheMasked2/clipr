# clipr

A lightweight, open source game recorder and clipper for Windows. Its depth sits close to OBS, but setup and
use stay as easy as Medal.

clipr sits in the system tray and stays idle until a game starts. Then it keeps the last few minutes in a
replay buffer, so you can clip a moment with a hotkey (or, later, with your voice). Clips land in a local
gallery where you can trim, sort and share them. They can also go to Discord: posted to a channel or your DMs,
and fetched from your phone with bot commands.

> **Status:** design phase, with no Rust code yet. The full roadmap is in [docs/dev-plan.md](docs/dev-plan.md).
> The code clipr grew out of is archived in [`legacy/`](legacy/).

## Principles

- **Standalone.** One `clipr.exe`, with no OBS and no server of ours. Discord and cloud storage are optional,
  and they run on the user's own bot token and bucket.
- **Lightweight.** Rust core, native Windows APIs, GPU encoding. It uses no CPU or GPU while no game runs.
  The UI is a small Svelte web app embedded in the exe, so no webview or Electron is needed.
- **Never in the way of the game.** No uploads or posts during play unless the user configures them. Encoding
  runs at low priority. Nothing is injected into games (see *Capture* below), so it's safe with anti-cheat.
- **Professional when you want it.** Simple mode gives a smart preset and a quality slider. Advanced mode
  exposes everything, OBS-style: encoder, rate control, an audio track matrix, filters and profiles.
- **Everything optional, everything explained.** Every integration, upload, cue, update check and crash report
  is opt-in or can be turned off, and each setting says what it does.
- **Private by default.** Zero telemetry. Crash reports and the update check are opt-in. The web UI listens on
  localhost only unless you open it up.

## Features

✅ = planned for v1.0 · ⏭ = planned for v1.x · 💡 = idea, not planned yet

### Recording
- ✅ Replay buffer (default 2 min) kept in **RAM or on disk** (your choice). Default clip is 30 s, and
  several clip hotkeys can each have their own length.
- ✅ **Full recordings** run alongside the buffer: a hotkey or button starts one, or it starts automatically
  per game. They share the buffer's encode by default, and advanced settings can give them their own quality.
- ✅ Capture of the **game window only** or the **full monitor**. It follows the monitor the game is on and
  restarts cleanly when the display mode changes.
- ✅ GPU encoding (NVENC / AMF / Quick Sync through Windows Media Foundation): H.264, plus HEVC/AV1 where the
  GPU supports them. MP4 or MKV.
- ✅ **Smart auto preset:** on first run clipr detects the GPU and picks resolution, fps and a generous bitrate
  (clarity matters most in clips), starting from about 1080p60 at a high bitrate.
- ✅ **Profiles** per game (for example "Valorant 240 fps" or "Minecraft chill") that switch automatically.
- ✅ HDR games are tonemapped to SDR automatically, so clips don't look washed out.
- ✅ **Markers:** a hotkey drops a bookmark mid-game, and it shows on the clip's timeline later.
- ✅ Low-disk guard, plus a game-priority mode (low-priority encoder that steps down quality if the game's
  FPS drops).
- ⏭ Optional real HDR files (10-bit HEVC).

### Audio (OBS-grade)
- ✅ Sources: system audio, **per-app audio** (pick the game and Discord, never Spotify), and the mic (with
  push-to-talk awareness).
- ✅ A **track matrix** of up to 6 tracks: every source gets a checkbox per track, and track 1 is the mix.
  So you can mute your mic or Discord afterwards.
- ✅ Mixer: per-source volume, mute, live level meters and sync offset.
- ✅ Mic filters: noise suppression (RNNoise), noise gate, compressor, gain.
- ✅ Audio encoder settings: AAC or Opus, bitrate per track, sample rate.

### Clipping triggers and feedback
- ✅ Global hotkeys: clip (several lengths), start/stop recording, mark moment, mute mic, toggle buffer.
- ✅ Minimal feedback, each part optional: a short sound cue only you hear, and a tiny overlay toast
  ("Clip saved · 0:30") in its own topmost window, not an in-game hook.
- ⏭ **Voice commands**, all local. A custom wake word (openWakeWord), after which whisper.cpp tiny parses the
  command: "clip that", "clip the last minute", "start recording", "mark this". The cue per stage (heard,
  saved) can be none, visual or sound. More commands can be enabled at the cost of stricter matching.

### Game detection
- ✅ Detects games from Discord's public detectable-games list and follows the game in front.
- ✅ A custom "treat as game" list and a blocklist for false positives.
- ✅ No game means idle: the buffer is off and nothing runs. An optional "always buffer the desktop" mode
  covers the rest.

### Gallery (web UI)
- ✅ Clips grouped by game and day, with **hover-scrub thumbnails**.
- ✅ Lossless **trim**, rename, shelves and favorites, search.
- ✅ **Auto-cleanup:** delete unfavorited clips after N days or past X GB.
- ✅ Full recordings listed next to clips, with markers on the timeline.
- ⏭ **Montage:** merge several clips into one video.
- ⏭ **Vertical export (9:16)** with an adjustable crop box for TikTok, Shorts and Reels.

### Discord
- ✅ **Webhook posting:** paste a webhook URL and you're done.
- ✅ **Bot inside clipr** on your own bot token (a guided setup in the wizard). No server is needed, and it's
  online while your PC is on.
  - Commands, each toggleable: `/last`, `/clips [game]`, `/clip [length]`, `/record start|stop`, `/status`.
  - Posts to a chosen channel and/or your DMs, with **per-game channels** and a **rich embed** (game, title,
    length, thumbnail).
  - **Ask before posting** is the default. Nothing is posted unless you've configured a destination, and
    posts wait until the game closes (no lag mid-game).
- ✅ Rich Presence: "Recording with clipr".
- ✅ Without cloud storage, clipr makes a size-fitted copy for Discord's upload limit. With cloud storage,
  Discord gets a link with a playable embed.

### Cloud (bring your own)
- ⏭ Your own **Cloudflare R2 / S3 / Backblaze B2** bucket, with a step-by-step tutorial in the app.
- ⏭ Storage mode: **local only**, **cloud only** or **both** (both is the safest).
- ⏭ Upload timing, configurable and explained: after the game closes (default, Pumpkin-style), immediately
  with a bandwidth cap, or manual.
- ⏭ **Static share pages** per clip (a player plus OpenGraph embed, served straight from your bucket) and a
  **full online gallery**, so you can browse your clips from anywhere, read-only.
- ⏭ Optional **Cloudflare Tunnel** on your own account for the full live web UI remotely, with login.
- 💡 Shared squad galleries (friends sharing one bucket).

### App and UI
- ✅ System tray: status line ("● Buffering Valorant · 2:00"), quick actions (save clip, record, pause buffer,
  mute mic), open main page, gallery and settings. There's also a desktop shortcut that opens the UI.
- ✅ The web UI is reachable on localhost by default, and LAN access with a PIN is opt-in (for your phone at home).
- ✅ First-run **step wizard**, where every step is skippable: look → GPU preset → hotkeys → audio sources →
  Discord → cloud.
- ✅ Choose your look at setup: **Dark sleek** (default), **Minimal / terminal**, **Playful** or **System
  theme**, with your own accent colour. v1 ships Dark sleek with an accent picker. The other themes come
  in v1.x.
- ✅ Installer and portable zip. Auto-update from GitHub releases and anonymous crash reports are both
  **opt-in**.
- ⏭ Linux (PipeWire capture and audio). The capture and audio layers are written behind traits from the
  start so this can be added later.

## How it works

```
                ┌─────────────── clipr.exe (Rust) ────────────────┐
 game window ──►│ capture (WGC, D3D11) ─► HDR tonemap ─► encoder   │
                │                                  (Media Found.) │
 apps / mic  ──►│ audio (WASAPI, per-app loopback) ─► mixer ─►    │
                │        filters ─► tracks 1–6 ─► AAC/Opus        │
                │                         │                       │
                │                   replay buffer (RAM | disk)    │
                │                   full recording   ─► mux mp4/mkv│
                │                         │                       │
                │ games · hotkeys · overlay · tray · library (db) │
                │ web server (axum) + embedded Svelte UI          │
                │ discord bot / webhook · cloud · updater         │
                └─────────────────────────────────────────────────┘
```

- **Capture.** clipr uses Windows Graphics Capture only, the same OS API the Snipping Tool and Xbox Game Bar
  use. It reads finished frames from Windows and never touches the game process. Older recorders injected a
  DLL into the game ("game capture") to reach exclusive-fullscreen games. Modern Windows runs most games
  borderless under the hood, so that hack isn't needed, and anti-cheat (Vanguard, EAC, BattlEye) has nothing
  to flag.
- **Frames stay on the GPU** from capture through encode. The CPU never copies a full frame.
- The replay buffer holds **encoded** packets, not raw frames, cut at keyframes. Saving a clip writes
  those packets to a file with no re-encode, so it's instant.

## Repository layout

| Path | What |
|---|---|
| `docs/dev-plan.md` | Phased development plan and milestones |
| `legacy/ultron/` | [TheMasked2/ultron](https://github.com/TheMasked2/ultron) at `5590c12`, MIT. Voice clipping on OBS: game detection, wake word + STT, installer, updater. Reference only. |
| `legacy/pumpkin/` | The recording side of TheMasked2/pumpkin at `bb70d3e`: WGC capture agent, segment ring buffer, GPU encoding, WASAPI + mic sync, R2 upload, clips archive, gallery. Reference only. |

The legacy code is Python. It stays here as a spec and an archive. clipr is a Rust rewrite and doesn't run any
of it. Pumpkin and Ultron keep running from their own repos.

## Works well with

- **Discord:** optional, through your own webhook or bot.
- **Cloudflare R2 / any S3:** optional cloud storage and share pages.

## License

GPL-3.0. The archived Ultron code in `legacy/ultron/` keeps its MIT license.
