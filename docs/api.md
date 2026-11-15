# clipr web API

The contract between `crates/web` and `ui/` (`ui/src/lib/api.ts` mirrors it). Everything is JSON unless
noted. The server listens on `127.0.0.1` only, unless LAN access is on (then every request from another
machine needs the PIN, see the end).

## Live state

- `GET /api/status` → `Status`
  ```json
  { "version": "0.1.0", "state": "idle|buffering|recording", "game": "Valorant" | null,
    "encoder": "AMD HW H.264 Encoder MFT" | null, "error": "text" | null,
    "recording_since": 1763213000 | null, "uptime_secs": 42,
    "paused": false }
  ```
  `paused` is true while the user has paused the replay buffer (then `state` is `idle`).
- `GET /api/events` (WebSocket). Server → client messages:
  - `{"type":"status", ...Status}` on connect and on every change
  - `{"type":"levels","levels":[{"name":"System","db":-23.5}]}` about 10 times a second while audio is
    running (nothing is sent without audio); `db` is dBFS per source, -60 and below is silence
  - `{"type":"clip_saved","clip": Clip}` when a clip or recording was written
  - `{"type":"clip_failed","message":"..."}`
  - `{"type":"library"}` when clips changed some other way (deleted, renamed, cleanup); refetch the list

## Actions

- `POST /api/clip` body `{"seconds": 30}` (optional; default `replay.clip_secs`) → `202`
- `POST /api/buffer` body `{"on": true}` → `204`
- `POST /api/recording` body `{"on": true}` → `204`
- `POST /api/mark` → `204`

## Settings

- `GET /api/config` → the whole `Config` (see `crates/core/src/config.rs`; sections `web ui general privacy
  replay video audio hotkeys clips games recording`, serde names as in the Rust structs, enums lowercase).
  `games.profiles` is a list of `{"game","fps","height","bitrate_kbps"}` (numbers may be null = keep
  `video.*`); `game` matches the detected game's name or exe, case-insensitive
- `PUT /api/config` body: a whole `Config` → `200 Config` (as saved) or `400 {"error":"message"}`
- `GET /api/system` → `{ "gpus": [{"name","vendor","encoders":["H.264","HEVC","AV1"]}], "monitors":
  [{"index":1,"name","width","height","primary"}], "known_games": 2600 }` (free-form; show what's there)

## Library

- `GET /api/clips?q=&game=&shelf=&favorite=true&kind=clip|recording` → `Clip[]`, newest first
  ```json
  { "id": "Valorant/Valorant_2026-11-15_14-03-22.mp4",   // path relative to the clips folder, '/' separators
    "title": "Valorant_2026-11-15_14-03-22",             // file stem, or the user's title
    "game": "Valorant", "kind": "clip|recording",
    "size_bytes": 56000000, "created": 1763215402,          // unix seconds
    "duration_secs": 30.0 | null, "width": 1920 | null, "height": 1080 | null,
    "favorite": false, "shelves": ["funny"], "markers": [12.5, 20.0],
    "url": "/media/Valorant/Valorant_2026-11-15_14-03-22.mp4",
    "thumb": "/thumbs/<id>.jpg" | null,                    // 320 px wide JPEG
    "sprite": "/thumbs/<id>.sprite.jpg" | null,            // 10 frames side by side, each 160 px wide, for hover scrub
    "sprite_frames": 10 }
  ```
- `GET /api/games` → `[{"name":"Valorant","clips":12,"size_bytes":...}]`
- `GET /api/shelves` → `["funny","clutches"]`
- `PATCH /api/clips` body `{"id", "title"?, "favorite"?, "shelves"?}` → `Clip`. A new title renames the file
  (same folder, same extension); the id changes, so use the returned clip.
- `DELETE /api/clips?id=<id>` → `204`; the file (and its sidecars) go to the Recycle Bin
- `POST /api/clips/trim` body `{"id","start","end","replace":false}` → `Clip` (the new file, `<title>_trim.mp4`,
  or the same file when `replace` is true). Lossless: the start snaps back to a keyframe.
- `POST /api/clips/reveal` body `{"id"}` (or `{}` for the clips folder) → `204`, opens Explorer
- `POST /api/cleanup` → `{"removed": 3, "freed_bytes": ...}` runs the age/size cleanup now (also runs hourly)
- `GET /api/disk` → `{"free_bytes", "total_bytes", "clips_bytes"}`

## Files

- `GET /media/<id>` → the video file, with HTTP Range support (the `<video>` element seeks with it)
- `GET /thumbs/<id>.jpg`, `/thumbs/<id>.sprite.jpg` → images (generated after a clip is saved, low priority)

## LAN access (opt-in)

When `web.lan` is on, the server also listens on the LAN. Requests from any address other than loopback must
carry the PIN: `POST /api/login {"pin"}` sets an HttpOnly cookie `clipr_session` (random token, valid 30 days);
without it every LAN request gets `401` and the UI shows a PIN screen. The PIN is `web.lan_pin` (6 digits,
generated when LAN access is first turned on, shown in Settings). Loopback requests never need it.
