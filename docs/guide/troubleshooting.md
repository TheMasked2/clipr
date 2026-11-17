# Troubleshooting

## Where things are

| What | Where |
| --- | --- |
| Clips and recordings | `Videos\clipr\<Game>\` (or the folder under Settings → Library) |
| Settings | `%APPDATA%\clipr\config.toml` |
| Logs | `%LOCALAPPDATA%\clipr\logs\` |
| Crash reports | `%LOCALAPPDATA%\clipr\crashes\` |

Paste a path like `%LOCALAPPDATA%\clipr\logs` into Explorer's address bar to open it.

## Asking for help

Open an [issue on GitHub](https://github.com/TheMasked2/clipr/issues) with:

- what you did and what happened,
- the newest log from `%LOCALAPPDATA%\clipr\logs\`,
- your GPU and Windows version.

## Crash reports

With **Settings → General → Crash reports** on, a crash leaves a report on your PC. The next time clipr starts,
Home shows a "clipr crashed" card:

- **Report on GitHub** opens a new GitHub issue with the report filled in. Read it, then send it. Nothing is
  sent by itself.
- **Show report** and **Copy report** let you read or copy it first.
- **Show dump file** opens the folder with the minidump, which helps with crashes inside drivers. Attach it to
  the issue if you like.
- **Dismiss** deletes the report.

Your user name, PC name and profile folder are removed from the report, but read it before you send it.

## Common questions

**Nothing happens when I press the clip hotkey.**
clipr only buffers while it sees a game. Check the status at the top of the page or in the tray. If it says
idle while you play, add the game's exe under Settings → Games → **Extra games**, or switch **Mode** to
**Always**. Also check that no other program uses the same hotkey (NVIDIA, AMD and Xbox overlays often do).

**clipr starts buffering for a program that isn't a game.**
Add its exe under Settings → Games → **Blocked programs**.

**I don't see the "Clip saved" note.**
Games in exclusive fullscreen hide it; the sound still plays. Borderless or windowed mode shows it.

**My clips stutter while the game runs at a high frame rate.**
Make sure **High priority** is on (Settings → Video → Advanced). Capping the game's frame rate a little under
your monitor's refresh rate also helps.

**My voice is too quiet, too loud or noisy.**
Use the filters under Settings → Audio, and watch the Before / After meter while you tune them. If the meter
shows **CLIP**, the mic is distorting before clipr gets it: lower its input volume or boost in Windows Sound
settings.

**The mic is out of sync with the video.**
Use **Microphone offset (ms)** under Settings → Audio. A positive value moves it later.

**Starting clipr again opens the page instead of a second copy.**
That's intended: only one clipr runs at a time. Starting it again opens the running copy's page.

**The page doesn't open on my phone.**
Check that **Allow access from my local network** is on and saved (the Network tab warns if the port couldn't
be opened), that the phone is on the same network, and that Windows Firewall allows clipr. Windows asks the first time the option is on; allow it on private
networks.

**How do I reset everything?**
Quit clipr and delete `%APPDATA%\clipr\config.toml`. Your clips stay.

## Known limits

- Exclusive-fullscreen games hide the "Clip saved" note (the sound still plays).
- HDR games are recorded without tonemapping, so their clips can look washed out.
- Capture delivers up to 60 frames per second for now.
- The beta isn't code-signed yet, so Windows SmartScreen warns on install.
