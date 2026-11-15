"""Voice clipping: Pumpkin sits in a call, keeps the last 2 minutes of whoever opted in, and clips on command.

"pumpkin clip that" -> the last 30 s of everyone who opted in, mixed into an mp3 in the call's text chat.
Only people who ran /voice optin are heard; everyone else's audio is dropped the moment it arrives. Nothing is
saved unless someone clips. An offline Vosk model listens for "pumpkin" (free, no limits); only then does the
clip go to cloud speech to text (stt.py) for the exact command, with Vosk's own guess as the fallback.
"""

import asyncio
import io
import json
import logging
import re
import shutil
import tempfile
import threading
import time
import wave
from collections import deque
from datetime import datetime
from pathlib import Path
from typing import Callable

import discord
import numpy as np
from discord import app_commands

from .clips import ClipStore
from .stt import Transcriber

try:
    from discord.ext import voice_recv
except ImportError:  # voice extras not installed: /voice says so
    voice_recv = None

log = logging.getLogger(__name__)

RATE = 48000  # what Discord decodes to
BUFFER_S = 120  # how far back a clip can reach
JOIN_GAP_S = 0.1  # frames closer than this are one stretch of talking
LOUD_RMS = 500  # rough speech threshold for 16-bit samples
END_SILENCE_S = 0.5  # this long quiet (or no packets) ends an utterance
MIN_SPEECH_S = 0.4
MAX_UTTER_S = 8.0  # long monologues get checked in 8 s pieces
DEFAULT_CLIP_S = 30
WHOLE_FIGHT_S = 120
UNDO_WINDOW_S = 15 * 60
STT_GAP_S = 1.5  # at most one cloud transcription per call this often
MODEL_DIR = Path("models/vosk-model-small-en-us-0.15")

NOTICE = ("🎙️ pumpkin is in the call and listening for its name. **only people who run `/voice optin` are heard**; "
          "everyone else is dropped on arrival. i keep the last 2 minutes in memory and save nothing unless someone "
          "says *\"pumpkin, clip that\"* (30 s), *\"pumpkin, clip the whole fight\"* (2 min), *\"pumpkin, clip the last "
          "10 seconds\"* or *\"pumpkin, nobody saw that\"* (deletes the last clip). `/voice leave` kicks me out.")

WAKE = re.compile(r"\b(p[ua]mp?[kc]+[iey]n|bumpkin)", re.I)
UNDO = re.compile(r"nobody saw|no one saw|delete|undo|remove|scrap|verwijder|niemand", re.I)
CLIP = re.compile(r"\b(clip|clips|clipped|clipping|knip|save|record)", re.I)
WHOLE = re.compile(r"\b(whole|entire|everything|all of it|hele|alles)\b", re.I)
NUMBERS = {"a": 1, "an": 1, "one": 1, "een": 1, "two": 2, "twee": 2, "three": 3, "drie": 3, "four": 4, "five": 5,
           "vijf": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10, "tien": 10, "fifteen": 15,
           "twenty": 20, "twintig": 20, "thirty": 30, "dertig": 30, "forty": 40, "fourty": 40, "fifty": 50,
           "sixty": 60, "zestig": 60, "ninety": 90}
AMOUNT = re.compile(r"(\d+|" + "|".join(sorted(NUMBERS, key=len, reverse=True)) + r")\s*"
                    r"(seconds?|secs?|s\b|seconden|minutes?|mins?|minuten|minuut)", re.I)
# What the offline model may hear, so it can stand in when every cloud model is resting.
SPOKEN = ["pumpkin clip that", "pumpkin clip it", "pumpkin clip the whole fight", "pumpkin clip the whole thing",
          "pumpkin nobody saw that", "pumpkin delete that"] + [
          f"pumpkin clip the last {n} {unit}" for n in ("ten", "twenty", "thirty", "sixty") for unit in ("seconds",)] + [
          "pumpkin clip the last minute", "pumpkin clip the last two minutes", "pumpkin", "[unk]"]


def parse_command(text: str) -> tuple[str, int] | None:
    """("clip", seconds) or ("undo", 0) when the text calls Pumpkin by name and asks for one; else None."""
    if not text or not WAKE.search(text):
        return None
    if UNDO.search(text):
        return ("undo", 0)
    if not CLIP.search(text):
        return None
    return parse_clip(text)


