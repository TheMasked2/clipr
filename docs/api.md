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
    "paused": false, "mic_muted": false }
  ```
  `paused` is true while the user has paused the replay buffer (then `state` is `idle`). `mic_muted` is true
  while the microphone is muted in clips (the mute hotkey or `POST /api/mic`; clipr starts unmuted). `muted`
  lists every source muted in clips by key (`POST /api/mute`; `mic` too while it is).
- `GET /api/events` (WebSocket). Server → client messages:
  - `{"type":"status", ...Status}` on connect and on every change
  - `{"type":"levels","levels":[{"source":"system","name":"System","db":-23.5,"after":-26.1}]}` about 10
    times a second while audio is running (nothing is sent without audio; see `POST /api/levels/watch`). Per source, peak dBFS over the
    last ~100 ms (-60 and below is silence): `db` as the device delivered it, `after` as it goes into the
    recording (after the source's filters, push-to-talk, mute and volume), each the louder channel;
    `db_lr` and `after_lr` give them per channel (left, right). `source` is the source's key in
    `audio.filters` (`system`, `mic`, or an app's exe name in lower case)
  - `{"type":"clip_saved","clip": Clip}` when a clip or recording was written
  - `{"type":"clip_failed","message":"..."}`
  - `{"type":"library"}` when clips changed some other way (deleted, renamed, cleanup); refetch the list
  - `{"type":"discord"}` when the Discord post list changed; refetch `GET /api/discord/posts`
  - `{"type":"config"}` when the settings were saved (by any page, the overlay or a fader); refetch `GET /api/config`
    so a page doesn't later save its old copy over the change
  - `{"type":"cloud"}` when uploads or clips' cloud states changed (also twice a second with upload progress);
    refetch `GET /api/cloud/uploads` and `GET /api/cloud/status`

## Actions

- `POST /api/clip` body `{"seconds": 30}` (optional; default `replay.clip_secs`) → `202`
- `POST /api/buffer` body `{"on": true}` → `204`
- `POST /api/recording` body `{"on": true}` → `204`
- `POST /api/mark` → `204`
- `POST /api/mute` body `{"source": "discord.exe", "muted": true}` → `204`: mutes or unmutes any source in
  clips by key (`system`, `mic`, `game`, an app's exe name), with a short fade; not saved. `400` for an empty
  key or one with `/`, `\` or `:`
- `POST /api/mic` body `{"muted": true}` → `204`: mutes or unmutes the microphone in clips (with a blip)
- `POST /api/sound/test` body `{"volume": 60}` (0–100) → `204`: plays the save sound once at that volume
- `GET /api/perf` → `200` JSON array, one sample a second over the last two minutes, oldest first:
  `{"at_ms", "fps", "fresh", "late", "dropped", "frame_ms", "kbps", "cpu", "mem_mb", "buffer_secs", "buffer_mb",
  "capturing"}`. `fps` counts encoded frames per second, `fresh` those with a new picture; `late` and `dropped`
  are frames encoded after their slot and slots skipped in that second; `frame_ms` is the capture thread's
  average time per frame; `kbps` the video actually written; `cpu` clipr's share of all cores in %; `mem_mb`
  its working set; `buffer_*` what the replay buffer holds.
- `POST /api/levels/watch` → `204`: a page shows level meters. For the next 5 seconds clipr keeps sending
  levels, and while nothing is captured it runs audio for the meters alone (recording nothing; it opens the
  microphone if that source is on). Pages call it every 2 seconds while a meter is on screen.
- `POST /api/audio/volume` body `{"source": "mic", "volume": 80}` (0–200 %) → `200` with the saved config: sets
  one source's volume (`system`, `mic`, `game` or an app's exe name), saved and applied at once without
  restarting audio. The game's and apps' volumes are in `audio.volumes`, by source key (missing = 100).
- `GET /api/preview?fps=5&width=1280` → `200` `image/jpeg` (`Cache-Control: no-store`; fitted in `width`×`width·9/16`, never bigger
  than what's captured), or `204` while nothing is
  captured (or before the first snapshot). The live preview: asking keeps clipr taking `fps` snapshots a second (query, 1–30, default 5), as wide as `width`
  (screen pixels, 320–1920, at most 1280 above 15 fps, default 640; the widest page asking wins) for
  the next 3 seconds, so a page polls it a few times a second while it shows the preview, and clipr stops when it
  stops asking. It shows what goes to the encoder. `503` `{"error"}` when the GPU can't make snapshots (clipr tries
  again every 10 s). Other devices get `403` unless `web.lan_preview` is on, since it shows the whole screen.

## Settings

- `GET /api/config` → the whole `Config` (see `crates/core/src/config.rs`; sections `web ui general privacy
  replay video audio hotkeys clips games recording discord cloud`, serde names as in the Rust structs, enums lowercase).
  `games.profiles` is a list of `{"game","fps","height","bitrate_kbps"}` (numbers may be null = keep
  `video.*`); `game` matches the detected game's name or exe, case-insensitive
- `PUT /api/config` body: a whole `Config` → `200 Config` (as saved) or `400 {"error":"message"}`.
  `clips.folder` must be empty (default) or a full path that isn't a drive root, inside Windows or Program
  Files, or the whole user profile. From another machine (LAN) `clips.folder` and everything under `web` are
  read-only: changing them gives `403`
- `GET /api/system` → `{ "gpus": [{"name","vendor","encoders":["H.264","HEVC","AV1"]}], "monitors":
  [{"index":1,"name","width","height","primary"}], "known_games": 2600, "elevated": false }` (free-form; show
  what's there). `elevated`: clipr runs as administrator, which can break game capture.

## Crash reports

Only with `privacy.crash_reports` on: a crash leaves `crashes/<id>.json` (+ `<id>.dmp`, a minidump) in the data
folder, and nothing is ever sent by itself. `CLIPR_CRASH_TEST=panic|access` crashes clipr on purpose 5 s after
start, for testing.

- `GET /api/crashes` → newest first, `[{"id","time","version","summary","report","issue_url","has_dump"}]`.
  `report` is Markdown with the user name, PC name, profile folder and LAN addresses taken out; `issue_url` is
  a pre-filled new GitHub issue (shortened to fit a URL). `[]` when the setting is off
- `POST /api/crashes/dismiss` body `{"id"}` → `204` (deletes the record and its dump) or `404`
- `POST /api/crashes/reveal` body `{"id"}` → `204` (Explorer at the dump file), `404` without a dump, `403`
  from another machine (LAN)

## Updates

clipr checks GitHub releases by itself only when `general.auto_update` is on, and then once per start, like
Steam (15 s after start; a failed check is retried after 1 and 5 min, then not again until the next start);
`general.update_beta` adds prereleases. Only an installed copy (Inno Setup's `unins000.exe` next to the
exe) can install; a portable copy links the release page instead.
For testing, `CLIPR_UPDATE_PRETEND_VERSION=0.0.1` makes the updater compare as that version, so an installed
copy can "update" to the current release (still sha256-verified).

- `GET /api/update` → `{"current":"0.1.0-beta.1","installed":true,"state","release","checked_at","error",
  "progress","busy","will_install"}`. `state`: `idle checking up_to_date available downloading ready waiting
  installing error`. `release`: `null` or `{"version","page","notes","published","prerelease","setup",
  "portable"}` (each asset `null` or `{"name","url","size"}`). `progress`: `{"done","total"}` bytes while
  downloading. `busy`: why it can't install now (`"a game is running"`, `"a recording is running"`) or `null`.
  `waiting` = an install is wanted; it downloads (if needed) and installs once nothing runs
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
- `GET /api/games/unlisted` → programs that ran fullscreen lately but aren't games to clipr, newest first, up to
  8: `[{"exe":"newgame.exe","title":"New Game"}]` (`exe` is what goes in `games.custom`). Another device
  (LAN) gets `"title": ""`: window titles stay on the PC.
- `PATCH /api/clips` body `{"id", "title"?, "favorite"?, "shelves"?}` → `Clip`. A new title renames the file
  (same folder, same extension); the id changes, so use the returned clip.
- `DELETE /api/clips?id=<id>` → `204`; the file (and its sidecars) go to the Recycle Bin
- `POST /api/clips/trim` body `{"id","start","end","replace":false}` → `Clip` (the new file, `<title>_trim.mp4`,
  or the same file when `replace` is true). Lossless: the start snaps back to a keyframe.
- `POST /api/clips/reveal` body `{"id"}` (or `{}` for the clips folder) → `204`, opens Explorer
- `POST /api/cleanup` → `{"removed": 3, "freed_bytes": ...}` runs the age/size cleanup now (also runs hourly)
- `POST /api/cleanup/preview` body `{"clips"?: {"max_age_days", "max_total_gb"}, "hours"?: 24}` →
  `{"count", "bytes", "clips": [Clip, ...]}`: what the cleanup would delete within `hours`, with the given limits
  (the settings page's unsaved ones) or the saved ones. Deletes nothing; `clips` holds the first five, oldest first.
- `POST /api/pick-folder` body `{"title"?, "start"?}` → `{"path": "D:\\Clips"}` or `{"path": null}` when
  cancelled. Opens the Windows folder picker on the PC and waits for it. `403` from other devices, `409` while
  one is already open.

Files in a separate recordings folder (`recording.folder`) have ids starting with `:recordings/`, like
`:recordings/Valorant/x_recording.mp4`; a colon can't be part of a folder name, so these never clash with clips.
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

Connect Discord (OAuth2 with PKCE as the public clipr app; only from this PC: `403` from a LAN device):

- `POST /api/discord/connect` body `{"channel": false}` → `{"url": "https://discord.com/oauth2/authorize?…",
  "state": "…"}`. Open `url`; with `"channel": true` Discord also asks for a channel and makes a webhook for it.
  `409` when clipr doesn't run on port 7845 (Discord only sends the browser back to
  `http://127.0.0.1:7845/discord/callback`), `429` with 20 sign-ins pending.
