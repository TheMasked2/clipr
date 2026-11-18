# Every setting explained

Open **Settings** at the top of the clipr page. Changes take effect when you click **Save**; **Revert** throws
them away. Defaults are given in brackets. Settings are stored in `%APPDATA%\clipr\config.toml`, and deleting
that file resets everything.

- [General](#general) · [Video](#video) · [Replay](#replay) · [Audio](#audio) · [Hotkeys](#hotkeys) ·
  [Games](#games) · [Library](#library) · [Recording](#recording) · [Discord](#discord) ·
  [Network](#network) · [Appearance](#appearance)

## General

- **Start with Windows** (off): start clipr when you sign in to Windows. It starts quietly in the tray.
- **Open the UI on start** (on): open the clipr page when you start clipr yourself, so you can see it's running.
  It never opens when Windows starts clipr.
- **Update automatically** (off): check GitHub releases once each time clipr starts (like Steam) and install a
  new version by itself, only when no game runs and nothing records. Switching it on takes effect at the next
  start; **Check now** works any time.
- **Beta versions** (on in beta builds): offer beta versions too, not only full releases.
- **Updates panel:** shows your version and the newest one, with **Check now** and **Update now** (or
  **Download** for the portable version). Installing only works on the PC itself, not from your phone.
- **Crash reports** (off): if clipr crashes, keep a report and a minidump on this PC and show them on Home next
  time, so you can file them as a GitHub issue. Your user name, PC name and profile folder are removed, and
  nothing is ever sent by itself. See [Troubleshooting](troubleshooting.md#crash-reports).

## Video

**Preview** at the top shows what clipr captures right now, 5 frames a second by default; pick 2, 15 or 30 in its
corner (Home shows it too, with a **Hide** link). It only runs while the buffer or a recording does, and only while a page shows it, so it costs
nothing otherwise. Other devices (LAN access) don't get it unless you turn on **Live preview on other devices**
under Network, because it shows your whole screen.

**Simple** shows three choices:

- **Quality** (Medium): Low (8 Mbit/s), Medium (15), High (25) or Ultra (40). Higher looks better and makes
  bigger files.
- **Resolution** (1080p): 720p, 1080p, 1440p or Native (your monitor's resolution). The width follows your
  monitor's shape. Lower is lighter on the GPU and on file size.
- **Frame rate** (60): 60 fps looks smoother in fast games; 30 fps halves the work.

clipr remembers whether you last used Simple or Advanced. Below the settings it tells you when clips come out
smaller than your monitor (downscaled). It never upscales: an output taller than the monitor records at the
monitor's own size.

**Advanced** shows the same settings in detail, plus more:

- **Codec** (H.264): what the video is compressed with. Pick by who needs to play it.
  - **H.264** plays everywhere: Discord, phones, browsers.
  - **HEVC** makes smaller files at the same quality, but not every browser or phone plays it.
  - **AV1** makes the smallest files, but needs a recent GPU (RTX 40, RX 7000, Arc) and a recent player.
- **Bitrate (kbit/s)** (15000): higher looks better and makes bigger files. 15000 is about 56 MB per 30 s.
- **Frames per second** (60): capture delivers up to 60 for now.
- **Output height (px)** (1080): the width follows the monitor's shape. 0 = the monitor's own resolution.
- **Monitor** (primary): which monitor to capture: the primary one, or a specific one in Windows' order.
- **GPU priority** (off): let clipr's capture go a little ahead of the game on the GPU. Try it if clips
  stutter: it helps on some PCs and makes clips skip frames on others, so try it both ways.

**Per-game profiles** (none): use other video settings while a particular game runs, for example a higher
bitrate for a fast shooter. Match by the game's name or exe (`cs2.exe`), in any case. Leave a box empty to keep
the normal value. Capture restarts when the game changes.

## Replay

- **Replay buffer** (on): keep the replay buffer running while a game runs, so the save-clip hotkey has
  something to save.
- **Clip length (seconds)** (30): how far back a saved clip goes.
- **Buffer length (seconds)** (120): how much the buffer keeps. **Save long clip** saves all of it. The buffer
  lives in RAM: the page shows an estimate for your bitrate (two minutes at 15 Mbit/s is about 225 MB).
- **Sound on save** (on): play a short chime when a clip is saved, or a low tone when saving fails. It plays
  through your speakers, so with system audio on it can be heard in a clip saved right after.
- **Sound volume** (60 %): how loud the save sound and the mic mute blip play (the blip plays even with
  **Sound on save** off). **Test** plays it at the
  volume on the slider, before you save. 0 % is silent.
- **Note on save** (on): show a small "Clip saved" note in the top-right corner. It never shows up in your
  clips. Games in exclusive fullscreen hide it, but the sound still plays.

## Audio

The Audio page has a section per source, so everything about the microphone (volume, push-to-talk, filters) is
in one place.

### Tracks

The top of the page lists the tracks your clips get and what each one holds:

- **Track 1, Mix**: system audio and the microphone together, plus the game and your app tracks when system
  audio is off (with it on, they are already in there). Players and Discord play this track, so clips sound right
  everywhere.
- **Game**, when **Game audio** is on.
- **One track per app** in **App tracks**, always.
- **Separate tracks** (off): also give system audio and the microphone a track of their own.

Clips hold up to 6 tracks; a source that doesn't fit is still in the mix. The clip page lets you listen to each
track, and video editors show them all, so you can mute voice chat or your mic afterwards. The free editor
[Kdenlive](https://kdenlive.org) puts each track on its own audio lane when you drop a clip in.

**Live levels** shows what each source records right now, after its filters, push-to-talk and volume, from -60
to 0 dB (hover the number to see the level before the filters). **CLIP** means the source is already distorting
before clipr gets it: lower its level in Windows (for a mic: Windows Sound settings → input volume or boost).
The meters show while the buffer runs and follow the saved settings.

### System audio

- **Record system audio** (on): record what you hear (game, voice chat, music) from your default output device.
- **Volume** (100 %): 0 to 200 %, applied after the filters. 100 % leaves it unchanged.
- **Filters**: see [Filters](#filters).

### Microphone

- **Record the microphone** (off): record your default microphone.
- **Volume** (100 %): 0 to 200 %, applied after the mic's filters.
- **Push-to-talk key** (empty): record the mic only while this key is held, for example the push-to-talk key of
  your voice chat, like `V` or `Mouse4`. Empty means the mic is always recorded.
- **Push-to-talk release (ms)** (200): keep the mic open this long after you let go, so the end of a word isn't
  cut off. Shows once a push-to-talk key is set.
- **Mute in clips:** **Mute now** silences your mic in clips (not in your voice chat) until you unmute it.
  The **Mute / unmute microphone** hotkey does the same, with a short blip, and the top bar shows **Mic
  muted** while it is. clipr always starts unmuted.
- **Filters**: see [Filters](#filters).

### Game audio

- **Game on its own track** (off): also record only the game's sound (no voice chat, no music) on a track of
  its own, so you can turn it up or down later in an editor. It follows whichever game clipr detects (if
  the game is also listed under **App tracks**, that track has it instead, so it isn't recorded twice). Needs
  Windows 10 version 2004 or newer. The game has its own filter list.

### App tracks

- **Apps on their own track** (none): record these programs separately, up to 4, like `discord.exe`. Each gets
  a track of its own, so you can mute voice chat later in a video editor. Needs Windows 10 version 2004 or
  newer. Each app has its own filter list below it.

### Audio out of sync with the video?

Tucked away at the bottom, because you normally don't need it:

- **Microphone offset (ms)** (0) and **System audio offset (ms)** (0): shift a source in time if it plays early
  or late in your clips. Positive = later.

### Filters

Each recorded source has its own filter list, like in OBS. There are none by default: add them with **Add a
filter…**, switch each one on or off, reorder them with ↑ ↓ (they run top to bottom), or remove them with ×.

Above each list, a **Before / After** meter shows the source's live level before its filters and as it is
recorded (after the filters, push-to-talk and volume), so you can see what a filter does while you tune it.
The After meter follows the saved settings: click **Save** to hear a change.

- **Noise suppression:** removes steady background noise like fans, hum and keyboards. **Strength** (100 %):
  lower keeps more of the original sound.
- **Noise gate:** mutes the source while it's quiet. It opens above the **Open level** (-32 dB) and closes
  below the **Close level** (-40 dB), after the **Hold** time (150 ms). **Attack** (10 ms) and **Release**
  (100 ms) set how fast it opens and closes. Set the levels against the meter: open just above your
  background noise.
- **Compressor:** evens out loud and quiet parts. Above the **Threshold** (-20 dB), every **Ratio** (3:1) dB in
  comes out as 1 dB. **Attack** (5 ms), **Release** (80 ms), and **Output gain** (+4 dB) to make up for the
  lost loudness.
- **Limiter:** never lets a peak past the **Ceiling** (-1 dB), so shouting or an explosion doesn't distort.
  **Release** (100 ms). Put it last, and keep the source volume at 100 % or lower, because the volume applies
  after the filters.
- **Gain:** turns the source up or down, -30 to +30 dB (0).

A good start for a microphone: Noise suppression, then Noise gate, then Compressor, then Limiter.

### Quality

- **Audio quality (AAC)** (160 kbit/s per track): 160 is plenty for game audio.

## Hotkeys

Click a box and press the combination you want (Esc cancels). The × next to a hotkey clears it, which turns
it off. A hotkey turns red when two actions share the same combination.

- **Save clip** (`Alt+F10`): saves the last clip-length seconds.
- **Save long clip** (off): saves the whole buffer.
- **Pause / resume buffer** (off): pauses or resumes the replay buffer.
- **Start / stop recording** (`Alt+F9`): starts or stops a full recording.
- **Mark moment** (off): marks this moment. Markers show on the clip's timeline.
- **Mute / unmute microphone** (off): mutes your mic in clips (not in your voice chat), with a short blip.
  Press it again to unmute.

## Games

Games and programs are named by their **exe name**, like `cs2.exe`. To find one, open Task Manager while the game
runs, right-click it and choose **Go to details**: the highlighted name is the exe.

- **Mode** (Game only): **Game only** buffers only while a known game runs and does nothing otherwise.
  **Desktop** buffers your monitor all the time, game or not.
- **Capture** (Game window, Game only mode): **Game window** records only the game, so alt-tabbing,
  notifications and your other screens stay out of clips. **Whole monitor** records the monitor the game is on;
  pick it if a game's clips come out black. The tab also shows which game clipr detects right now.
- **Extra games** (none): games to detect by exe name (`mygame.exe`), on top of the known list.
- **Blocked programs** (none): exe names never treated as games, even if the known list has them. Use this
  when clipr starts buffering for a program that isn't a game.
- **Auto-record** (none): start a full recording by itself when one of these games starts (exe names).

## Library

- **Clips folder** (empty = `Videos\clipr`): where clips go. Each game gets its own subfolder.
- **Delete after (days)** (0 = keep forever): delete clips older than this. Favorites are never deleted.
- **Maximum total size (GB)** (0 = no limit): keep the clips folder under this size by deleting the oldest
  clips. Favorites are never deleted.
- **Minimum free space (GB)** (5): stop recording and warn when the clips drive has less than this free.
- **Clean up now:** runs the age and size limits on the saved settings right away. It also runs every hour.

The Home page shows your newest clip, how many clips you have, how much space is free and how much your
clips use.

## Recording

- **Maximum length (minutes)** (0 = no limit): stop a full recording after this many minutes.

While a recording runs it's written so that a crash or power cut keeps everything up to the last second. When
it stops, clipr adds an index to the file (a second or two, even for hours of video), so it opens and seeks at
once in the gallery and in editors. Recordings made with beta.3 or older get that index the next time clipr
starts.

## Discord

**Show in my Discord status** (on): while clipr clips or records, your Discord profile shows the game and its
icon, like *VALORANT · Clipping with clipr* (the member list says *Playing VALORANT*), with the time since clipr started clipping the game
(or since the recording started, while one runs). A custom game shows its program name (like `emulator`), never
its window title. Recording the desktop shows nothing. It talks to the Discord app on this PC, so nothing goes online and there's nothing to sign in
to; with Discord closed it does nothing.

**Discord account:** **Connect Discord** signs you in on Discord's own page and shows who's connected. No
password or token is kept. Only on the PC running clipr.

Post clips to Discord channels through webhooks: **Pick a channel in Discord** lets you choose one in Discord's
own picker, **Paste a webhook URL** adds one by hand. See the [Discord guide](discord.md) for the step-by-step
setup and what each option does.

## Cloud

Copy clips to your own bucket on Cloudflare R2, Backblaze B2, Amazon S3 or another S3-compatible service. See
the [cloud storage guide](cloud.md) for the step-by-step setup.

- **Service** (Cloudflare R2): picks the setup steps and fills in the region (`auto` on R2).
- **Endpoint, Region, Bucket, Access key ID, Secret access key:** where clips go and the key that may write
  there. Only on the PC running clipr; other devices never see the secret.
- **Public address** (empty): the bucket's public link (`https://pub-….r2.dev` or your own domain). Discord
  gets links under it; without one, Discord gets the file.
- **Folder in the bucket** (`clipr/`): clips go to `<folder>/<game>/<file>`.
- **Test:** writes a small file, reads it back (and through the public address) and deletes it.
- **Copy clips to the cloud** (off): needs the fields above.
- **When to upload** (After the game): after the game, right away, or only by hand.
- **Speed limit** (0 = no limit): in Mbit/s.
- **Upload recordings too** (off): full recordings can be gigabytes.

## Network

- **Allow access from my local network** (off): open the clipr page to other devices at home, like your phone.
  They have to enter a PIN, which is shown here after you save, together with the address to open (this PC's
  address on your network, like `http://192.168.1.20:7845`). Anyone on
  your network with the PIN can see and delete your clips, and the connection isn't encrypted, so only turn
  this on at home. Turning it on or off works right away.
- **First port** (7845): the first port to try for the page. If it's taken, clipr uses the next free one.
  Applies after a restart.

**Away from home:** clipr doesn't open itself to the internet, and you shouldn't forward its port on your router
(the connection isn't encrypted). To reach it from outside, put your phone and PC on a private network such as
[Tailscale](https://tailscale.com), turn on local network access, and open the PC's address in that network. To
share single clips, use [Cloud](#cloud).

## Appearance

- **Accent colour** (purple): the one colour used for buttons, highlights and the timeline.
- **Theme** (Dark sleek): more looks will follow.
