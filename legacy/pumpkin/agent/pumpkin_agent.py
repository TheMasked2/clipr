# /// script
# requires-python = ">=3.11"
# dependencies = ["aiohttp>=3.9", "PyAudioWPatch>=0.2.12; sys_platform == 'win32'"]
# ///
"""Pumpkin PC agent: records your screen into a rolling 2-minute buffer while you Go Live in Discord, and hands
Pumpkin the matching video when someone in the call says "pumpkin, clip that".

The owner runs it from the repo (agent.ps1: deploy/server.txt, its key and PUMPKIN_AGENT_TOKEN). Anyone else
double-clicks pumpkin-agent.cmd: the first run makes an SSH key, they pair it with /agent link and paste the code
back. Either way it reaches Pumpkin through an SSH tunnel that can only reach the agent port, so no ports are opened
anywhere. Pumpkin tells it when you start and stop streaming; it records nothing otherwise.
It starts by itself, hidden, when you sign in to Windows (/agent autostart in Discord turns that off), only one
copy runs at a time (starting it again replaces the old one), and a friend's copy updates itself when Pumpkin has
a newer version. It writes what it does to pumpkin-agent.log next to this file.
Capture: Windows Graphics Capture (gfxcapture, else Desktop Duplication / ddagrab) at 720p30, encoded by the graphics card (AMD AMF, NVIDIA NVENC or
Intel QSV, whichever works), 2-second pieces that overwrite themselves. Sound: what your PC plays (the game, and
the call as you hear it), from the default output device, kept in memory for the same 2 minutes. Pumpkin adds only
your own mic on top, since that's the one thing your PC doesn't play back.
"""

import asyncio
import hashlib
import json
import logging
import logging.handlers
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import wave
from collections import deque
from pathlib import Path

import aiohttp

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
FRIEND_KEY = HERE / "pumpkin-agent-key"
FRIEND_CONFIG = HERE / "pumpkin-agent.json"
PID_FILE = HERE / "pumpkin-agent.pid"
LOG_FILE = HERE / "pumpkin-agent.log"
NO_AUTOSTART = HERE / "pumpkin-agent.noautostart"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_NAME = "Pumpkin agent"
BACKGROUND = "--background" in sys.argv
VERSION = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()[:12]
RING = Path(tempfile.gettempdir()) / "pumpkin-ring"
PORT = 8765
SEGMENT_S = 2
KEEP_S = 130
STALE_S = 10  # no new piece for this long while live: ffmpeg died or froze, start it again
FFMPEG_LOG = Path(tempfile.gettempdir()) / "pumpkin-agent-ffmpeg.log"
# Bytes a second for a clip upload, under what a phone hotspot sends, so the game's ping stays put.
# PUMPKIN_AGENT_UPLOAD_KBPS (kilobits) sets your own when your upload is faster or slower.
UPLOAD_RATE = int(os.environ.get("PUMPKIN_AGENT_UPLOAD_KBPS", "5000")) * 1000 // 8
CHUNK = 256 * 1024
MORE = 1 << 63  # set on every piece of a clip except the last
MAX_BYTES = 5_000_000  # well under Discord's 10 MB: half the size is half the upload (and the wait)
# "pumpkin, clip that" saves the moment here at full quality; it's encoded properly and uploaded once you stop
# streaming (or the next time the agent runs), then deleted. PUMPKIN_AGENT_SAVE_KBPS sets the quality.
SAVED = HERE / "pumpkin-agent-saved"
# Saved clips go up once no game has been running for this long (like Medal's "When Game Closes"): uploading while
# you play costs ping. Games are recognised with Discord's own list of detectable games, like the OBS game watcher.
GAME_LIST_URL = "https://discord.com/api/v9/applications/detectable"
GAME_LIST = HERE / "pumpkin-agent-games.json"
GAME_GONE_S = 30
MIN_FREE = 2 * 1024 ** 3  # saved clips need room on the disk: below this, say so instead of filling it
NOT_GAMES = {"discord.exe", "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "battle.net.exe",
             "eadesktop.exe", "galaxyclient.exe", "ubisoftconnect.exe", "riot client.exe", "riotclientservices.exe",
             "spotify.exe", "obs64.exe", "explorer.exe", "medal.exe", "sh.exe", "bash.exe", "git.exe", "gh.exe"}
SAVE_KBPS = int(os.environ.get("PUMPKIN_AGENT_SAVE_KBPS", "0"))  # 0: 8000 at 1080p, 4000 at 720p
ENCODERS = ["h264_amf", "h264_nvenc", "h264_qsv", "libx264"]
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)

log = logging.getLogger("agent")