- `GET /api/discord/connect/{state}` → `202 {"pending": true}` until Discord sent the browser back, then
  `{"account": {"id", "username", "name", "avatar"}, "webhook": {"url", "channel_id", "guild_id"} | null}`, or
  `502` with what went wrong (cancelled, Discord refused). The answer can be read again until 15 minutes after
  the start, then `404`. Nothing is saved: put `account` in `discord.account` (and the webhook in
  `discord.webhooks`) with `PUT /api/config`. `discord.account` can only be changed from this PC (a LAN device's
  `PUT` keeps the saved one), and its `avatar` must be on `https://cdn.discordapp.com/avatars/<id>/`.
- `GET /discord/callback?code&state` is where Discord sends the browser; it shows a short page to close. Each
  sign-in takes one redirect; a reload of that page says it expired.

## Cloud

The bucket is `cloud` in the config (`enabled`, `provider`: `r2|s3|b2|other`, `endpoint`, `region`, `bucket`,
`access_key_id`, `secret_access_key`, `public_url`, `prefix`, `upload`: `after_game|now|manual`, `max_mbps`,
`recordings`). `endpoint` and `public_url` are `https://` (or `http://` to this PC, for a local test bucket).
The secret is like a webhook URL: a LAN device gets `""`, may send `""` to keep it, and changing the bucket
(provider, endpoint, region, bucket, key, secret, public address, folder) from a LAN device is `403`.
Turning uploads on or off, the timing, the speed limit and `recordings` are fine. Objects go to
`<prefix><clip id>` (path-style URLs; virtual-hosted on `*.amazonaws.com`), signed with AWS Signature V4.