def parse_clip(text: str) -> tuple[str, int]:
    if WHOLE.search(text):
        return ("clip", WHOLE_FIGHT_S)
    m = AMOUNT.search(text)
    if m:
        word, unit = m.group(1).lower(), m.group(2).lower()
        n = int(word) if word.isdigit() else NUMBERS[word]
        seconds = n * 60 if unit.startswith("min") else n
        return ("clip", max(5, min(BUFFER_S, seconds)))
    if re.search(r"\b(last|laatste) (minute|minuut)\b", text, re.I):
        return ("clip", 60)
    return ("clip", DEFAULT_CLIP_S)


def offline_sure(text: str) -> bool:
    """Vosk forces everything onto its phrase list ("the pumpkin pie was great" -> "thing clip pumpkin [unk]"),
    so on its own only a clean phrase that starts with the name counts."""
    return text.startswith("pumpkin ") and "[unk]" not in text


def rms(samples: np.ndarray) -> float:
    return float(np.sqrt(np.mean(samples.astype(np.float32) ** 2))) if len(samples) else 0.0


def to_mono(pcm: bytes) -> np.ndarray:
    """Discord's 48 kHz 16-bit stereo -> mono int16."""
    stereo = np.frombuffer(pcm, dtype=np.int16)
    if len(stereo) % 2:
        stereo = stereo[:-1]
    return stereo.reshape(-1, 2).mean(axis=1).astype(np.int16)


def to_16k(samples: np.ndarray) -> np.ndarray:
    """48 kHz -> 16 kHz by averaging each 3 samples (a crude low-pass, plenty for speech)."""
    n = len(samples) // 3 * 3
    return samples[:n].astype(np.int32).reshape(-1, 3).mean(axis=1).astype(np.int16)


def wav_bytes(samples: np.ndarray, rate: int) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(samples.astype(np.int16).tobytes())
    return out.getvalue()


class Track:
    """One person's recent audio as (start time, samples) stretches, oldest dropped past BUFFER_S."""

    def __init__(self, keep_s: float = BUFFER_S):
        self.keep_s = keep_s
        self.frames: deque[tuple[float, np.ndarray]] = deque()
        self.end = -1e9

    def add(self, now: float, samples: np.ndarray) -> None:
        length = len(samples) / RATE
        # Packets come in bursts and with jitter; while they keep coming, lay them end to end.
        start = self.end if 0 <= now - self.end < JOIN_GAP_S or (self.end > now and self.end - now < 0.5) \
            else now - length
        self.frames.append((start, samples))
        self.end = start + length
        while self.frames and self.frames[0][0] < now - self.keep_s:
            self.frames.popleft()


def mix(tracks: list[Track], start: float, end: float) -> np.ndarray:
    """Everyone's audio between start and end, laid on one timeline and summed (scaled down if it clips)."""
    n = max(0, int((end - start) * RATE))
    out = np.zeros(n, dtype=np.float32)
    for track in tracks:
        for at, samples in track.frames:
            i = int(round((at - start) * RATE))
            a, b = max(i, 0), min(i + len(samples), n)
            if a < b:
                out[a:b] += samples[a - i:b - i]
    peak = float(np.abs(out).max()) if n else 0.0
    if peak > 32767:
        out *= 32767 / peak
    return out.astype(np.int16)


class Utterances:
    """Cuts one person's audio into utterances: loud stretches ended by half a second of quiet or no packets."""

    def __init__(self):
        self.parts: list[np.ndarray] = []
        self.talking = False
        self.quiet = 0.0
        self.last = 0.0

    def feed(self, now: float, samples: np.ndarray) -> np.ndarray | None:
        self.last = now
        if rms(samples) > LOUD_RMS:
            self.talking, self.quiet = True, 0.0
        elif not self.talking:
            return None
        else:
            self.quiet += len(samples) / RATE
        self.parts.append(samples)
        if self.quiet >= END_SILENCE_S or self.length() >= MAX_UTTER_S:
            return self.finish()
        return None

    def idle(self, now: float) -> np.ndarray | None:
        """Discord stops sending packets when someone stops talking, so silence often never arrives."""
        if self.talking and now - self.last >= END_SILENCE_S:
            return self.finish()
        return None

    def length(self) -> float:
        return sum(len(p) for p in self.parts) / RATE

    def finish(self) -> np.ndarray | None:
        audio = np.concatenate(self.parts) if self.parts else np.zeros(0, np.int16)
        spoken = self.length() - self.quiet
        self.parts, self.talking, self.quiet = [], False, 0.0
        return audio if spoken >= MIN_SPEECH_S else None