def screen_mode(monitor: int) -> tuple | None:
    """(width, height, refresh rate) of every monitor on the desktop right now, or None when Windows can't say.
    All of them, not just the captured one: which number capture gives a monitor isn't certain."""
    if sys.platform != "win32":
        return None
    import ctypes
    from ctypes import wintypes

    class DISPLAY_DEVICEW(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("DeviceName", wintypes.WCHAR * 32), ("DeviceString", wintypes.WCHAR * 128),
                    ("StateFlags", wintypes.DWORD), ("DeviceID", wintypes.WCHAR * 128),
                    ("DeviceKey", wintypes.WCHAR * 128)]

    class DEVMODEW(ctypes.Structure):
        _fields_ = [("dmDeviceName", wintypes.WCHAR * 32), ("dmSpecVersion", wintypes.WORD),
                    ("dmDriverVersion", wintypes.WORD), ("dmSize", wintypes.WORD), ("dmDriverExtra", wintypes.WORD),
                    ("dmFields", wintypes.DWORD), ("dmPositionX", wintypes.LONG), ("dmPositionY", wintypes.LONG),
                    ("dmDisplayOrientation", wintypes.DWORD), ("dmDisplayFixedOutput", wintypes.DWORD),
                    ("dmColor", ctypes.c_short), ("dmDuplex", ctypes.c_short), ("dmYResolution", ctypes.c_short),
                    ("dmTTOption", ctypes.c_short), ("dmCollate", ctypes.c_short), ("dmFormName", wintypes.WCHAR * 32),
                    ("dmLogPixels", wintypes.WORD), ("dmBitsPerPel", wintypes.DWORD), ("dmPelsWidth", wintypes.DWORD),
                    ("dmPelsHeight", wintypes.DWORD), ("dmDisplayFlags", wintypes.DWORD),
                    ("dmDisplayFrequency", wintypes.DWORD)]

    user32 = ctypes.windll.user32
    modes, i = [], 0
    while True:
        dev = DISPLAY_DEVICEW()
        dev.cb = ctypes.sizeof(dev)
        if not user32.EnumDisplayDevicesW(None, i, ctypes.byref(dev), 0):
            break
        mode = DEVMODEW()
        mode.dmSize = ctypes.sizeof(mode)
        if dev.StateFlags & 1 and user32.EnumDisplaySettingsW(dev.DeviceName, -1, ctypes.byref(mode)):
            modes.append((mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency))
        i += 1
    return tuple(modes) or None


def find_ffmpeg() -> str:
    found = os.environ.get("PUMPKIN_FFMPEG") or shutil.which("ffmpeg")
    if found:
        return found
    packages = Path(os.environ.get("LOCALAPPDATA", "")) / "Microsoft" / "WinGet" / "Packages"
    for exe in sorted(packages.glob("Gyan.FFmpeg*/*/bin/ffmpeg.exe"), reverse=True):
        return str(exe)
    sys.exit("ffmpeg not found: winget install Gyan.FFmpeg")


def encoder_args(encoder: str, kbps: int, best: bool = False) -> list[str]:
    """`best`: no hurry (a saved clip after the stream), so the slower settings that look better per byte."""
    if encoder == "h264_amf":
        if best:
            return ["-c:v", encoder, "-usage", "transcoding", "-quality", "quality", "-rc", "vbr_peak",
                    "-b:v", f"{kbps}k", "-maxrate", f"{kbps * 3 // 2}k"]
        return ["-c:v", encoder, "-usage", "lowlatency", "-rc", "cbr", "-b:v", f"{kbps}k", "-bf", "0",
                "-forced_idr", "1"]
    if encoder == "libx264":
        if best:
            return ["-c:v", encoder, "-preset", "veryfast", "-b:v", f"{kbps}k", "-maxrate", f"{kbps * 3 // 2}k",
                    "-bufsize", f"{kbps * 2}k"]
        return ["-c:v", encoder, "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", f"{kbps}k"]
    return ["-c:v", encoder, "-b:v", f"{kbps}k", "-bf", "0", "-forced_idr", "1"]