- `GET /api/cloud/status` → `{ "<clip id>": { "state": "uploaded|queued|uploading|failed", "url": "..." | null,
  "progress": 0.42 | null, "error": "..." | null } }` for every clip that has one. `url` is the public link
  (needs `public_url`); it keeps the clip's name at upload time, so a renamed clip's link still works.
- `GET /api/cloud/uploads` → jobs, newest first: `{ "id", "clip": {…, no local paths}, "key",
  "state": "queued|uploading|done|failed|cancelled", "now", "error", "created", "done_bytes" }`. New clips
  (recordings with `recordings`) are queued when cloud storage is on and `upload` isn't `manual`; they go
  when no game runs (`after_game`) or right away (`now`). The last 50 finished jobs are kept.
- `POST /api/cloud/upload` body `{"id": "<clip id>"}` → `{"job": 5}`: uploads now, game or not (also a retry);
  `400` when cloud storage isn't set up or the clip is already up
- `POST /api/cloud/cancel` body `{"job": 5}` → `204`
- `POST /api/cloud/remove` body `{"id": "<clip id>"}` → `204` after deleting it from the bucket (the file on
  this PC stays); `502` with the bucket's answer; `403` from a LAN device
- `POST /api/cloud/test` body: a `cloud` object (a blank secret means the saved one) → `{"message": "..."}`
  after writing, reading back (also through `public_url`) and deleting a small file; `400` when fields are
  missing or malformed, `502` with what went wrong; `403` from a LAN device

With cloud storage on and `public_url` set, a Discord post is the clip's link (uploaded first if it isn't up
yet, waiting up to an hour); otherwise, or if that fails, the file as before.

## Files

- `GET /media/<id>` → the video file, with HTTP Range support (the `<video>` element seeks with it)
- `GET /thumbs/<id>.jpg`, `/thumbs/<id>.sprite.jpg` → images (generated after a clip is saved, low priority)

## LAN access (opt-in)

When `web.lan` is on, the server also listens on the LAN (`0.0.0.0`, same port). Saving the setting opens or
closes that listener at once, without a restart; turning it off also ends LAN event streams. If the port can't
be opened, `GET /api/system` has `"lan_error"` (a message; `null` otherwise), and `"lan_ip"` is the PC's
local network address to open (`null` when unknown). Requests from any address other than loopback must
carry the PIN: `POST /api/login {"pin"}` sets an HttpOnly cookie `clipr_session` (random token, valid 30 days);
without it every LAN request gets `401` and the UI shows a PIN screen. The PIN is `web.lan_pin` (6 digits,
generated when LAN access is first turned on, shown in Settings). Loopback requests never need it. A session ends when `web.lan_pin` changes or `web.lan` is turned off. After
5 wrong PINs from one address, logins get `429` for a minute.
