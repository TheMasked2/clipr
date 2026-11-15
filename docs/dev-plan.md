# clipr development plan

The feature list lives in the [README](../README.md). This file covers how we get there: architecture,
phases, milestones and the risks to settle early.

## Tech stack

| Area | Choice | Why |
|---|---|---|
| Language | Rust (stable), Cargo workspace | Performance, a single exe, no runtime |
| Windows APIs | `windows` crate (windows-rs) | Official bindings for WGC, D3D11, Media Foundation, WASAPI, Win32 |
| Capture | Windows Graphics Capture → D3D11 textures | No injection, safe with anti-cheat, frames stay on the GPU |
| Colour / HDR | D3D11 compute shader (BGRA/FP16 → NV12, HDR tonemap) | GPU-only conversion before the encoder |
| Video encode | Media Foundation hardware MFTs (H.264, HEVC, AV1) | Native NVENC/AMF/QSV, no ffmpeg |
| Audio | WASAPI loopback, process loopback (per-app), capture (mic) | Built in since Windows 10 2004 |
| Audio filters | `nnnoiseless` (RNNoise port), own gate / compressor / gain | Pure Rust, cheap |
| Audio encode | MF AAC encoder, `opus` for Opus | |
| Mux | Own fragmented-MP4 + MKV writer (or the MF Sink Writer if simpler) | Instant clip save from buffered packets |
| Async / web | `tokio`, `axum` (HTTP + WebSocket), `rust-embed` | Small, fast, UI embedded in the exe |
| UI | Svelte 5 + Vite, built into `ui/dist`, embedded at compile time | About 30 KB JS, easy theming |
| Library | SQLite through `rusqlite` | Clip index, shelves, markers, settings history |
| Config | TOML (`serde`, `toml`) with a JSON schema for the UI | Editable by hand and by the UI |
| Tray / hotkeys | `tray-icon`, Win32 `RegisterHotKey` plus a raw-input hook for PTT state | |
| Overlay | A layered, click-through, topmost Win32 window | Not a game hook |
| Discord bot | `twilight` (light, modular) or `serenity` | Gateway + slash commands |
| Webhooks / HTTP | `reqwest` (rustls) | |
| Rich Presence | Discord IPC (`discord-rich-presence`) | |
| Cloud | `rusty-s3` (SigV4 presign) + `reqwest` | Works with R2, S3, B2. Avoids the heavy AWS SDK |
| Voice (v1.x) | `ort` (ONNX Runtime) for openWakeWord, `whisper-rs` (whisper.cpp) | Local, multilingual-capable |
| Logging / crash | `tracing`, opt-in crash reports (minidump + backtrace upload) | |
| Packaging | Inno Setup installer + portable zip, GitHub Actions release | Same as Ultron |

### Crate layout

```
crates/
  clipr          # the binary: wires everything, tray, single instance, lifecycle
  core           # config, event bus, shared types (Clip, Game, Track, Profile)
  capture        # WGC session, monitor/window follow, display-change restart, HDR detect
  gpu            # D3D11 device, colour convert + tonemap shaders
  encode         # Encoder trait + Media Foundation implementation, auto-preset probe
  audio          # sources, process loopback, mixer, filters, track matrix, audio encoders
  buffer         # replay buffer (RAM ring or disk segments) of encoded packets
  mux            # mp4/mkv writers, lossless trim, concat
  games          # Discord detectable list, foreground tracking, custom/block lists
  library        # SQLite clip index, thumbnails/scrub sprites, cleanup, markers
  web            # axum server, REST + WebSocket API, LAN PIN auth, embedded UI
  discord        # webhook, bot, commands, embeds, post queue, Rich Presence
  cloud          # (v1.x) S3 upload queue, share pages, online gallery generator
  voice          # (v1.x) wake word + STT + command grammar
  update         # opt-in GitHub release updater
ui/              # Svelte app
installer/       # Inno Setup script
legacy/          # archived Python (Ultron, Pumpkin), reference only
```

Capture, audio and encode each sit behind a trait (`CaptureSource`, `AudioSource`, `VideoEncoder`), so Linux
(PipeWire + VA-API) can be added later without touching the rest.

## Performance budget

These targets get measured in CI on a perf test and checked by hand on low-end hardware.

