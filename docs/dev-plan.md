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
| Audio | WASAPI system loopback, microphone capture, per-app process loopback (Windows build 20348+) | Detect process-loopback support; use system loopback when unavailable |
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
| Logging / crash | `tracing`, opt-in crash reports (minidump + `module+offset` stack, reported as a GitHub issue by the user) | |
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
- [x] Cargo workspace with the crates above (empty stubs), `rustfmt`, `clippy -D warnings`, GitHub Actions
      (build, test, clippy on Windows)
- [x] `LICENSE` (GPL-3.0), contributing notes
- [x] `core`: config load/save (TOML), event bus, `tracing` logs in `%LOCALAPPDATA%\clipr\logs`
- [x] `clipr`: single instance, tray icon with a placeholder menu, clean shutdown
- [x] `web`: axum on `127.0.0.1:<port>`, serves the embedded Svelte "hello" page, opened from the tray; a second launch opens the running copy (the installer adds the shortcut in Phase 7)
- [x] Svelte project in `ui/` with the theme token system (one theme, accent variable)

Done 2026-11-15. The tray quick actions (Save clip, Start recording) are already in the menu, greyed out until Phase 2.

### Phase 1: Capture + encode spike (go / no-go for pure Rust)
- [x] WGC capture of a monitor into D3D11 textures (window capture is written but not tested yet)
- [x] GPU colour conversion to NV12 (D3D11 video processor, BT.709 limited range)
- [x] Media Foundation hardware H.264, HEVC and AV1 encode, D3D-backed (no CPU copies)
- [x] Write a 30 s MP4 to disk; decodes cleanly in ffmpeg (Discord, phone and VLC not checked yet)
- [ ] Measure FPS impact in a real game on NVIDIA, AMD and Intel (only AMD so far, desktop only)

Spike results (RX 9070 XT, 1080p60, 20 Mbps, desktop capture, `cargo run --release -p clipr-encode
--example spike`): 3–7 % of one CPU core in total, colour conversion ~0.1 ms and encoder push ~0.2–0.3 ms
per frame, 0 dropped frames, keyframe every 1.0 s, no B-frames. What we learned:
- **Pick the encoder by PCI device, not by adapter LUID.** A PC with two AMD GPUs has one encoder
  registration per GPU, and AMD ignores the `MFT_ENUM_ADAPTER_LUID` filter. The registrations carry a
  `PCI\VEN_..&DEV_..` path, which we match against the adapter's ids. Starting the other GPU's encoder
  doesn't just stall: it breaks the driver's other encoders for the rest of the process. A one-frame
  self-test stays as a safety net.
- **Set codec properties before the media types.** Set after, AMD's H.264 encoder sizes frames as if the
  stream were 30 fps (double the bitrate at 60 fps).
- **Use the `MF_LOW_LATENCY` attribute, not `CODECAPI_AVLowLatencyMode`.** The codec API one, set early,
  makes AMD's H.264 encoder write a corrupt stream.
- AMD's AV1 encoder treats CBR as a cap (~2 Mbps on a still desktop). Fine for clips; recheck in a game.
- WGC delivers at most ~60 fps by default. 120/144 fps capture needs `MinUpdateInterval`, which
  windows-rs 0.61 doesn't expose yet (Phase 2).
- **Gate:** if MF encoding turns out to be unworkable on one vendor, fall back to calling that vendor's SDK
  directly (NVENC/AMF) behind the same `VideoEncoder` trait. ffmpeg stays a last resort.

### Phase 2: Replay buffer + clipping
- [x] `buffer`: ring of encoded packets in RAM, keyframe-aligned, configurable length (disk segments: later)
- [x] Save last N seconds to MP4 with no re-encode, in `Videos\clipr\<Game>\{game}_{date}_{time}.mp4` (MKV: later)
- [x] Global hotkeys: clip, long clip (whole buffer), toggle buffer, mark moment (markers stored next to the clip)
- [x] `games`: Discord detectable list (cached, refreshed daily), foreground follow, custom/block lists
- [x] Idle when no game runs; optional "always buffer desktop" mode
- [x] Follow the game's monitor; a resolution change keeps the output size (the converter rescales)
- [ ] HDR detection + tonemap shader to SDR
- [x] Sound cue (optional); overlay toast: later
- [x] Uncapped-FPS protection instead of a game-priority mode: clipr's GPU work gets high scheduling priority
      and its capture thread joins MMCSS (`video.high_priority`, on by default); the frame loop repeats or
      skips frames to stay on time, and the log warns when a game starves it

Saving a 10 s 1080p60 clip takes ~30 ms. The whole app uses ~6 % of one core while buffering the desktop.

### Phase 3: Audio
- [x] System loopback, per-app process loopback, mic (the live "apps playing audio" picker comes with the
      Phase 4 audio page)
