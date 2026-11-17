# clipr web API

The contract between `crates/web` and `ui/` (`ui/src/lib/api.ts` mirrors it). Everything is JSON unless
noted. The server listens on `127.0.0.1` only, unless LAN access is on (then every request from another
machine needs the PIN, see the end).

Requests that change things (everything but `GET`/`HEAD`) must send `Content-Type: application/json` when
they have a body (`415` otherwise). If a request has an `Origin` header (browsers add it to cross-site
requests and WebSockets) and it isn't the server's own address (`http://<Host>`), the answer is `403`. A
request from this machine whose `Host` isn't `127.0.0.1`, `localhost` or `[::1]` also gets `403` (DNS
rebinding). Other machines get a generic message on `500` (details stay in clipr's log).

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
  - `{"type":"discord"}` when the Discord post list changed; refetch `GET /api/discord/posts`

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
- `PUT /api/config` body: a whole `Config` → `200 Config` (as saved) or `400 {"error":"message"}`.
  `clips.folder` must be empty (default) or a full path that isn't a drive root, inside Windows or Program
  Files, or the whole user profile. From another machine (LAN) `clips.folder` and everything under `web` are
  read-only: changing them gives `403`
- `GET /api/system` → `{ "gpus": [{"name","vendor","encoders":["H.264","HEVC","AV1"]}], "monitors":
  [{"index":1,"name","width","height","primary"}], "known_games": 2600 }` (free-form; show what's there)

## Updates

clipr checks GitHub releases by itself only when `general.auto_update` is on (2 min after start, then every
6 h); `general.update_beta` adds prereleases. Only an installed copy (Inno Setup's `unins000.exe` next to the
exe) can install; a portable copy links the release page instead.

- `GET /api/update` → `{"current":"0.1.0-beta.1","installed":true,"state","release","checked_at","error",
  "progress","busy","will_install"}`. `state`: `idle checking up_to_date available downloading ready waiting
  installing error`. `release`: `null` or `{"version","page","notes","published","prerelease","setup",
  "portable"}` (each asset `null` or `{"name","url","size"}`). `progress`: `{"done","total"}` bytes while
  downloading. `busy`: why it can't install now (`"a game is running"`, `"a recording is running"`) or `null`.
  `waiting` = downloaded, installs once nothing runs
- `POST /api/update/check` → `202`; the answer shows up in `GET /api/update` (repeat checks within 10 s ask
  GitHub once)
- `POST /api/update/apply` → `202`: downloads the installer, checks its sha256 (GitHub's asset digest, else a
  `<name>.sha256` asset; with neither it refuses), quits clipr and runs it with `/VERYSILENT
  /SUPPRESSMSGBOXES /NORESTART /UPDATE=1`, which reinstalls and starts clipr again in the tray. `409
  {"error"}` while a game runs or something records, on a portable copy, or with nothing to install; `403`
  from another machine (LAN)

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

## Discord

Channels are `discord.webhooks` in the config (`name`, `url`, `mode`: `ask|auto|off`, `games`). Webhook URLs
are secrets: `GET`/`PUT /api/config` from a LAN device return them as `""` (and `GET /api/system` says
`"remote": true`). A LAN device may send `""` for a saved channel (keep its URL) or for a new one (no URL yet);
setting, changing or renaming its way to a URL is `403`.

- `GET /api/discord/posts` → jobs, newest first:
  ```jsonc
  [{ "id": 3, "webhook": "#clips", "state": "ask|queued|posting|posted|skipped|failed",
     "now": false,                       // posts even while a game runs (shared by hand, or answered)
     "error": "..." | null, "created": 1763215402,
     "clip": { "id", "title", "game", "duration_secs", "size_bytes", "kind" } }]
  ```
  A new clip (not a recording) becomes a job for every webhook whose mode isn't `off` and whose `games` list is
  empty or names the clip's game: `ask` jobs wait, `auto` jobs are posted once no game runs (or right away
  with `discord.post_during_game`). A post that already started finishes even if a game starts meanwhile.
  The last 50 finished jobs are kept. Renaming a clip updates the posts still waiting for it. `clip` has no
  local paths here.
- `POST /api/discord/share` body `{"id": "<clip id>", "webhook": "#clips"}` → `{"job": 4}`; posted right away
- `POST /api/discord/answer` body `{"job": 3, "yes": true}` → `204`. Yes on an `ask` or `failed` job posts it
  now; no on an `ask`, `queued` or `failed` job skips it.
- `POST /api/discord/test` body `{"url": "..."}` or `{"name": "#clips"}` (a saved one) → `204` after Discord
  took a short test message, `400` for a URL that isn't a Discord webhook, `502` with Discord's answer

## Files

- `GET /media/<id>` → the video file, with HTTP Range support (the `<video>` element seeks with it)
- `GET /thumbs/<id>.jpg`, `/thumbs/<id>.sprite.jpg` → images (generated after a clip is saved, low priority)

## LAN access (opt-in)

When `web.lan` is on, the server also listens on the LAN. Requests from any address other than loopback must
carry the PIN: `POST /api/login {"pin"}` sets an HttpOnly cookie `clipr_session` (random token, valid 30 days);
without it every LAN request gets `401` and the UI shows a PIN screen. The PIN is `web.lan_pin` (6 digits,
generated when LAN access is first turned on, shown in Settings). Loopback requests never need it. A session ends when `web.lan_pin` changes or `web.lan` is turned off. After
5 wrong PINs from one address, logins get `429` for a minute.