| State | CPU | RAM | GPU |
|---|---|---|---|
| Idle (no game) | ~0% | < 20 MB | 0 |
| Buffering 1080p60 | < 2% of one core | < 60 MB + buffer (RAM mode: ~15 Mbps × 120 s ≈ 225 MB) | encoder only, < 1% game FPS loss |
| Saving a clip | spike < 100 ms, no re-encode | | |
| Web UI open | + a few MB (it's the browser's tab, not ours) | | |

## Phases

Each phase ends in something runnable. Tick items off here as they land.

### Phase 0: Foundations
- [ ] Cargo workspace with the crates above (empty stubs), `rustfmt`, `clippy -D warnings`, GitHub Actions
      (build, test, clippy on Windows)
- [ ] `LICENSE` (GPL-3.0), contributing notes
- [ ] `core`: config load/save (TOML), event bus, `tracing` logs in `%LOCALAPPDATA%\clipr\logs`
- [ ] `clipr`: single instance, tray icon with a placeholder menu, clean shutdown
- [ ] `web`: axum on `127.0.0.1:<port>`, serves the embedded Svelte "hello" page, opened from tray + desktop shortcut
- [ ] Svelte project in `ui/` with the theme token system (one theme, accent variable)

### Phase 1: Capture + encode spike (go / no-go for pure Rust)
- [ ] WGC capture of a window and a monitor into D3D11 textures
- [ ] GPU colour conversion to NV12
- [ ] Media Foundation hardware H.264 encode, D3D-backed (no CPU copies)
- [ ] Write a 30 s MP4 to disk and verify it plays everywhere (Discord, phone, VLC)
- [ ] Measure FPS impact in a real game on NVIDIA, AMD and Intel
- **Gate:** if MF encoding turns out to be unworkable on one vendor, fall back to calling that vendor's SDK
  directly (NVENC/AMF) behind the same `VideoEncoder` trait. ffmpeg stays a last resort.

### Phase 2: Replay buffer + clipping
- [ ] `buffer`: ring of encoded packets in RAM or on disk segments, keyframe-aligned, configurable length
- [ ] Save last N seconds to MP4/MKV with no re-encode, in `Videos\clipr\<Game>\{game}_{date}_{time}.mp4`
- [ ] Global hotkeys: several clip lengths, toggle buffer, mark moment (markers stored next to the clip)
- [ ] `games`: Discord detectable list (cached, refreshed daily), foreground follow, custom/block lists
- [ ] Idle when no game runs; optional "always buffer desktop" mode
- [ ] Follow the game's monitor; restart capture on display-mode change and on game window recreate
- [ ] HDR detection + tonemap shader to SDR
- [ ] Sound cue + overlay toast (each optional)
- [ ] Encoder at low priority; game-priority mode (quality steps down when game FPS drops)

### Phase 3: Audio
- [ ] System loopback, per-app process loopback (pick apps from a live list of apps playing audio), mic
- [ ] Clock alignment of all sources to the video timeline, per-source sync offset
- [ ] Mixer: volume, mute, level meters (streamed to the UI over WebSocket)
- [ ] Track matrix: up to 6 tracks, track 1 = mix; written as separate audio streams
- [ ] Mic filters: RNNoise, gate, compressor, gain
- [ ] Push-to-talk awareness: mic only while the PTT key is held (key read from settings)
- [ ] AAC/Opus encoders with per-track bitrate and sample rate

### Phase 4: Web UI + library
- [ ] `library`: SQLite index, thumbnail + hover-scrub sprite generated after save (low priority)
- [ ] Gallery: by game/day, hover scrub, player with track switcher, markers on timeline
- [ ] Lossless trim (keyframe-snapped, with an option for a precise re-encoded trim), rename, shelves, favorites, search
- [ ] Auto-cleanup (age / size, never favorites), low-disk guard
- [ ] Settings: Simple (auto preset + quality slider) / Advanced (encoder, rate control, bitrate, keyframe
      interval, profile, preset, resolution, scaling, fps, container), with an explanation on every setting