class Spotter:
    """Offline Vosk listening for "pumpkin" (and the commands, as a fallback). Free and unlimited."""

    def __init__(self, model_dir: Path):
        from vosk import KaldiRecognizer, Model, SetLogLevel
        SetLogLevel(-1)
        self._recognizer = KaldiRecognizer
        self.model = Model(str(model_dir))
        self.grammar = json.dumps(SPOKEN)

    def hear(self, pcm16k: bytes) -> str:
        rec = self._recognizer(self.model, 16000, self.grammar)
        rec.AcceptWaveform(pcm16k)
        return json.loads(rec.FinalResult()).get("text", "")


def load_spotter(model_dir: Path = MODEL_DIR) -> Spotter | None:
    if not model_dir.exists():
        log.warning("No Vosk model at %s; voice commands are off (only /voice clip works). Run deploy/setup.sh.",
                    model_dir)
        return None
    try:
        return Spotter(model_dir)
    except Exception as e:
        log.warning("Couldn't load the Vosk model: %s", e)
        return None


class Session:
    """One call Pumpkin sits in. hear() runs on the voice reader thread; everything else on the event loop."""

    def __init__(self, voice_client, channel, consent: Callable[[int], bool], loop: asyncio.AbstractEventLoop,
                 on_utterance: Callable):
        self.vc = voice_client
        self.channel = channel
        self.consent = consent
        self.loop = loop
        self.on_utterance = on_utterance
        self.tracks: dict[int, Track] = {}
        self.cutters: dict[int, Utterances] = {}
        self.lock = threading.Lock()
        self.frames = self.dropped = self.heard_name = self.commands = 0
        self.last_stt = 0.0
        self.last_clip: tuple | None = None  # (message, when[, saved clip id])
        self.started = time.monotonic()
        self.ticker: asyncio.Task | None = None

    def hear(self, user_id: int, pcm: bytes) -> None:
        if not self.consent(user_id):
            self.dropped += 1
            return
        samples = to_mono(pcm)
        now = time.monotonic()
        with self.lock:
            self.frames += 1
            self.tracks.setdefault(user_id, Track()).add(now, samples)
            done = self.cutters.setdefault(user_id, Utterances()).feed(now, samples)
        if done is not None:
            asyncio.run_coroutine_threadsafe(self.on_utterance(self, user_id, done), self.loop)

    async def tick(self) -> None:
        while True:
            await asyncio.sleep(0.25)
            now = time.monotonic()
            with self.lock:
                done = [(uid, a) for uid, c in self.cutters.items() if (a := c.idle(now)) is not None]
            for uid, audio in done:
                await self.on_utterance(self, uid, audio)

    def forget(self, user_id: int) -> None:
        with self.lock:
            self.tracks.pop(user_id, None)
            self.cutters.pop(user_id, None)

    def clip(self, seconds: float, only: int | None = None) -> tuple[np.ndarray, list[int]]:
        now = time.monotonic()
        with self.lock:
            people = [uid for uid, t in self.tracks.items() if (only is None or uid == only) and t.end > now - seconds]
            audio = mix([self.tracks[uid] for uid in people], now - seconds, now)
        return audio, people


