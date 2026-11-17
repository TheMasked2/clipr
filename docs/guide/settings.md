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

**Simple** shows three choices:

- **Quality** (Medium): Low (8 Mbit/s), Medium (15), High (25) or Ultra (40). Higher looks better and makes
  bigger files.
- **Resolution** (1080p): 720p, 1080p, 1440p or Native (your monitor's resolution). The width follows your
  monitor's shape. Lower is lighter on the GPU and on file size.
- **Frame rate** (60): 60 fps looks smoother in fast games; 30 fps halves the work.

**Advanced** shows the same settings in detail, plus more:

- **Codec** (H.264): what the video is compressed with. Pick by who needs to play it.
  - **H.264** plays everywhere: Discord, phones, browsers.
  - **HEVC** makes smaller files at the same quality, but not every browser or phone plays it.
  - **AV1** makes the smallest files, but needs a recent GPU (RTX 40, RX 7000, Arc) and a recent player.
- **Bitrate (kbit/s)** (15000): higher looks better and makes bigger files. 15000 is about 56 MB per 30 s.
- **Frames per second** (60): capture delivers up to 60 for now.
- **Output height (px)** (1080): the width follows the monitor's shape. 0 = the monitor's own resolution.
- **Monitor** (primary): which monitor to capture: the primary one, or a specific one in Windows' order.
- **High priority** (on): let clipr's capture go first on the GPU and CPU, so a game running at uncapped FPS
  can't make your clips stutter. The game doesn't notice: clipr's GPU work is about 0.1 ms a frame.

**Per-game profiles** (none): use other video settings while a particular game runs, for example a higher
bitrate for a fast shooter. Match by the game's name or exe (`cs2.exe`), in any case. Leave a box empty to keep
the normal value. Capture restarts when the game changes.

## Replay

- **Replay buffer** (on): keep the replay buffer running while a game runs, so the save-clip hotkey has
  something to save.
- **Clip length (seconds)** (30): how far back a saved clip goes.
- **Buffer length (seconds)** (120): how much the buffer keeps. **Save long clip** saves all of it. The buffer
  lives in RAM: the page shows an estimate for your bitrate (two minutes at 15 Mbit/s is about 225 MB).
- **Sound on save** (on): play a short sound when a clip is saved. It plays through your speakers, so with
  system audio on it can be heard in a clip saved right after.
- **Note on save** (on): show a small "Clip saved" note in the top-right corner. It never shows up in your
  clips. Games in exclusive fullscreen hide it, but the sound still plays.

## Audio

The Audio page has a section per source, so everything about the microphone (volume, push-to-talk, filters) is
in one place.

### Tracks

The top of the page lists the tracks your clips get and what each one holds:

- **Track 1, Mix**: system audio and the microphone together, plus your app tracks when system audio is off
  (with it on, the apps are already in there). Players and Discord play this track, so clips sound right
  everywhere.
- **One track per app** in **App tracks**, always.
- **Separate tracks** (off): also give system audio and the microphone a track of their own.

Clips hold up to 6 tracks; a source that doesn't fit is still in the mix. The clip page lets you listen to each
track, and video editors show them all, so you can mute voice chat or your mic afterwards.

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
- **Filters**: see [Filters](#filters).

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

## Games

Games and programs are named by their **exe name**, like `cs2.exe`. To find one, open Task Manager while the game
runs, right-click it and choose **Go to details**: the highlighted name is the exe.

- **Mode** (Game only): **Game only** buffers only while a known game runs and does nothing otherwise.
  **Desktop** buffers your monitor all the time, game or not.
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

## Discord

Post clips to Discord channels through webhooks. See the [Discord guide](discord.md) for the step-by-step
setup and what each option does.

## Network

- **Allow access from my local network** (off): open the clipr page to other devices at home, like your phone.
  They have to enter a PIN, which is shown here after you save, together with the address to open (this PC's
  address on your network, like `http://192.168.1.20:7845`). Anyone on
  your network with the PIN can see and delete your clips, and the connection isn't encrypted, so only turn
  this on at home. Turning it on or off works right away.
- **First port** (7845): the first port to try for the page. If it's taken, clipr uses the next free one.
  Applies after a restart.

## Appearance

- **Accent colour** (purple): the one colour used for buttons, highlights and the timeline.
- **Theme** (Dark sleek): more looks will follow.