- [x] Clock alignment of all sources to the video timeline (QPC), per-source sync offset
- [x] Mixer: volume, mute, level meters (meters reach the UI in Phase 4)
- [x] Track matrix: up to 6 tracks, track 1 = mix; written as separate audio streams
- [x] Mic filters: RNNoise, gate, compressor, gain
- [x] Push-to-talk awareness: mic only while the PTT key is held (key read from settings)
- [x] AAC with a bitrate setting (Opus: skipped, Media Foundation can't put it in MP4)

Audio processing costs about 2 % of one core. A saved clip's audio starts within 10 ms of its video.

### Phase 4: Web UI + library
- [x] `library`: SQLite index, thumbnail + hover-scrub sprite generated after save (low priority)
- [x] Gallery: by game/day, hover scrub, player with track switcher, markers on timeline
- [x] Lossless trim (keyframe-snapped), rename, shelves, favorites, search (precise re-encoded trim: later)
- [x] Auto-cleanup (age / size, never favorites), low-disk guard
- [x] Settings: Simple (auto preset + quality slider) / Advanced (encoder, rate control, bitrate, keyframe
      interval, profile, preset, resolution, scaling, fps, container), with an explanation on every setting
- [x] Auto preset: GPU probe + short encode benchmark on first run, biased toward high bitrate
- [x] Profiles per game, switched automatically (separate quality for full recordings: with the second
      encoder, later)
- [x] Audio page: sources, mixer with live level meters, track matrix, filters
- [x] Hotkeys page with conflict detection
- [x] First-run step wizard (look → GPU preset → hotkeys → audio), all skippable (Discord and cloud steps
      come with those features)
- [x] LAN access opt-in with PIN
- [x] Theme: Dark sleek + accent picker

### Phase 5: Full recordings
- [x] Start/stop by hotkey, tray, UI; auto per game (stops when that game closes)
- [x] Shares the buffer's encoder (an optional second encoder with its own settings: later)
- [x] Crash-safe writing (fragmented MP4, so a crash keeps everything up to the last fragment)
- [x] Markers in recordings; cut a clip out of a recording in the gallery (trim)
- [x] Stop before disk full (`clips.min_free_gb`), and at `recording.max_minutes`

A capture restart (new monitor or video settings) finishes the file and continues in a new one.

### Phase 6: Discord
- [x] Webhook destinations (Settings → Discord): any number of channels, each with a Test button. URLs are
      secrets: never logged, and only the PC's own UI sees or changes them (LAN devices get them blanked)
- [ ] Bot on the user's own token: setup tutorial in the wizard (create app → token → invite link generated by clipr)
- [ ] Commands `/last`, `/clips [game]`, `/clip [length]`, `/record start|stop`, `/status`, each toggleable
- [x] Destinations: per-game channels (a game list per webhook), each one Ask (default) / Auto / Off
- [ ] Destinations: DMs (needs the bot)
- [x] Rich embed: title, game, length, thumbnail, no mentions. Game icon still to do
- [x] Post queue: nothing goes out while a game runs unless configured; kept across restarts. Ask prompts and
      post results show on Home; sharing a clip by hand from its page posts right away
- [ ] Ask prompts through the tray/overlay/DM
- [x] Size-fitted copy for Discord's upload limit (10/50/500 MB) when no cloud is set up: a software H.264
      transcode at a bitrate that fits, at 480p/720p/1080p by budget; the clip itself never changes
- [ ] Rich Presence (optional)

### Phase 7: Ship v1.0
- [x] Audio filters per source, like OBS: every source (system, mic, each app) has its own ordered list of
      filters, none by default: noise suppression (strength 0-100 %), noise gate (open/close levels, attack,
      hold, release), compressor, limiter, gain. Each can be switched off, reordered or removed
- [x] Every audio setting on the Audio page (volumes, push-to-talk, apps, separate tracks, offsets)
- [x] CLIP badge on the level meters when a source's raw input reaches full scale
- [x] Live before/after preview of a source's filters on the Audio page: a post-filter meter under each source's filters (after the filters, push-to-talk and volume) next to the raw level. No listen-back: playing system audio back would loop into its own capture.
- [x] Inno Setup installer (`installer/clipr.iss`: per user by default, Start menu, optional desktop icon,
      asks to quit a running clipr, uninstall keeps clips and asks about settings) + portable zip, both built
      by `scripts/package.ps1` and in CI. Start with Windows is a setting (Phase 2)
- [x] Opt-in updater (GitHub releases, only applies when no game runs and nothing is recording): checks every
      6 h when on, "Check now" / "Update now" in Settings → General, beta channel toggle, sha256-verified
      installer, portable copies get a Download link
- [x] Opt-in crash reports, no server: panics and native exceptions leave a record + minidump; Home shows a
      scrubbed report (no user/PC name, profile paths, LAN IPs) with "Report on GitHub" (pre-filled issue the
      user reviews and sends), Copy, Show dump file, Dismiss. Nothing is sent by itself
- [x] User docs in `docs/guide/`: getting started, every setting explained (with defaults), Discord webhooks,
      troubleshooting; a Help link in the UI. The last two settings without a UI control got one
- [ ] Discord bot tutorial in `docs/guide/discord.md` (with the bot)
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
| Per-app process loopback requires Windows build 20348+ | Detect process-loopback API support; keep system loopback and microphone capture distinct, and fall back to system loopback when process loopback is unavailable |
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