async def to_mp3(samples: np.ndarray) -> bytes:
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        raise RuntimeError("ffmpeg isn't installed")
    proc = await asyncio.create_subprocess_exec(
        ffmpeg, "-loglevel", "error", "-f", "s16le", "-ar", str(RATE), "-ac", "1", "-i", "pipe:0",
        "-b:a", "96k", "-f", "mp3", "pipe:1",
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    out, err = await proc.communicate(samples.astype(np.int16).tobytes())
    if proc.returncode:
        raise RuntimeError(err.decode(errors="replace")[:200])
    return out


async def probe(path: Path) -> tuple[bool, float]:
    """(has a sound track, length in seconds) of a video; (False, 0) when ffprobe can't tell."""
    found = shutil.which("ffprobe")
    if not found:
        return False, 0.0
    proc = await asyncio.create_subprocess_exec(
        found, "-v", "error", "-show_entries", "stream=codec_type:format=duration", "-of", "csv=p=0", str(path),
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    out, _ = await proc.communicate()
    lines = out.decode(errors="replace").split()
    length = 0.0
    for line in lines:
        try:
            length = float(line.strip(","))
        except ValueError:
            pass
    return any(line.startswith("audio") for line in lines), length


FRAME = RATE // 100  # 10 ms


def gate_mic(mic: np.ndarray, own: np.ndarray | None) -> np.ndarray:
    """The streamer's mic as their PC recorded it (in step with the screen), kept only where Discord heard them:
    `own` is what Pumpkin received from them, a bit late, and silent while they're muted or not pressing
    push-to-talk. The delay is found by lining the two up; anything said off-mic stays out of the clip."""
    n = len(mic)
    if own is None or n < FRAME:
        return np.zeros(n, np.int16)
    own = own[-n:] if len(own) >= n else np.concatenate([np.zeros(n - len(own), own.dtype), own])
    frames = n // FRAME
    level = lambda x: np.abs(x[:frames * FRAME].astype(np.float32)).reshape(frames, FRAME).mean(axis=1)
    m, o = level(mic), level(own)
    talking = o > 30
    if not talking.any():
        return np.zeros(n, np.int16)
    # Discord's copy runs `lag` frames behind the PC's: own[t] ~ mic[t - lag]
    best, lag, mz, oz = 0.0, 0, m - m.mean(), o - o.mean()
    for k in range(0, min(150, frames // 2)):  # up to 1.5 s late
        a, b = mz[:frames - k], oz[k:]
        score = float(a @ b) / (float(np.linalg.norm(a) * np.linalg.norm(b)) or 1.0)
        if score > best:
            best, lag = score, k
    pad = 25 if best > 0.3 else 60  # 250 ms either side; wider when the two didn't line up well
    mask = np.zeros(frames, np.float32)
    mask[:frames - lag] = talking[lag:]
    mask = np.convolve(mask, np.ones(2 * pad + 1, np.float32), "same") > 0
    smooth = np.repeat(mask.astype(np.float32), FRAME)
    smooth = np.concatenate([smooth, np.zeros(n - len(smooth), np.float32)])
    smooth = np.convolve(smooth, np.ones(FRAME, np.float32) / FRAME, "same")  # 10 ms fades, no clicks
    return (mic.astype(np.float32) * smooth).astype(np.int16)


async def mic_track(src: Path) -> np.ndarray | None:
    """The agent's second sound track (the streamer's own mic) as 48 kHz mono, or None when there isn't one."""
    ffmpeg, ffprobe = shutil.which("ffmpeg"), shutil.which("ffprobe")
    if not ffmpeg or not ffprobe:
        return None
    proc = await asyncio.create_subprocess_exec(
        ffprobe, "-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", str(src),
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    out, _ = await proc.communicate()
    if len(out.split()) < 2:
        return None
    proc = await asyncio.create_subprocess_exec(
        ffmpeg, "-loglevel", "error", "-i", str(src), "-map", "0:a:1", "-f", "s16le", "-ar", str(RATE), "-ac", "1",
        "pipe:1", stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    raw, _ = await proc.communicate()
    return np.frombuffer(raw, np.int16).copy() if proc.returncode == 0 and raw else None


async def to_mp4(video: bytes | Path, audio: np.ndarray, own: np.ndarray | None = None,
                 best: bool = False) -> bytes:
    """A screen clip with sound. When the agent sent the streamer's PC sound (the game, and the call as they heard
    it), only their own mic goes on top, so nobody is heard twice; otherwise the whole call audio. Their mic is the
    agent's own recording when it sent one (in sync, see `gate_mic`), else Discord's copy (`own`)."""
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        raise RuntimeError("ffmpeg isn't installed")
    with tempfile.TemporaryDirectory() as d:
        src, out = Path(d) / "screen.mp4", Path(d) / "clip.mp4"
        if isinstance(video, Path):
            src = video
        else:
            src.write_bytes(video)
        has_sound, length = await probe(src)
        if length > 0:  # the video ends now; a shorter one (recording only just started) gets the end of the sound
            keep = int(length * RATE)
            audio = audio[-keep:]
            own = own[-keep:] if own is not None else None
        if has_sound and (mic := await mic_track(src)) is not None:
            own = gate_mic(mic, own)
        if own is not None and has_sound:
            # Both brought to the same loudness first: PC sound comes in at whatever the Windows volume is.
            level = "loudnorm=I=-18:TP=-2:LRA=11,aresample=48000"
            under, sound = own, ["-filter_complex", f"[0:a:0]{level}[pc];[1:a]{level}[mic];"
                                 "[pc][mic]amix=inputs=2:duration=first:normalize=0,alimiter=limit=0.9[a]",
                                 "-map", "0:v", "-map", "[a]"]
        else:
            under, sound = audio, ["-map", "0:v", "-map", "1:a", "-shortest"]
        proc = await asyncio.create_subprocess_exec(
            ffmpeg, "-loglevel", "error", "-i", str(src), "-f", "s16le", "-ar", str(RATE), "-ac", "1", "-i", "pipe:0",
            *sound, "-c:v", "copy", "-c:a", "aac", "-b:a", "192k" if best else "128k", "-movflags", "+faststart",
            str(out),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        _, err = await proc.communicate(under.astype(np.int16).tobytes())
        if proc.returncode:
            raise RuntimeError(err.decode(errors="replace")[:200])
        return out.read_bytes()


class Voice:
    def __init__(self, consent: Callable[[int], bool], clips: ClipStore, stt: Transcriber,
                 spotter: Spotter | None = None):
        # Screen video for a clip, from the owner's PC agent: async (session, seconds) -> mp4 bytes or None
        # Save a streamer's moment for after the stream: async (session, seconds, asker) -> (id, streamer, note)
        self.screen_save: Callable | None = None
        self.later = None  # later.Later: clips waiting for the end of the stream
        self.on_join: Callable | None = None  # async, after Pumpkin joins a call
        self.clips_channel: Callable = lambda guild: None  # where clips go: the server's clips channel
        self.consent = consent
        self.clips = clips
        self.stt = stt
        self.spotter = spotter
        self.sessions: dict[int, Session] = {}  # guild -> call

    async def join(self, channel: discord.VoiceChannel) -> Session:
        if voice_recv is None:
            raise RuntimeError("voice extras aren't installed")
        if not discord.opus.is_loaded():
            for name in ("libopus.so.0", "libopus.so", "opus"):
                try:
                    discord.opus.load_opus(name)
                    break
                except OSError:
                    continue
        old = self.sessions.get(channel.guild.id)
        if old:
            await self.leave(channel.guild.id)
        patch_decoder()
        vc = await channel.connect(cls=voice_recv.VoiceRecvClient, self_deaf=False)
        session = Session(vc, channel, self.consent, asyncio.get_running_loop(), self.utterance)
        vc.listen(Listener(session))
        session.ticker = asyncio.create_task(session.tick())
        self.sessions[channel.guild.id] = session
        if self.on_join:
            await self.on_join()
        return session

    async def leave(self, guild_id: int) -> bool:
        session = self.sessions.pop(guild_id, None)
        if not session:
            return False
        if session.ticker:
            session.ticker.cancel()
        try:
            session.vc.stop_listening()
        except Exception:
            pass
        await session.vc.disconnect(force=True)
        return True

    async def utterance(self, session: Session, user_id: int, audio: np.ndarray) -> None:
        try:
            await self._utterance(session, user_id, audio)
        except Exception:
            log.exception("Voice command failed")

    async def _utterance(self, session: Session, user_id: int, audio: np.ndarray) -> None:
        if self.spotter is None:
            return
        pcm16 = to_16k(audio)
        offline = await asyncio.to_thread(self.spotter.hear, pcm16.tobytes())
        if "pumpkin" not in offline:
            return
        session.heard_name += 1
        now = time.monotonic()
        text = None
        if now - session.last_stt >= STT_GAP_S and self.stt.providers:
            session.last_stt = now
            text = await self.stt.transcribe(wav_bytes(pcm16, 16000))
        log.debug("Voice: heard %d chars", len(text or ""))  # never the words: logs are not private
        command = parse_command(text) if text else None
        if command is None and not text and offline_sure(offline):  # every cloud model resting or failed
            command = parse_command(offline)
        if command is None:
            return
        session.commands += 1
        member = session.channel.guild.get_member(user_id)
        who = member.display_name if member else "someone"
        if command[0] == "undo":
            await self.undo(session, who)
        else:
            if problem := await self.post_clip(session, command[1], who, asker=user_id):
                await session.channel.send(f"🎬 {problem}", allowed_mentions=discord.AllowedMentions.none())

    async def post_clip(self, session: Session, seconds: int, who: str, only: int | None = None,
                        interaction: discord.Interaction | None = None, asker: int = 0) -> str:
        """A clip is what's on screen: the streamer's agent keeps the moment and uploads it once no game is running
        (straight away if there's none), and the link goes in the clips channel. Whoever asked hears privately
        where it is (a hidden reply to /voice clip, or a button on the "clipped" message). When there's no video to be had it says why; there are no sound-only clips. Returns a problem
        to show, or ""."""
        if only is not None:  # /voice test: hear yourself, privately
            return await self._post_clip(session, seconds, who, only, interaction, asker)
        if not self.screen_save or self.later is None:
            return "can't clip: clip storage isn't set up yet (the owner needs the R2 keys in .env)."
        audio, people = session.clip(seconds)
        sid, streamer, why = await self.screen_save(session, seconds, asker)
        if sid is None:
            return f"can't clip: {why}"
        game = why
        guild = session.channel.guild
        names = ", ".join(m.display_name for uid in people if (m := guild.get_member(uid)))
        star = guild.get_member(streamer)
        whose = star.display_name if star else "the streamer"
        target = self.clips_channel(guild) or session.channel
        where = "here" if target == session.channel else target.mention
        if game:
            hidden = (f"🎮 {whose} was in a game ({game}), so the clip waits on {whose}'s PC and uploads once the "
                      f"game's closed. the link goes in {where} then.")
        else:
            hidden = f"📤 no game running on {whose}'s PC, so it's uploading now. the link goes in {where} in a minute."
        # Discord only lets a bot answer privately to a click or a slash command: a spoken clip gets a button
        message = await session.channel.send(f"🎬 clipped: last {seconds}s of {whose}'s screen.",
                                             view=None if interaction else ClipInfo(self),
                                             allowed_mentions=discord.AllowedMentions.none())
        own = session.clip(seconds, streamer)[0]
        self.later.add(sid, {"guild": guild.id, "call_channel": session.channel.id, "note": message.id,
                             "who": who, "asker": asker, "seconds": seconds, "names": names, "streamer": streamer,
                             "whose": whose, "where": session.vc.channel.name if session.vc else "",
                             "hidden": hidden}, audio, own)
        session.last_clip = (message, time.monotonic(), sid)
        if interaction is not None:
            try:
                await interaction.followup.send(hidden, ephemeral=True)
            except discord.HTTPException:
                pass
        return ""

    async def _post_clip(self, session: Session, seconds: int, who: str, only: int | None,
                         interaction: discord.Interaction | None, asker: int) -> str:
        """/voice test: your own mic for the last few seconds, only for you."""
        audio, people = session.clip(seconds, only)
        if not people or rms(audio) < 30:
            return "nothing to clip. dead silence, or nobody here ran `/voice optin`."
        mp3 = await to_mp3(audio)
        if interaction is None:
            return "that test only works as `/voice test`."
        await interaction.followup.send(f"🎧 your last {seconds}s, just for you.",
                                        file=discord.File(io.BytesIO(mp3), f"pumpkin-test-{datetime.now():%H%M%S}.mp3"),
                                        ephemeral=True)
        return ""

    async def undo(self, session: Session, who: str) -> str:
        if not session.last_clip or time.monotonic() - session.last_clip[1] > UNDO_WINDOW_S:
            return "no fresh clip to bury."
        message, _, *saved = session.last_clip
        session.last_clip = None
        if saved and self.later is not None:  # still waiting for the stream to end: it never gets posted
            self.later.done(saved[0])
        self.clips.remove(message.id)
        try:
            await message.delete()
        except discord.HTTPException:
            pass
        await session.channel.send(f"🗑️ {who} says nobody saw that. nobody saw that.")
        return ""

    async def alone_check(self, guild_id: int) -> None:
        session = self.sessions.get(guild_id)
        if session and not any(not m.bot for m in session.vc.channel.members):
            await session.channel.send("everyone left. i'm not sitting here alone like a weirdo. 👋")
            await self.leave(guild_id)


class ClipInfo(discord.ui.View):
    """The button on a spoken clip's "clipped" message: tells only whoever asked where their clip is."""

    def __init__(self, voice: "Voice"):
        super().__init__(timeout=None)
        self.voice = voice

    @discord.ui.button(label="where's my clip?", emoji="🔒", style=discord.ButtonStyle.secondary,
                       custom_id="pumpkin:clipinfo")
    async def info(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        later = self.voice.later
        entry = next((e for e in later.index.values() if e.get("note") == interaction.message.id), None) \
            if later is not None else None
        if entry is None:
            text = "that one's done: the link's in the clips channel. (or somebody buried it.)"
        elif interaction.user.id != entry.get("asker"):
            text = f"that's {entry['who']}'s clip. the link shows up in the clips channel once it's uploaded."
        else:
            text = entry.get("hidden", "it's on its way.")
        await interaction.response.send_message(text, ephemeral=True)


DAVE_STATS: dict[str, int] = {}  # what happened to incoming frames, for /voice status and the logs
_logged: set[str] = set()


def _count(what: str, detail: str = "") -> None:
    DAVE_STATS[what] = DAVE_STATS.get(what, 0) + 1
    if what not in _logged:  # the first of each kind goes in the log
        _logged.add(what)
        log.warning("Voice frames: first %s %s", what, detail)


def patch_decoder() -> None:
    """Make the voice-recv fork sturdier against DAVE (Discord's end-to-end voice encryption):
    - a frame from someone Discord hasn't announced yet is tried against everyone's key in the call (only the right
      key decrypts, so that also tells us who it is), instead of going to opus still encrypted;
    - a frame that still won't decode becomes silence instead of killing the whole receive thread."""
    from discord.ext.voice_recv import opus as recv_opus
    davey = recv_opus.davey
    decoder = recv_opus.PacketDecoder
    if getattr(decoder, "_pumpkin", False):
        return

    def dave_decrypt(self, packet) -> None:
        if davey is None or not packet or not packet.decrypted_data:
            return
        state = getattr(self.sink.voice_client, "_connection", None)
        session = getattr(state, "dave_session", None)
        if session is None or getattr(state, "dave_protocol_version", 0) == 0:
            _count("plain (no DAVE)")
            return
        if not session.ready:
            _count("DAVE not ready yet")
            return
        frame = bytes(packet.decrypted_data)
        if self._cached_id is None:
            self._cached_id = self.sink.voice_client._get_id_from_ssrc(self.ssrc)
        candidates = [self._cached_id] if self._cached_id else [
            int(u) for u in session.get_user_ids() if int(u) != session.user_id]
        error = None
        for uid in candidates:
            try:
                packet.decrypted_data = session.decrypt(int(uid), davey.MediaType.audio, frame)
            except Exception as e:
                error = e
                continue
            if self._cached_id is None:
                self._cached_id = int(uid)
                self.sink.voice_client._add_ssrc(int(uid), self.ssrc)
                _count("mapped by key")
            _count("decrypted")
            return
        _count("decrypt failed", f"(ssrc {self.ssrc}, tried {len(candidates)}): {error!r}")

    original = decoder._decode_packet

    def decode_packet(self, packet):
        try:
            return original(self, packet)
        except discord.opus.OpusError as e:
            _count("undecodable", repr(e))
            return packet, b"\x00" * 3840  # 20 ms of stereo silence

    decoder._dave_decrypt = dave_decrypt
    decoder._decode_packet = decode_packet
    decoder._pumpkin = True


if voice_recv is not None:
    class Listener(voice_recv.AudioSink):
        def __init__(self, session: Session):
            super().__init__()
            self.session = session

        def wants_opus(self) -> bool:
            return False

        def write(self, user, data) -> None:
            if user is None or not data.pcm:
                return
            self.session.hear(user.id, data.pcm)

        def cleanup(self) -> None:
            pass


def add_voice_commands(tree: app_commands.CommandTree, voice: Voice, personal) -> None:
    group = app_commands.Group(name="voice", description="Pumpkin in your voice call: clips on command",
                               guild_only=True)

    @group.command(name="join", description="Pumpkin joins your call and listens for 'pumpkin, clip that'")
    async def join(interaction: discord.Interaction):
        where = getattr(interaction.user, "voice", None)
        if not where or not where.channel:
            await interaction.response.send_message("get in a voice channel first.", ephemeral=True)
            return
        await interaction.response.defer(ephemeral=True)
        try:
            session = await voice.join(where.channel)
        except Exception as e:
            log.exception("Voice join failed")
            await interaction.followup.send(f"couldn't get in: {e}", ephemeral=True)
            return
        await session.channel.send(NOTICE, allowed_mentions=discord.AllowedMentions.none())
        extra = "" if voice.spotter else " (my offline ears aren't installed, so only `/voice clip` works)"
        await interaction.followup.send(f"in 🔊 {where.channel.name}.{extra}", ephemeral=True)

    @group.command(name="leave", description="Pumpkin leaves the call and forgets everything it heard")
    async def leave(interaction: discord.Interaction):
        gone = await voice.leave(interaction.guild_id)
        await interaction.response.send_message("out. everything i heard is gone." if gone else "i'm not in a call.",
                                                ephemeral=not gone)

    @group.command(name="optin", description="Let Pumpkin hear you in calls, so you end up in clips")
    async def optin(interaction: discord.Interaction):
        personal.get(interaction.user.id)["voice_optin"] = True
        personal.save(interaction.user.id)
        await interaction.response.send_message(
            "🎙️ you're in. when i'm in your call i keep your last 2 minutes in memory and you can end up in clips. "
            "`/voice optout` any time.", ephemeral=True)

    @group.command(name="optout", description="Pumpkin stops hearing you and drops what it had")
    async def optout(interaction: discord.Interaction):
        personal.get(interaction.user.id).pop("voice_optin", None)
        personal.save(interaction.user.id)
        for session in voice.sessions.values():
            session.forget(interaction.user.id)
        await interaction.response.send_message("🔇 done. i can't hear you anymore and what i had is gone.",
                                                ephemeral=True)

    @group.command(name="clip", description="Clip the last N seconds of the call")
    @app_commands.describe(seconds="How far back (5 to 120, default 30)")
    async def clip(interaction: discord.Interaction, seconds: app_commands.Range[int, 5, BUFFER_S] = DEFAULT_CLIP_S):
        session = voice.sessions.get(interaction.guild_id)
        if not session:
            await interaction.response.send_message("i'm not in a call. `/voice join` first.", ephemeral=True)
            return
        await interaction.response.defer(ephemeral=True)
        problem = await voice.post_clip(session, seconds, interaction.user.display_name, asker=interaction.user.id,
                                        interaction=interaction)
        if problem:
            await interaction.followup.send(problem, ephemeral=True)

    @group.command(name="test", description="Hear what Pumpkin hears from you: your last 10 seconds, only for you")
    async def test(interaction: discord.Interaction):
        session = voice.sessions.get(interaction.guild_id)
        if not session:
            await interaction.response.send_message("i'm not in a call. `/voice join` first.", ephemeral=True)
            return
        if not voice.consent(interaction.user.id):
            await interaction.response.send_message("run `/voice optin` first, i can't hear you.", ephemeral=True)
            return
        await interaction.response.defer(ephemeral=True)
        problem = await voice.post_clip(session, 10, interaction.user.display_name, only=interaction.user.id,
                                        interaction=interaction)
        if problem:
            await interaction.followup.send(problem, ephemeral=True)

    @group.command(name="status", description="What Pumpkin hears and which speech models it has")
    async def status(interaction: discord.Interaction):
        session = voice.sessions.get(interaction.guild_id)
        lines = [f"offline ears (Vosk): {'on' if voice.spotter else 'not installed'}",
                 "speech models: " + (", ".join(
                     f"{p.name}{' (resting)' if p.resting_until > time.monotonic() else ''} ×{p.used}"
                     for p in voice.stt.providers) or "none")]
        if session:
            guild = interaction.guild
            names = ", ".join(m.display_name for uid in session.tracks if (m := guild.get_member(uid))) or "nobody yet"
            lines += [f"in 🔊 {session.vc.channel.name} for {int(time.monotonic() - session.started) // 60} min",
                      f"hearing: {names}", f"frames: {session.frames} kept, {session.dropped} dropped (not opted in)",
                      f"heard my name {session.heard_name}×, did {session.commands} command(s)"]
            if DAVE_STATS:
                lines.append("frames: " + ", ".join(f"{k} {v}" for k, v in DAVE_STATS.items()))
        else:
            lines.append("not in a call.")
        await interaction.response.send_message("\n".join(lines), ephemeral=True)

    tree.add_command(group)