- [ ] Auto preset: GPU probe + short encode benchmark on first run, biased toward high bitrate
- [ ] Profiles per game, switched automatically; optional separate quality for full recordings
- [ ] Audio page: sources, mixer, track matrix, filters
- [ ] Hotkeys page with conflict detection
- [ ] First-run step wizard (look → GPU preset → hotkeys → audio → Discord → cloud), all skippable
- [ ] LAN access opt-in with PIN
- [ ] Theme: Dark sleek + accent picker

### Phase 5: Full recordings
- [ ] Start/stop by hotkey, tray, UI; auto per game
- [ ] Shares the buffer's encoder by default; optional second encoder with its own settings
- [ ] Crash-safe writing (fragmented MP4 / MKV, so a crash keeps everything up to the last second)
- [ ] Markers in recordings; cut a clip out of a recording in the gallery
- [ ] Stop before disk full

### Phase 6: Discord
- [ ] Webhook destinations
- [ ] Bot on the user's own token: setup tutorial in the wizard (create app → token → invite link generated by clipr)
- [ ] Commands `/last`, `/clips [game]`, `/clip [length]`, `/record start|stop`, `/status`, each toggleable
- [ ] Destinations: channel, DM, per-game channels; each one Ask (default) / Auto / Off
- [ ] Rich embed (game icon, title, length, thumbnail)
- [ ] Post queue: nothing goes out while a game runs unless configured; "ask" prompts come through the
      tray/overlay/DM after the game
- [ ] Size-fitted copy for Discord's upload limit when no cloud is set up
- [ ] Rich Presence (optional)

### Phase 7: Ship v1.0
- [ ] Inno Setup installer + portable zip, start with Windows (optional)
- [ ] Opt-in updater (GitHub releases, only applies when no game runs and nothing is recording)
- [ ] Opt-in crash reports
- [ ] User docs: setup, Discord bot tutorial, every setting explained
- [ ] Test matrix: NVIDIA / AMD / Intel, HDR on/off, multi-monitor, borderless + fullscreen, laptop iGPU
- [ ] Release v1.0 on GitHub

### v1.x (after release, roughly in this order)
1. **Cloud:** BYO R2/S3/B2 with an in-app tutorial; storage mode local / cloud / both; upload timing
   (after game / immediate throttled / manual); Discord posts as links
2. **Share pages + online gallery:** static HTML in the bucket with OpenGraph embeds
3. **Voice:** wake word (custom), whisper tiny command parsing, length by voice, extra commands, configurable cues
4. **Montage** and **vertical 9:16 export** (these need re-encoding through MF)
5. **More themes:** Minimal / terminal, Playful, System
6. **Cloudflare Tunnel** for remote full UI with login
7. Optional real HDR saves (10-bit HEVC)
8. **Linux:** PipeWire capture/audio, VA-API encode

Ideas, not planned yet: shared squad galleries, auto highlights (kill/death detection).

## Risks to settle early

| Risk | Plan |
|---|---|
| Media Foundation HW encoders differ between vendors (quirks, missing AV1) | Phase 1 spike on all three vendors, `VideoEncoder` trait with vendor fallbacks |
| WGC yellow border / capture prompt | On Windows 11, request borderless-capture access with `GraphicsCaptureAccess.RequestAccessAsync(GraphicsCaptureAccessKind.Borderless)` before setting `IsBorderRequired = false`; retain the border if access is denied or unsupported |
| Exclusive fullscreen games that WGC can't capture | Fall back to monitor capture; document it |
| Per-app audio needs Windows 10 2004+ | Detect it; fall back to system loopback |
| Lossless trim snaps to keyframes | 1–2 s keyframe interval by default; precise trim re-encodes only the edges |
| Bot token handling | Stored with Windows DPAPI, never logged, never sent anywhere except Discord |
| Scope creep | v1 cutline above; everything else waits for v1.x |

## Settled details

- **Wake words (v1.x):** ship a pick list of pre-trained openWakeWord models ("hey clipr", ...). An optional
  guided script trains a custom phrase. An advanced "any phrase" mode runs Whisper continuously and warns
  about its CPU cost (~3–5%).
- **Web UI port:** fixed `7845`. If it's taken, clipr uses the next free port, and the tray and shortcut
  always open the right one.
- **Logo:** a bold lowercase "c" wordmark that doubles as a circular rewind arrow, legible at 16 px.
- **Tray icon shows state:** grey = idle, accent = buffering, red dot = full recording.
