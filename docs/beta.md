# clipr beta

Thanks for testing clipr! It records your games in the background and saves the last moments when you
press a hotkey, like ShadowPlay or Medal, but open source, with no account and no game hooks.

## Start

1. Unzip anywhere and run `clipr.exe`. Windows SmartScreen may warn because the beta isn't signed:
   click **More info → Run anyway**.
2. Your browser opens the clipr page (`http://127.0.0.1:7845`). A short setup guides you through the look,
   quality, hotkeys and audio. Every step can be skipped.
3. clipr lives in the tray (the icon near the clock). Closing the browser tab doesn't stop it; use
   **Quit clipr** in the tray menu.

## Use

| What | Default |
| --- | --- |
| Save the last 30 seconds | `Alt+F10` |
| Start / stop a full recording | `Alt+F9` |
| Save the whole buffer, pause the buffer, mark a moment | off; set them under Settings → Hotkeys |

- clipr starts buffering when it sees a game, and idles otherwise. To buffer the desktop too, open
  Settings → Games.
- Clips go to `Videos\clipr\<Game>\`. The Gallery lets you play, trim, rename, favorite and delete them.
- A small "Clip saved" note appears top right. It never shows up in your clips.
- Full recordings are saved next to your clips as `<Game>_<date>_<time>_recording.mp4`. They stop by
  themselves at the time limit (Settings → Recording, off by default) or when the disk gets nearly full
  (Settings → Library). If clipr or the PC crashes mid-recording, the file is recovered the next time clipr
  starts, up to about the last second.

## If something goes wrong

Please send:
- what you did and what happened,
- the newest log from `%LOCALAPPDATA%\clipr\logs\` (paste that path into Explorer's address bar),
- your GPU and Windows version.

Known limits in this beta:
- Exclusive-fullscreen games hide the "Clip saved" note (the sound still plays). Borderless works.
- HDR games are recorded without tonemapping, so their clips can look washed out.
- Settings live in `%APPDATA%\clipr\config.toml`. Deleting that file resets everything.

To uninstall, quit clipr, delete the folder, and optionally delete `%APPDATA%\clipr` and
`%LOCALAPPDATA%\clipr`. If you turned on "Start with Windows", turn it off first (Settings → General).
