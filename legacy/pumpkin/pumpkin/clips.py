"""The clips archive: videos posted in a clips channel get saved on the server, titled and listed.

    data/clips/index.json                    channels that archive, and every clip's details
    data/clips/<guild id>/<date>-<msg id>-<file>   the files

Turned on per channel by someone who can manage it (/clips here), so Pumpkin never creates anything itself.
A ⭐ reaction makes a clip a keeper: kept when clips older than clip_days get cleaned up.
"""

import json
import re
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta
from pathlib import Path

VIDEO = re.compile(r"\.(mp4|mov|webm|mkv|m4v|gif)$", re.I)
MAX_BYTES = 500 * 1024 * 1024  # Discord's largest upload (Nitro)

TITLE_PROMPT = """You name gaming and friend-group clips for a Discord server's clip archive, in Pumpkin's voice:
dry, a bit chaotic, never cringe. Given who posted it, the file name and what they wrote, answer with only a
short title, 2 to 8 words, no quotes, no emoji spam. If there's nothing to go on, make a vague but funny one."""


@dataclass
class Clip:
    id: int  # the Discord message id
    guild: int
    channel: int
    author: int
    author_name: str
    file: str  # path under data/clips
    title: str
    size: int
    posted: str  # ISO time
    keeper: bool = False
    url: str = ""  # jump link to the message
    remote: str = ""  # key on R2 for full-quality clips that live there instead of under data/clips
    streamer: int = 0  # whose screen it is (screen clips)
    streamer_name: str = ""
    names: str = ""  # who was on tape


@dataclass
class Index:
    channels: list[int] = field(default_factory=list)
    clips: list[Clip] = field(default_factory=list)
    trash: list[str] = field(default_factory=list)  # R2 keys still to delete (done by the bot's clean loop)


def is_clip(filename: str, content_type: str | None) -> bool:
    return bool((content_type or "").startswith("video/") or VIDEO.search(filename))


def safe_name(name: str) -> str:
    return re.sub(r"[^\w.-]+", "_", name)[-80:] or "clip"


class ClipStore:
    def __init__(self, root: Path):
        self.root = root
        path = root / "index.json"
        if path.exists():
            data = json.loads(path.read_text(encoding="utf-8"))
            self.index = Index(data.get("channels", []), [Clip(**c) for c in data.get("clips", [])],
                               data.get("trash", []))
        else:
            self.index = Index()

    def save(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        tmp = self.root / "index.json.tmp"
        data = {"channels": self.index.channels, "clips": [asdict(c) for c in self.index.clips],
                "trash": self.index.trash}
        tmp.write_text(json.dumps(data, indent=2), encoding="utf-8")
        tmp.replace(self.root / "index.json")

    def archiving(self, channel_id: int) -> bool:
        return channel_id in self.index.channels

    def toggle(self, channel_id: int) -> bool:
        """Turn archiving on or off for a channel. Returns the new state."""
        on = channel_id not in self.index.channels
        if on:
            self.index.channels.append(channel_id)
        else:
            self.index.channels.remove(channel_id)
        self.save()
        return on

    def path_for(self, guild: int, message: int, filename: str, when: datetime) -> Path:
        return self.root / str(guild) / f"{when:%Y-%m-%d}-{message}-{safe_name(filename)}"

    def add(self, clip: Clip) -> None:
        self.index.clips.append(clip)
        self.save()

    def _drop_files(self, clip: Clip) -> None:
        if clip.file:
            (self.root / clip.file).unlink(missing_ok=True)
        if clip.remote:
            self.index.trash.append(clip.remote)

    def remove(self, message_id: int) -> None:
        """Drop a clip and its file ("nobody saw that")."""
        for clip in self.find(message_id):
            self._drop_files(clip)
        self.index.clips = [c for c in self.index.clips if c.id != message_id]
        self.save()

    def clear_all(self) -> None:
        """Every archived file and the index; which channels archive stays."""
        for clip in self.index.clips:
            self._drop_files(clip)
        self.index.clips = []
        self.save()

    def find(self, message_id: int) -> list[Clip]:
        return [c for c in self.index.clips if c.id == message_id]

    def star(self, message_id: int, keeper: bool = True) -> bool:
        clips = self.find(message_id)
        for c in clips:
            c.keeper = keeper
        if clips:
            self.save()
        return bool(clips)

    def latest(self, guild: int, author: int | None = None, keepers: bool = False, limit: int = 10) -> list[Clip]:
        found = [c for c in self.index.clips if c.guild == guild and (author is None or c.author == author)
                 and (c.keeper or not keepers)]
        return sorted(found, key=lambda c: c.posted, reverse=True)[:limit]

    def clean(self, now: datetime, days: int) -> list[Clip]:
        """Delete clips older than `days` that nobody starred. Returns the ones removed. 0 days = keep all."""
        if days <= 0:
            return []
        cutoff = (now - timedelta(days=days)).isoformat(timespec="seconds")
        old = [c for c in self.index.clips if not c.keeper and c.posted[:19] < cutoff[:19]]
        for c in old:
            self._drop_files(c)
        if old:
            self.index.clips = [c for c in self.index.clips if c not in old]
            self.save()
        return old

    def total_bytes(self, guild: int) -> int:
        return sum(c.size for c in self.index.clips if c.guild == guild)

    def search(self, guild: int, query: str, limit: int = 10) -> list[Clip]:
        """Clips whose title or poster matches the most words of the query, newest first among equals."""
        words = set(re.findall(r"[a-z0-9']{2,}", query.lower()))
        scored = []
        for c in self.index.clips:
            if c.guild != guild:
                continue
            text = f"{c.title} {c.author_name}".lower()
            hits = sum(w in text for w in words)
            if hits:
                scored.append((hits, c.posted, c))
        scored.sort(key=lambda s: (s[0], s[1]), reverse=True)
        return [c for _, _, c in scored[:limit]]

    def stats(self, guild: int) -> list[tuple[str, int, int]]:
        """(poster, clips, keepers), most clips first."""
        per: dict[int, list] = {}
        for c in self.index.clips:
            if c.guild == guild:
                row = per.setdefault(c.author, [c.author_name, 0, 0])
                row[0], row[1], row[2] = c.author_name, row[1] + 1, row[2] + c.keeper
        return sorted((tuple(r) for r in per.values()), key=lambda r: (-r[1], -r[2]))

    def this_week(self, channel: int, now: datetime) -> list[Clip]:
        cutoff = (now - timedelta(days=7)).isoformat(timespec="seconds")[:19]
        return [c for c in self.index.clips if c.channel == channel and c.posted[:19] >= cutoff]

    def weekly_done(self, channel: int, now: datetime) -> bool:
        path = self.root / "weekly.json"
        done = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
        return done.get(str(channel)) == f"{now:%G-W%V}"

    def mark_weekly(self, channel: int, now: datetime) -> None:
        path = self.root / "weekly.json"
        done = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
        done[str(channel)] = f"{now:%G-W%V}"
        self.root.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(done), encoding="utf-8")


def pick_of_the_week(clips: list[Clip], stars: dict[int, int]) -> Clip | None:
    """Most ⭐ wins, newest breaks ties. Needs at least one star."""
    best = max(clips, key=lambda c: (stars.get(c.id, 0), c.posted), default=None)
    return best if best and stars.get(best.id, 0) > 0 else None


def fallback_title(author_name: str, filename: str) -> str:
    stem = re.sub(r"[_\-.]+", " ", Path(filename).stem).strip()
    return stem if stem and not re.fullmatch(r"[\d\s]+", stem) else f"a {author_name} moment"