GENTLE = getattr(subprocess, "BELOW_NORMAL_PRIORITY_CLASS", 0)  # the game goes first, the clip right after
SPARE_THREADS = str(max(1, (os.cpu_count() or 4) // 4))


def run(cmd: list[str], gentle: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, creationflags=NO_WINDOW | (GENTLE if gentle else 0))


class Sound:
    """What the PC plays (WASAPI loopback of the default output device), or with `mic` your default microphone,
    the last KEEP_S seconds in memory. The mic is recorded here, in step with the screen, because Discord's copy
    arrives late; Pumpkin only keeps it where Discord shows you were actually talking (so never while muted).
    Chunks are laid end to end (their arrival times wobble by milliseconds, and placing each by its own time
    crackles); only a real pause, when loopback hands over nothing because nothing plays, starts a new run, and
    the pause becomes silence when a clip is cut."""

    def __init__(self, mic: bool = False):
        self.mic = mic
        self.chunks: deque[tuple[float, bytes]] = deque()
        self.rate, self.channels = 48000, 2
        self.pa = self.stream = None
        self.lock = threading.Lock()
        self.next_t = 0.0  # where the next chunk starts, if it follows on

    def start(self) -> None:
        if self.stream is not None:
            return
        try:
            import pyaudiowpatch as pyaudio
            self.pa = pyaudio.PyAudio()
            if self.mic:
                wasapi = self.pa.get_host_api_info_by_type(pyaudio.paWASAPI)
                device = self.pa.get_device_info_by_index(wasapi["defaultInputDevice"])
            else:
                device = self.pa.get_default_wasapi_loopback()  # picked again every time you go live
            self.rate = int(device["defaultSampleRate"])
            self.channels = min(2, int(device["maxInputChannels"])) if self.mic else int(device["maxInputChannels"])
            self.stream = self.pa.open(format=pyaudio.paInt16, channels=self.channels, rate=self.rate, input=True,
                                       input_device_index=device["index"], frames_per_buffer=1024,
                                       stream_callback=self._heard)
            log.info("recording %s (%s)", "your mic" if self.mic else "PC sound", device["name"])
        except Exception as e:  # no device, or the library is missing: clips just go without it
            log.warning("can't record %s: %s", "your mic" if self.mic else "PC sound", e)
            self.stop()

    def _heard(self, data, frames, info, status):
        now = time.time()
        arrived = now - frames / self.rate
        start = self.next_t if abs(arrived - self.next_t) < 0.2 else arrived  # follows on, or a pause / drift
        self.next_t = start + frames / self.rate
        with self.lock:
            self.chunks.append((start, data))
            while self.chunks and self.chunks[0][0] < now - KEEP_S:
                self.chunks.popleft()
        return None, 0  # paContinue

    def stop(self) -> None:
        for close in (lambda: self.stream.stop_stream(), lambda: self.stream.close(), lambda: self.pa.terminate()):
            try:
                close()
            except Exception:
                pass
        self.pa = self.stream = None
        with self.lock:
            self.chunks.clear()  # nothing stays once you stop streaming

    def wav(self, path: Path, end: float, seconds: float) -> bool:
        """The `seconds` before `end` (a time.time()) as a wav. False when there's no PC sound at all."""
        if self.stream is None:
            return False
        frame = 2 * self.channels
        total = int(seconds * self.rate)
        out = bytearray(total * frame)
        start = end - seconds
        with self.lock:
            chunks = list(self.chunks)
        for t, data in chunks:
            n = len(data) // frame
            at = int(round((t - start) * self.rate))
            lo, hi = max(at, 0), min(at + n, total)
            if lo < hi:
                out[lo * frame:hi * frame] = data[(lo - at) * frame:(hi - at) * frame]
        with wave.open(str(path), "wb") as w:
            w.setnchannels(self.channels)
            w.setsampwidth(2)
            w.setframerate(self.rate)
            w.writeframes(bytes(out))
        return True


class Recorder:
    def __init__(self, ffmpeg: str, monitor: int):
        self.ffmpeg, self.monitor = ffmpeg, monitor
        self.proc: subprocess.Popen | None = None
        self.wanted = False  # Pumpkin said you're live
        self.sound = Sound()
        self.mic = Sound(mic=True)
        self.restarts = 0
        self.run = 0  # each ffmpeg start writes its own pieces, so a restart keeps what came before
        self.lock = threading.Lock()  # a restart deletes a piece; never while a clip is copying them
        self.lengths: dict[tuple[str, float, int], float] = {}
        probe = Path(ffmpeg).with_name("ffprobe" + Path(ffmpeg).suffix)
        self.ffprobe = str(probe) if probe.exists() else shutil.which("ffprobe")
        self.encoder = self._pick_encoder()
        self.source = self._pick_source()
        # Always 16:9 (stretched 4:3 is stretched to fill it): 1080p when the graphics card encodes, else 720p so
        # encoding on the processor doesn't cost frames in game.
        self.height = 1080 if self.encoder != "libx264" else 720
        self.width = self.height * 16 // 9
        self.mode: tuple | None = None  # the monitor's display mode when capture started
        log.info("recording at %dx%d", self.width, self.height)

    @property
    def save_kbps(self) -> int:
        return SAVE_KBPS or (8000 if self.height >= 1080 else 4000)

    def _pick_source(self) -> str:
        """Windows Graphics Capture (what OBS and the Xbox Game Bar use; it doesn't lose the screen when a game
        changes display mode the way Desktop Duplication does) when this ffmpeg has it, else Desktop Duplication."""
        wgc = f"gfxcapture=monitor_idx={self.monitor}:max_framerate=30"
        test = run([self.ffmpeg, "-hide_banner", "-loglevel", "error", "-filter_complex",
                    f"{wgc},hwdownload,format=bgra", "-frames:v", "3", "-f", "null", "-"])
        if test.returncode == 0:
            log.info("capture: Windows Graphics Capture")
            return wgc
        log.info("capture: Desktop Duplication (this ffmpeg has no gfxcapture)")
        return f"ddagrab=output_idx={self.monitor}:framerate=30"

    def _pick_encoder(self) -> str:
        """The first hardware encoder that takes a frame on this PC (games barely notice those)."""
        for encoder in ENCODERS:
            test = run([self.ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                        "color=black:s=1280x720:r=30", "-frames:v", "5", "-vf", "format=nv12",
                        *encoder_args(encoder, 2000), "-f", "null", "-"])
            if test.returncode == 0:
                log.info("encoder: %s", encoder)
                return encoder
        sys.exit("no working h264 encoder in this ffmpeg")

    @property
    def running(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def start(self) -> None:
        self.wanted = True
        if self.running:
            return
        RING.mkdir(parents=True, exist_ok=True)
        for old in RING.glob("seg*.ts"):
            old.unlink(missing_ok=True)
        self._launch()
        self.sound.start()
        self.mic.start()
        log.info("recording the screen (monitor %s)", self.monitor)

    def _launch(self) -> None:
        # ffmpeg's errors go to a file: an unread pipe fills up after a few minutes and freezes it.
        with open(FFMPEG_LOG, "wb") as errors:
            self.proc = subprocess.Popen(
            [self.ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
             f"{self.source},hwdownload,format=bgra",
             "-vf", f"scale={self.width}:{self.height},setsar=1,format=nv12",
             *encoder_args(self.encoder, 10000 if self.height >= 1080 else 6000), "-g", "60",
             "-force_key_frames", f"expr:gte(t,n_forced*{SEGMENT_S})",
             "-f", "segment", "-segment_time", str(SEGMENT_S), "-segment_wrap", str(KEEP_S // SEGMENT_S),
             "-reset_timestamps", "1", str(RING / f"seg{self.run:04d}_%03d.ts")],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=errors, creationflags=NO_WINDOW)
        self.started = time.time()
        self.mode = screen_mode(self.monitor)

    def pieces(self) -> list[Path]:
        """Every piece still in the buffer, oldest first, across restarts."""
        now = time.time()
        found = []
        for piece in RING.glob("seg*.ts"):
            try:
                mtime = piece.stat().st_mtime
            except OSError:
                continue
            if now - mtime < KEEP_S:
                found.append((mtime, piece))
            else:
                piece.unlink(missing_ok=True)  # an older run's piece that nothing overwrites anymore
        return [piece for _, piece in sorted(found)]

    def newest(self) -> float:
        return max((p.stat().st_mtime for p in RING.glob("seg*.ts")), default=0.0)

    def check(self) -> str | None:
        """While live, make sure ffmpeg is still writing pieces; restart it if it died or froze (a game going
        fullscreen, a resolution change or a UAC prompt can kill screen capture). Returns why it restarted."""
        if not self.wanted or not self.lock.acquire(blocking=False):
            return None
        try:
            return self._check()
        finally:
            self.lock.release()

    def _check(self) -> str | None:
        stale = STALE_S if self.source.startswith("ddagrab") else 120  # WGC: a still screen sends no frames
        mode = screen_mode(self.monitor)
        # A game switching the monitor to another resolution (stretched 4:3) leaves capture at the old size, which
        # shows only a corner of the screen: start capture again at the new size.
        moved = bool(mode and self.mode and mode != self.mode and time.time() - self.started > 2)
        if self.running and not moved and time.time() - max(self.newest(), self.started) < stale:
            return None
        why = (f"screen mode changed ({self.mode} to {mode})" if moved else
               "ffmpeg stopped" if not self.running else "no new video for a while")
        try:
            tail = FFMPEG_LOG.read_text(encoding="utf-8", errors="replace").strip().splitlines()[-1:]
        except OSError:
            tail = []
        if tail and not moved:
            why += f" ({tail[0][:200]})"
        self._kill()
        # Keep the buffer; only the piece it died in is broken (it won't join with the rest).
        mine = [p for p in self.pieces() if p.name.startswith(f"seg{self.run:04d}_")]
        if mine:
            mine[-1].unlink(missing_ok=True)
        self.run += 1
        self._launch()
        self.restarts += 1
        log.info("recorder restarted: %s", why)
        return why

    def _kill(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()

    def stop(self) -> None:
        self.wanted = False
        self.sound.stop()
        self.mic.stop()
        if not self.running:
            self.proc = None
            for old in RING.glob("seg*.ts"):
                old.unlink(missing_ok=True)
            return
        try:
            self.proc.communicate(b"q", timeout=5)  # lets ffmpeg finish the current piece
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.proc = None
        for old in RING.glob("seg*.ts"):  # nothing stays on disk once you stop streaming
            old.unlink(missing_ok=True)
        log.info("stopped recording")

    def length(self, piece: Path, key: tuple) -> float | None:
        """How long a piece is, in seconds (None: unreadable)."""
        if key in self.lengths:
            return self.lengths[key]
        if self.ffprobe:
            out = run([self.ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(piece)])
            text = out.stdout.decode(errors="replace").strip()
        else:  # ffmpeg -i prints "Duration: 00:00:02.00"
            out = run([self.ffmpeg, "-hide_banner", "-i", str(piece)])
            found = re.search(r"Duration: (\d+):(\d+):([\d.]+)", out.stderr.decode(errors="replace"))
            text = str(int(found[1]) * 3600 + int(found[2]) * 60 + float(found[3])) if found else ""
        try:
            value = float(text)
        except ValueError:
            value = None
        if value is not None and value < 0.1:
            value = None
        self.lengths[key] = value
        return value

    def collect(self, seconds: float, dest: Path) -> list[tuple[Path, tuple, float]]:
        """Copy the pieces of the last `seconds` (and a bit) into dest: (copy, cache key, when it was written)."""
        copies = []
        with self.lock:  # the watchdog can't delete a piece while we copy them
            wanted_from = time.time() - seconds - SEGMENT_S
            for i, piece in enumerate(self.pieces()):
                try:
                    st = piece.stat()
                    if st.st_mtime < wanted_from or not st.st_size:
                        continue
                    copy = dest / f"p{i:03d}.ts"
                    shutil.copyfile(piece, copy)
                except OSError:
                    continue
                copies.append((copy, (piece.name, st.st_mtime, st.st_size), st.st_mtime))
        return copies

    def build(self, copies: list[tuple[Path, tuple, float]], seconds: float, out: Path, sound, kbps_for,
              height: int, best: bool = False, mic=None) -> str:
        """Encode the pieces into one mp4 of the last `seconds`, keeping real time: where the recorder restarted
        (a game took the screen away), the last frame holds for the missing moment, so the sound, and the mic
        Pumpkin lays on top, stay in sync. `sound(begin, end, path)` writes the PC sound for that stretch and
        returns the input offset into it (None: no sound); `mic` does the same for your mic, which goes in as a
        second sound track (only next to PC sound) for Pumpkin to mix. Returns why it failed, or ""."""
        parts = []  # (file, start, length) in wall-clock time
        for copy, key, end in copies:
            if (n := self.length(copy, key)) is not None:
                parts.append((copy, end - n, n))
        if not parts:
            return "nothing recorded yet" if not copies else "every recorded piece was unreadable"
        t0, end = parts[0][1], parts[-1][1] + parts[-1][2]
        begin = max(t0, end - seconds)
        chains, at = [], 0.0
        for i, (_, start, n) in enumerate(parts):
            nxt = parts[i + 1][1] - t0 if i + 1 < len(parts) else at + n
            hold = max(0.0, nxt - (at + n))
            chains.append(f"[{i}:v]setpts=PTS-STARTPTS" + (f",tpad=stop_mode=clone:stop_duration={hold:.3f}"
                                                           if hold > 0.05 else "") + f"[v{i}]")
            at += n + (hold if hold > 0.05 else 0)
        graph = ";".join(chains) + ";" + "".join(f"[v{i}]" for i in range(len(parts))) + \
            f"concat=n={len(parts)}:v=1:a=0,fps=30,trim=start={begin - t0:.3f},setpts=PTS-STARTPTS," \
            f"scale={height * 16 // 9}:{height},setsar=1,format=nv12[v]"
        length = end - begin
        log.info("clip covers %s-%s", time.strftime("%H:%M:%S", time.localtime(begin)),
                 time.strftime("%H:%M:%S", time.localtime(end)))
        wav = out.with_name("sound.wav")
        offset = sound(begin, end, wav)
        # A saved clip keeps its sound lossless: Pumpkin compresses it once, when it mixes the mics in.
        codec = ["-c:a", "flac"] if best else ["-c:a", "aac", "-b:a", "128k"]
        kbps = kbps_for(length, offset is not None)
        # One decoder thread per piece (a piece is tiny, and dozens of full thread pools fight the game).
        inputs = [x for part in parts for x in ("-threads", "1", "-i", str(part[0]))]
        mic_wav = out.with_name("mic.wav")
        mic_at = mic(begin, end, mic_wav) if mic is not None and offset is not None else None
        audio = (["-map", f"{len(parts)}:a", *codec, "-ac:a:0", "2"] if offset is not None
                 else ["-an"])
        if mic_at is not None:
            audio += ["-map", f"{len(parts) + 1}:a", "-ac:a:1", "1", "-metadata:s:a:1", "title=mic"]
        done = run([self.ffmpeg, "-hide_banner", "-loglevel", "error", "-y", *inputs,
                    *(["-ss", f"{offset:.3f}", "-i", str(wav)] if offset is not None else []),
                    *(["-ss", f"{mic_at:.3f}", "-i", str(mic_wav)] if mic_at is not None else []),
                    "-filter_threads", SPARE_THREADS, "-filter_complex", graph, "-map", "[v]",
                    *encoder_args(self.encoder, kbps, best), "-threads", SPARE_THREADS, *audio,
                    "-t", f"{length:.3f}", *([] if best else ["-movflags", "+faststart"]), str(out)], gentle=True)
        if done.returncode or not out.exists():
            why = done.stderr.decode(errors="replace").strip()[-300:]
            log.warning("cutting failed: %s", why)
            return f"cutting failed: {why}"
        return ""

    def cut(self, seconds: int) -> tuple[bytes | None, str]:
        """The last `seconds` now, as an mp4 small enough for Discord ("pumpkin, post that now")."""
        with tempfile.TemporaryDirectory() as d:
            copies = self.collect(seconds, Path(d))
            for key in [k for k in self.lengths if k not in {c[1] for c in copies}]:
                del self.lengths[key]

            def sound(begin, end, wav):
                return 0.0 if self.sound.wav(wav, end, end - begin) else None

            def kbps_for(length, has_sound):
                return max(300, min(5000, int(MAX_BYTES * 8 / max(length, 1) / 1000) - 50 - (128 if has_sound else 0)))

            out = Path(d) / "clip.mp4"
            why = self.build(copies, seconds, out, sound, kbps_for, 720 if seconds <= 30 else 480)
            return (None, why) if why else (out.read_bytes(), "")

    def save(self, seconds: int, sid: int) -> str:
        """Keep the last `seconds` (pieces and PC sound) for after the stream. Returns why not, or ""."""
        dest = SAVED / str(sid)
        dest.mkdir(parents=True, exist_ok=True)
        copies = self.collect(seconds, dest)
        if not copies:
            shutil.rmtree(dest, ignore_errors=True)
            return "nothing recorded yet"
        now, span = time.time(), seconds + 2 * SEGMENT_S
        has_sound = self.sound.wav(dest / "sound.wav", now, span)
        has_mic = self.mic.wav(dest / "mic.wav", now, span)
        meta = {"id": sid, "seconds": seconds, "saved": now,
                "pieces": [[c[0].name, c[2]] for c in copies], "sound": [now - span] if has_sound else None,
                "mic": [now - span] if has_mic else None}
        (dest / "meta.json").write_text(json.dumps(meta), encoding="utf-8")
        log.info("saved %ss for after the stream (%d pieces)", seconds, len(copies))
        return ""


class Games:
    """Is a game running? Discord's list of detectable games (refreshed daily), matched against running programs."""

    def __init__(self):
        self.exes: dict[str, list[str]] = {}  # exe name -> path endings Discord expects ("" = any path)
        self.loaded = -1e9
        self.last_seen = 0.0  # when a game was last running
        self.playing = ""

    def load(self) -> None:
        if time.time() - self.loaded < (86400 if self.exes else 3600):
            return
        self.loaded = time.time()
        data = None
        fresh = GAME_LIST.exists() and time.time() - GAME_LIST.stat().st_mtime < 86400
        if not fresh:
            try:
                import urllib.request
                ask = urllib.request.Request(GAME_LIST_URL, headers={"User-Agent": "Mozilla/5.0 (PumpkinAgent)"})
                with urllib.request.urlopen(ask, timeout=30) as r:
                    data = json.loads(r.read())
                GAME_LIST.write_text(json.dumps(data), encoding="utf-8")
                log.info("game list: %d games from Discord", len(data))
            except Exception as e:
                log.warning("can't fetch Discord's game list (%s)", e)
        if data is None and GAME_LIST.exists():
            data = json.loads(GAME_LIST.read_text(encoding="utf-8"))
        exes: dict[str, list[str]] = {}
        for app in data or []:
            for exe in app.get("executables") or []:
                if exe.get("os") != "win32":
                    continue
                end = exe["name"].lower().replace("\\", "/").lstrip(">")
                base = end.rsplit("/", 1)[-1]
                if base.endswith(".exe") and base not in NOT_GAMES:
                    exes.setdefault(base, []).append(end if "/" in end else "")
        if exes:
            self.exes = exes

    def running(self) -> str:
        """The name of a running game's program, or ""."""
        self.load()
        try:
            out = subprocess.run(["tasklist", "/fo", "csv", "/nh"], capture_output=True, text=True,
                                 creationflags=NO_WINDOW, timeout=20).stdout
        except (OSError, subprocess.SubprocessError):
            return self.playing  # can't tell: assume nothing changed
        for line in out.splitlines():
            found = re.match(r'^"([^"]+)","(\d+)"', line)
            if not found or (exe := found[1].lower()) not in self.exes:
                continue
            ends = self.exes[exe]
            if "" in ends:
                return exe
            path = process_path(int(found[2]))
            if not path or any(path.endswith("/" + e) or path == e for e in ends):
                return exe  # anti-cheat games hide their path: trust the name then
        return ""

    def quiet(self) -> bool:
        """True once no game has been running for GAME_GONE_S."""
        now = time.time()
        playing = self.running()
        if playing != self.playing:
            log.info("game %s", f"running: {playing}" if playing else f"closed: {self.playing}")
            self.playing = playing
        if playing:
            self.last_seen = now
        return not playing and now - self.last_seen >= GAME_GONE_S


def process_path(pid: int) -> str:
    if sys.platform != "win32":
        return ""
    import ctypes
    from ctypes import wintypes
    k32 = ctypes.windll.kernel32
    k32.OpenProcess.restype = wintypes.HANDLE
    handle = k32.OpenProcess(0x1000, False, pid)
    if not handle:
        return ""
    try:
        size = wintypes.DWORD(1024)
        buf = ctypes.create_unicode_buffer(1024)
        if not k32.QueryFullProcessImageNameW(wintypes.HANDLE(handle), 0, buf, ctypes.byref(size)):
            return ""
        return buf.value.lower().replace("\\", "/")
    finally:
        k32.CloseHandle(wintypes.HANDLE(handle))


def render_saved(recorder: Recorder, dest: Path) -> tuple[bytes | None, str]:
    """A saved clip, encoded properly: the recording's full size at its save bitrate, better sound."""
    meta = json.loads((dest / "meta.json").read_text(encoding="utf-8"))
    copies = [(dest / name, (str(dest), name, mtime), mtime) for name, mtime in meta["pieces"]
              if (dest / name).exists()]

    def sound(begin, end, wav):
        if not meta["sound"]:
            return None
        return max(0.0, begin - meta["sound"][0])  # the saved sound starts a bit before the clip

    def mic(begin, end, wav):
        if not meta.get("mic"):
            return None
        return max(0.0, begin - meta["mic"][0])  # build reads it from dest/mic.wav, saved next to the clip

    out = dest / "clip.mkv"  # mkv: lossless sound in it is no trouble
    why = recorder.build(copies, meta["seconds"], out, sound, lambda length, has_sound: recorder.save_kbps,
                         recorder.height, best=True,
                         mic=mic)
    for key in [k for k in recorder.lengths if k[0] == str(dest)]:
        del recorder.lengths[key]
    return (None, why) if why else (out.read_bytes(), "")


async def upload_saved(ws, recorder: Recorder, done: dict, games: "Games") -> None:
    """Once no game is running, encode and send each saved clip at full speed; it's deleted once Pumpkin has it.
    Mid-game it waits (an upload mid-game costs ping); without a game it goes straight away, streaming or not."""
    async def render(dest: Path) -> tuple[bytes | None, str]:
        started = time.monotonic()
        try:
            video, why = await asyncio.to_thread(render_saved, recorder, dest)
        except Exception as e:
            log.exception("saved clip failed")
            video, why = None, f"saved clip failed: {e!r}"[:300]
        log.info("saved clip %s -> %s bytes in %.1fs", dest.name, len(video or b""), time.monotonic() - started)
        return video, why

    while True:
        await asyncio.sleep(5)
        if not SAVED.exists() or not any(SAVED.iterdir()):
            continue
        if not await asyncio.to_thread(games.quiet):
            continue
        todo = []
        for dest in sorted(SAVED.iterdir(), key=lambda d: d.stat().st_mtime):
            if not (dest / "meta.json").exists():
                shutil.rmtree(dest, ignore_errors=True)  # a save that never finished
            elif dest.name not in done:
                todo.append(dest)
        # encode the next clip while this one uploads, so a backlog goes out back to back
        nxt = asyncio.create_task(render(todo[0])) if todo else None
        for i, dest in enumerate(todo):
            video, why = await nxt
            nxt = asyncio.create_task(render(todo[i + 1])) if i + 1 < len(todo) and not games.playing else None
            sid = int(dest.name)
            await ws.send_str(json.dumps({"op": "saved_clip", "id": sid, "ok": bool(video), "why": why}))
            await send_paced(ws, sid, video or b"", rate=0)  # no game running: full speed
            done[dest.name] = time.time()  # until Pumpkin says it has it (or drops it), don't send it again
            if nxt is None:
                break  # done, or a game started again: the rest waits until it closes


def tunnel(server: str, key: str) -> subprocess.Popen:
    return subprocess.Popen(
        ["ssh", "-N", "-i", key, "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=20",
         "-o", "StrictHostKeyChecking=accept-new", "-L", f"{PORT}:127.0.0.1:{PORT}", server],
        creationflags=NO_WINDOW)


async def serve(recorder: Recorder, token: str) -> None:
    async with aiohttp.ClientSession() as http:
        async with http.ws_connect(f"http://127.0.0.1:{PORT}/agent", params={"token": token},
                                   heartbeat=30, max_msg_size=0) as ws:
            log.info("connected to Pumpkin")
            await ws.send_str(json.dumps({"op": "hi", "encoder": recorder.encoder, "version": VERSION,
                                          "autostart": autostart(), "save": True}))

            async def watchdog():
                told = 0.0
                while True:
                    await asyncio.sleep(1)  # capture can die every few seconds in some games: be back fast
                    if (why := recorder.check()) and time.time() - told > 60:
                        told = time.time()  # tell Pumpkin at most once a minute
                        await ws.send_str(f"recorder restarted ({recorder.restarts} so far): {why}")

            sent: dict[str, float] = {}
            guard = asyncio.create_task(watchdog())
            uploads = asyncio.create_task(upload_saved(ws, recorder, sent, GAMES))
            try:
                await handle(ws, recorder, sent)
            finally:
                guard.cancel()
                uploads.cancel()


async def handle(ws, recorder: Recorder, sent: dict | None = None) -> None:
    async for msg in ws:
        if msg.type != aiohttp.WSMsgType.TEXT:
            continue
        order = json.loads(msg.data)
        if order["op"] == "update" and not OWNER:
            if update(order["code"]):
                raise Restart
        elif order["op"] == "autostart":
            set_autostart(bool(order["on"]))
            log.info("starts with Windows: %s", "yes" if order["on"] else "no")
        elif order["op"] == "quit":
            log.info("Pumpkin asked me to stop (/agent stop)")
            raise Quit
        elif order["op"] == "record":
            recorder.start() if order["on"] else recorder.stop()
        elif order["op"] == "save":
            why = "not recording (Pumpkin doesn't think you're live)"
            if recorder.wanted:
                recorder.check()
                SAVED.mkdir(exist_ok=True)
                if shutil.disk_usage(SAVED).free < MIN_FREE:
                    why = f"less than {MIN_FREE // 1024 ** 3} GB free on the disk, so there's no room to keep it"
                else:
                    try:
                        why = await asyncio.to_thread(recorder.save, int(order["seconds"]), int(order["id"]))
                    except Exception as e:
                        log.exception("save failed")
                        why = f"save failed: {e!r}"[:300]
            game = "" if why else await asyncio.to_thread(GAMES.running)
            await ws.send_str(json.dumps({"op": "saved", "id": order["id"], "ok": not why, "why": why,
                                          "game": game.removesuffix(".exe")}))
        elif order["op"] in ("got", "drop"):  # Pumpkin has the saved clip (or doesn't want it): delete it here
            shutil.rmtree(SAVED / str(int(order["id"])), ignore_errors=True)
            if sent is not None:
                sent.pop(str(int(order["id"])), None)
        elif order["op"] == "clip":
            started = time.monotonic()
            recorder.check()
            video, why = None, "not recording (Pumpkin doesn't think you're live)"
            if recorder.wanted:
                try:
                    video, why = await asyncio.to_thread(recorder.cut, int(order["seconds"]))
                except Exception as e:  # a bad clip must never take the connection (and the buffer) down
                    log.exception("clip failed")
                    why = f"clip failed: {e!r}"[:300]
            log.info("clip %ss -> %s bytes in %.1fs", order["seconds"], len(video or b""),
                     time.monotonic() - started)
            if not video:
                await ws.send_str(f"no clip: {why}")
            await send_paced(ws, order["id"], video or b"")


async def send_paced(ws, rid: int, data: bytes, rate: int = UPLOAD_RATE) -> None:
    """The clip in small pieces at `rate` bytes/s, so the upload doesn't lag the game (0: as fast as it goes)."""
    for at in range(0, max(len(data), 1), CHUNK):
        piece = data[at:at + CHUNK]
        last = at + CHUNK >= len(data)
        await ws.send_bytes(struct.pack(">Q", rid if last else rid | MORE) + piece)
        if not last and rate:
            await asyncio.sleep(len(piece) / rate)


def background_command() -> str:
    """How Windows starts the hidden copy: uvw (uv without a window) for a friend's copy, the repo's pythonw for
    the owner's."""
    script = f'"{Path(__file__).resolve()}" --background'
    uv = os.environ.get("UV")
    if uv and (uvw := Path(uv).with_name("uvw.exe")).exists():
        return f'"{uvw}" run --quiet --python 3.12 --gui-script {script}'
    pythonw = Path(sys.executable).with_name("pythonw.exe")
    return f'"{pythonw if pythonw.exists() else sys.executable}" {script}'


def set_autostart(on: bool) -> None:
    if sys.platform != "win32":
        return
    import winreg
    if on:
        NO_AUTOSTART.unlink(missing_ok=True)
    else:
        NO_AUTOSTART.write_text("autostart off (/agent autostart in Discord)\n", encoding="utf-8")
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        if on:
            winreg.SetValueEx(k, RUN_NAME, 0, winreg.REG_SZ, background_command())
        else:
            try:
                winreg.DeleteValue(k, RUN_NAME)
            except FileNotFoundError:
                pass


def autostart() -> bool:
    return not NO_AUTOSTART.exists()


def started_at(pid: int) -> int:
    """When a process started (Windows FILETIME), 0 if it's gone. A process number alone isn't enough: after a
    restart Windows hands the old number to some other program."""
    if sys.platform != "win32":
        return 0
    import ctypes
    from ctypes import wintypes
    k32 = ctypes.windll.kernel32
    k32.OpenProcess.restype = wintypes.HANDLE
    handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return 0
    try:
        times = [wintypes.FILETIME() for _ in range(4)]
        if not k32.GetProcessTimes(wintypes.HANDLE(handle), *[ctypes.byref(t) for t in times]):
            return 0
        return (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
    finally:
        k32.CloseHandle(wintypes.HANDLE(handle))


def take_over() -> None:
    """One copy at a time: stop an older one (with its ffmpeg and tunnel), then claim the pid file. Only when the
    saved number still belongs to that same copy (same start time), never whatever got the number since."""
    try:
        pid, born = (int(x) for x in PID_FILE.read_text().split())
    except (OSError, ValueError):
        pid = born = 0  # none, or an old-style file we can't check: leave it alone
    if pid and pid != os.getpid() and born and started_at(pid) == born:
        gone = subprocess.run(["taskkill", "/T", "/F", "/PID", str(pid)], capture_output=True,
                              creationflags=NO_WINDOW).returncode == 0
        if gone:
            log.info("stopped the copy that was already running")
            time.sleep(1)
    PID_FILE.write_text(f"{os.getpid()} {started_at(os.getpid())}")


def relaunch_hidden() -> None:
    """Start the hidden copy, free of this window (after a double-click, or an update)."""
    PID_FILE.unlink(missing_ok=True)
    flags = getattr(subprocess, "DETACHED_PROCESS", 0) | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
    subprocess.Popen(background_command(), creationflags=flags, close_fds=True)


def update(code: str) -> bool:
    """Pumpkin sent a newer agent: check it compiles, swap it in. The caller restarts."""
    try:
        compile(code, "pumpkin_agent.py", "exec")
    except SyntaxError as e:
        log.warning("Pumpkin sent an update that doesn't compile (%s); keeping this version", e)
        return False
    me = Path(__file__).resolve()
    tmp = me.with_suffix(".new")
    tmp.write_text(code, encoding="utf-8", newline="\n")
    tmp.replace(me)
    log.info("updated to the newest version; restarting")
    return True


class Restart(Exception):
    pass


class Quit(Exception):
    pass


def owner_setup() -> tuple[str, str, str] | None:
    """The owner's repo copy: deploy/server.txt (address and key) and PUMPKIN_AGENT_TOKEN from .env."""
    server_txt = ROOT / "deploy" / "server.txt"
    if not server_txt.exists():
        return None
    try:
        from dotenv import load_dotenv
        load_dotenv(ROOT / ".env")
    except ImportError:
        pass
    token = os.environ.get("PUMPKIN_AGENT_TOKEN", "")
    if not token:
        return None
    server, key = server_txt.read_text(encoding="utf-8").split()[:2]
    return server, key, token


def friend_setup() -> tuple[str, str, str]:
    """Anyone else: a key of our own, paired once through /agent link."""
    if not FRIEND_KEY.exists():
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "pumpkin-agent", "-f", str(FRIEND_KEY)],
                       check=True)
    if FRIEND_CONFIG.exists():
        config = json.loads(FRIEND_CONFIG.read_text(encoding="utf-8"))
        return config["server"], str(FRIEND_KEY), config["token"]
    if BACKGROUND:
        sys.exit("not paired yet: double-click pumpkin-agent.cmd once")
    public = Path(str(FRIEND_KEY) + ".pub").read_text(encoding="utf-8").strip()
    print("\nFirst time here. In Discord, run /agent link and paste this line:\n")
    print("    " + public + "\n")
    while True:
        code = input("Then paste the pairing code Pumpkin sends back: ").strip().strip("`")
        parts = code.split(":", 2)
        if len(parts) == 3 and parts[0] == "pumpkin" and "@" in parts[1] and parts[2]:
            break
        print("That doesn't look right: it starts with pumpkin: (copy the whole code).")
    FRIEND_CONFIG.write_text(json.dumps({"server": parts[1], "token": parts[2]}), encoding="utf-8")
    print("Paired. It records only while you Go Live.\n")
    return parts[1], str(FRIEND_KEY), parts[2]


OWNER = False
GAMES = Games()


def setup_logging() -> None:
    handlers: list[logging.Handler] = [logging.handlers.RotatingFileHandler(LOG_FILE, maxBytes=1_000_000,
                                                                             backupCount=1, encoding="utf-8")]
    if not BACKGROUND and sys.stdout:
        handlers.append(logging.StreamHandler(sys.stdout))
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(message)s", datefmt="%H:%M:%S", handlers=handlers)


async def main() -> None:
    global OWNER
    take_over()
    owner = owner_setup()
    OWNER = owner is not None
    server, key, token = owner or friend_setup()
    if autostart():
        set_autostart(True)  # written again every start, so it follows the folder if you move it
    recorder = Recorder(find_ffmpeg(), int(os.environ.get("PUMPKIN_AGENT_MONITOR", "0")))
    ssh = None
    failures = 0
    try:
        while True:
            if ssh is None or ssh.poll() is not None:
                ssh = tunnel(server, key)
                await asyncio.sleep(3)
            try:
                await serve(recorder, token)
                failures = 0
            except Restart:
                recorder.stop()
                ssh.terminate()
                relaunch_hidden()
                return
            except Quit:
                PID_FILE.unlink(missing_ok=True)
                return
            except aiohttp.WSServerHandshakeError as e:
                if e.status == 403 and FRIEND_CONFIG.exists():
                    FRIEND_CONFIG.unlink()  # unlinked or paired again elsewhere: ask for a new code next start
                    sys.exit("Pumpkin doesn't know this agent anymore. Start it again and pair with /agent link.")
                log.info("not connected (%s); retrying in 10 s", e.status)
            except (aiohttp.ClientError, OSError) as e:
                failures += 1
                if failures == 3:
                    log.info("still can't reach Pumpkin. Is the server up? (/agent status in Discord)")
                log.info("not connected (%s); retrying in 10 s", type(e).__name__)
            except Exception:  # anything else (waking from sleep can throw odd ones): log it, never die of it
                failures += 1
                log.exception("connection broke unexpectedly; retrying in 10 s")
                if ssh is not None:
                    ssh.terminate()  # a fresh tunnel too: after sleep the old one is often dead
                    ssh = None
            recorder.stop()  # lost Pumpkin: don't keep recording blind
            await asyncio.sleep(10)
    finally:
        recorder.stop()
        if ssh:
            ssh.terminate()


if __name__ == "__main__":
    setup_logging()
    if not BACKGROUND and "--here" not in sys.argv and sys.platform == "win32":
        # Double-clicked: pair in this window if needed, then hand over to the hidden copy, so closing it is fine.
        if owner_setup() is None:
            friend_setup()
        take_over()
        relaunch_hidden()
        print("Running in the background now, and it starts by itself when you sign in to Windows.\n"
              "/agent status in Discord shows it, /agent stop stops it. You can close this window.")
        sys.exit(0)
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
    except SystemExit as e:  # hidden, nobody sees the message: keep it in the log
        if e.code not in (None, 0):
            log.error("stopped: %s", e.code)
        raise
    except BaseException:
        # a crash: log it and start over, so the agent is never silently gone until the next sign-in
        log.exception("crashed; starting again in 30 s")
        if BACKGROUND:
            time.sleep(30)
            relaunch_hidden()
