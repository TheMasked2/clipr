# Getting started

## Install

Download the newest `clipr-<version>-setup.exe` from the
[releases page](https://github.com/TheMasked2/clipr/releases) and run it. It installs for your Windows user
only, so it doesn't need admin rights, and adds clipr to the Start menu. There's also a portable zip: unzip it
anywhere and run `clipr.exe`.

The beta isn't code-signed yet, so Windows SmartScreen may warn you. Click **More info → Run anyway**.

You need Windows 10 (version 1903 or newer) or Windows 11, and a GPU with a hardware video encoder: NVIDIA,
AMD or Intel, which almost every GPU from the last ten years has. Recording single apps on their own audio
track needs Windows 10 version 2004 or newer. To see your version, press `Win+R`, type `winver` and press
Enter.

## First run

clipr opens its page in your browser: `http://127.0.0.1:7845`. A short setup asks about the look, the video
quality, your clip hotkey and audio. Every step can be skipped, and everything can be changed later under
**Settings**.

After that, clipr lives in the **tray** (the icons near the clock). Closing the browser tab doesn't stop it.
Right-click the tray icon for:

- a status line (for example "● Buffering Valorant"),
- **Save clip** and **Start recording**, with their hotkeys,
- **Pause buffer**,
- **Open clipr**, **Gallery**, **Open clips folder** and **Settings**,
- **Quit clipr**.

To open the page again later, use **Open clipr** in the tray menu.

## Saving clips

clipr starts buffering when it sees a game and stays idle otherwise: no capture, no GPU work. It recognises
games from Discord's public list of games. You can add your own or block false hits under
[Settings → Games](settings.md#games).

While a game runs, clipr keeps the last two minutes in memory (the **replay buffer**). Press the hotkey to save
part of it:

| What | Default hotkey |
| --- | --- |
| Save the last 30 seconds | `Alt+F10` |
| Start or stop a full recording | `Alt+F9` |
| Save the whole buffer ("long clip") | off |
| Pause or resume the buffer | off |
| Mark a moment | off |

Set or change them under [Settings → Hotkeys](settings.md#hotkeys). The **Home** page has the same actions as
buttons, plus an overview like OBS's main window:

- a status card that shows whether clipr is buffering or recording, and the live preview;
- **Capture**: what to record (games only, or always) and the quality, resolution and frame rate, one click
  each. These save at once and restart capture, so the replay buffer starts over;
- **Audio**: the mixer, with each source's live level and a volume fader (see
  [Settings → Audio](settings.md#audio));
- **Performance**: the frame rate against your target, how many frames had a new picture, the time clipr spends
  per frame, the video bitrate, clipr's CPU and memory, each with the last two minutes as a small graph (hover it
  for the numbers, or click **Show numbers** for a table). Late or skipped frames in the last minute are named
  below, with what to try.

When a clip is saved you hear a short sound and see a small "Clip saved" note in the top-right corner. The note
never shows up in your clips. The sound plays through your speakers like any other sound, so with system audio
on it can be heard in a clip you save right after. Both can be turned off under
[Settings → Replay](settings.md#replay).

Clips go to `Videos\clipr\<Game>\` unless you pick another folder.

## The gallery

**Gallery** lists your clips grouped by game and day. Hover over a thumbnail to scrub through the clip. The
filters sit in a column on the left, like Settings' sections (on a phone, in a row above the clips): search by
title, show clips, recordings or favorites, pick a game (with how many clips each has) or one of your shelves.

The player remembers its volume and mute from clip to clip, on each device.

Open a clip to:

- play it, and switch audio tracks if you recorded several (see
  [Tracks](settings.md#tracks)),
- **trim** it: drag the two handles on the timeline, then **Save as new clip** or **Replace original**.
  Trimming is lossless and takes a moment, not a re-encode,
- rename it, mark it as a favorite (★), or put it on **shelves** (your own named collections),
- see the moments you marked, as markers on the timeline,
- **Share to Discord** (see [Discord](discord.md)), **Show in folder** or **Delete** it.

Favorites are never removed by the automatic cleanup.

## Full recordings

A full recording runs next to the replay buffer and keeps everything until you stop it. Start and stop one with
`Alt+F9`, the tray menu or the Home page. clipr can also start one by itself when a particular game starts
([Settings → Games → Auto-record](settings.md#games)).

Recordings are saved next to your clips as `<Game>_<date>_<time>_recording.mp4`. They stop by themselves at the
time limit ([Settings → Recording](settings.md#recording), off by default) or when the disk gets nearly full
([Settings → Library](settings.md#library)). If clipr or the PC crashes mid-recording, the file is recovered the
next time clipr starts, up to about the last second.

## From your phone

clipr's page can be opened from other devices at home, protected by a PIN:

1. On the PC, open **Settings → Network**, turn on **Allow access from my local network** and click **Save**.
   It works right away. The Network tab now shows the PIN and the address to open, like
   `http://192.168.1.20:7845`.
2. Open that address on your phone and enter the PIN.

Turning it off closes the access at once, and pages open on other devices stop updating.

Anyone on your network with the PIN can see and delete your clips, and the connection isn't encrypted, so only
do this on a network you trust. A few things only work on the PC itself: pasting a Discord webhook URL,
changing the clips folder or the Network settings, installing an update, and opening Explorer.

## Updates

**Settings → General** shows your version and can check for a new one. Updates are opt-in: with **Update
automatically** on, clipr checks GitHub once each time it starts, like Steam, and installs a new version by
itself, but only when no game runs and nothing records. It doesn't check again until the next start. Otherwise click **Update now** when you like; it waits while a game runs or a
recording is on. Installing takes a few seconds, and clipr starts again by itself.

## Uninstall

Use Windows **Settings → Apps → clipr → Uninstall**. Your clips are kept; the uninstaller asks whether to remove
clipr's settings too.

For the portable zip: quit clipr, delete its folder, and optionally delete `%APPDATA%\clipr` (settings) and
`%LOCALAPPDATA%\clipr` (logs and the clip library index). If you turned on **Start with Windows**, turn it off
first under Settings → General.
