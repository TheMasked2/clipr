"""Saved clips and the web gallery.

A ⭐ on a clip in #clips asks whoever starred it which shelf it goes on (funny sayings, aimer clips, BS clips,
random). Pumpkin then keeps its own copy on the server's disk, so it no longer counts against R2, which lets
clips expire after clip_days as usual:

    data/saved/index.json       every saved clip's details, and each person's gallery key
    data/saved/<id>.mp4, .jpg   the video and its thumbnail

The gallery is a small website served by Pumpkin itself (aiohttp, GALLERY_PORT, 8080 by default). The server has
no domain, so it's on the bare IP and nothing in it is public: each person opens it with their own link from
`/clips gallery`, which sets a cookie. They see the saved clips of the servers they're in. Whoever clipped it,
whoever's screen it is, and the owner can rename it, move it to another shelf or delete it.
"""

import asyncio
import json
import logging
import os
import secrets
import shutil
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path

from aiohttp import web

log = logging.getLogger(__name__)

CATEGORIES = {"funny": "😂 funny sayings", "aim": "🎯 aimer clips", "bs": "🤡 BS clips", "random": "🎲 random"}
PAGE = Path(__file__).with_name("gallery.html")
COOKIE = "pumpkin_gallery"


@dataclass
class Saved:
    id: str
    clip: int  # the Discord message id of the clip in #clips
    guild: int
    title: str
    category: str
    asker: int
    asker_name: str
    streamer: int
    streamer_name: str
    names: str  # who was in the call
    saved_by: int
    created: str  # ISO time the clip was made
    seconds: float = 0.0
    size: int = 0


class Shelf:
    def __init__(self, root: Path):
        self.root = root
        self.clips: dict[str, Saved] = {}
        self.keys: dict[str, str] = {}  # key -> user id
        path = root / "index.json"
        if path.exists():
            data = json.loads(path.read_text(encoding="utf-8"))
            self.clips = {c["id"]: Saved(**c) for c in data.get("clips", [])}
            self.keys = data.get("keys", {})

    def save(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        tmp = self.root / "index.json.tmp"
        tmp.write_text(json.dumps({"clips": [asdict(c) for c in self.clips.values()], "keys": self.keys}, indent=1),
                       encoding="utf-8")
        tmp.replace(self.root / "index.json")

    def video(self, sid: str) -> Path:
        return self.root / f"{sid}.mp4"

    def thumb(self, sid: str) -> Path:
        return self.root / f"{sid}.jpg"

    def by_clip(self, clip_id: int) -> Saved | None:
        return next((c for c in self.clips.values() if c.clip == clip_id), None)

    def key_for(self, user_id: int) -> str:
        """Their personal gallery key (made the first time)."""
        for key, uid in self.keys.items():
            if uid == str(user_id):
                return key
        key = secrets.token_urlsafe(24)
        self.keys[key] = str(user_id)
        self.save()
        return key

    def user_for(self, key: str | None) -> int | None:
        uid = self.keys.get(key or "")
        return int(uid) if uid else None

    async def add(self, entry: Saved, video: bytes) -> Saved:
        """Keep the video and make its thumbnail."""
        self.root.mkdir(parents=True, exist_ok=True)
        path = self.video(entry.id)
        path.write_bytes(video)
        entry.size = len(video)
        entry.seconds = await length_of(path)
        await make_thumb(path, self.thumb(entry.id), entry.seconds)
        self.clips[entry.id] = entry
        self.save()
        return entry

    def remove(self, sid: str) -> None:
        self.clips.pop(sid, None)
        self.video(sid).unlink(missing_ok=True)
        self.thumb(sid).unlink(missing_ok=True)
        self.save()

    def bytes_used(self) -> int:
        return sum(c.size for c in self.clips.values())


async def length_of(path: Path) -> float:
    ffprobe = shutil.which("ffprobe")
    if not ffprobe:
        return 0.0
    proc = await asyncio.create_subprocess_exec(
        ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path),
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    out, _ = await proc.communicate()
    try:
        return float(out.decode().strip())
    except ValueError:
        return 0.0


async def make_thumb(video: Path, out: Path, seconds: float) -> None:
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        return
    at = min(max(seconds / 3, 0.0), 10.0)  # a third in: past the start, usually where it gets interesting
    proc = await asyncio.create_subprocess_exec(
        ffmpeg, "-loglevel", "error", "-y", "-ss", f"{at:.2f}", "-i", str(video), "-frames:v", "1",
        "-vf", "scale=640:360", "-q:v", "4", str(out),
        stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
    await proc.wait()


def new_id() -> str:
    return secrets.token_urlsafe(9)


def gallery_url(env=os.environ) -> str:
    """Where the gallery lives: GALLERY_URL, else the server's address from PUMPKIN_PUBLIC_SSH and GALLERY_PORT."""
    if url := env.get("GALLERY_URL", "").strip():
        return url.rstrip("/")
    host = env.get("PUMPKIN_PUBLIC_SSH", "").strip().rsplit("@", 1)[-1] or "localhost"
    return f"http://{host}:{env.get('GALLERY_PORT', '8080')}"


class Gallery:
    """The website. `bot` is asked who's in which server and who the owner is."""

    def __init__(self, bot, shelf: Shelf, port: int):
        self.bot, self.shelf, self.port = bot, shelf, port
        self.runner: web.AppRunner | None = None

    def app(self) -> web.Application:
        app = web.Application(client_max_size=64 * 1024)
        app.add_routes([
            web.get("/", self.page), web.get("/g", self.enter), web.get("/api/clips", self.list),
            web.post("/api/clips/{id}", self.edit), web.delete("/api/clips/{id}", self.delete),
            web.get("/v/{id}.mp4", self.video), web.get("/t/{id}.jpg", self.thumb),
        ])
        return app

    async def start(self) -> None:
        self.runner = web.AppRunner(self.app(), access_log=None)
        await self.runner.setup()
        await web.TCPSite(self.runner, "0.0.0.0", self.port).start()
        log.info("Clip gallery on port %s", self.port)

    async def stop(self) -> None:
        if self.runner:
            await self.runner.cleanup()

    # --- who may do what
    def viewer(self, request: web.Request) -> int | None:
        return self.shelf.user_for(request.cookies.get(COOKIE))

    def is_owner(self, uid: int) -> bool:
        return self.bot.settings.is_owner(uid)

    def can_see(self, uid: int, clip: Saved) -> bool:
        guild = self.bot.get_guild(clip.guild)
        return self.is_owner(uid) or bool(guild and guild.get_member(uid))

    def can_edit(self, uid: int, clip: Saved) -> bool:
        return self.is_owner(uid) or uid in (clip.asker, clip.streamer)

    def _clip(self, request: web.Request, edit: bool = False) -> tuple[int, Saved]:
        uid = self.viewer(request)
        if uid is None:
            raise web.HTTPUnauthorized(text="open the gallery with your link from /clips gallery")
        clip = self.shelf.clips.get(request.match_info["id"])
        if clip is None or not self.can_see(uid, clip):
            raise web.HTTPNotFound()
        if edit and not self.can_edit(uid, clip):
            raise web.HTTPForbidden(text="only whoever clipped it, whoever's screen it is, or the owner")
        return uid, clip

    # --- routes
    async def enter(self, request: web.Request) -> web.Response:
        key = request.query.get("k", "")
        if self.shelf.user_for(key) is None:
            raise web.HTTPUnauthorized(text="that link doesn't work (anymore). get yours with /clips gallery")
        response = web.HTTPFound("/")
        response.set_cookie(COOKIE, key, max_age=365 * 86400, httponly=True, samesite="Lax")
        raise response

    async def page(self, request: web.Request) -> web.Response:
        if self.viewer(request) is None:
            return web.Response(text="🎃 this is Pumpkin's clip gallery. open it with your own link: run /clips "
                                     "gallery in Discord.", content_type="text/plain")
        return web.FileResponse(PAGE, headers={"Cache-Control": "no-cache"})

    async def list(self, request: web.Request) -> web.Response:
        uid = self.viewer(request)
        if uid is None:
            raise web.HTTPUnauthorized()
        clips = sorted((c for c in self.shelf.clips.values() if self.can_see(uid, c)),
                       key=lambda c: c.created, reverse=True)
        return web.json_response({
            "categories": CATEGORIES, "owner": self.is_owner(uid),
            "clips": [{**asdict(c), "clip": str(c.clip), "guild": str(c.guild), "asker": str(c.asker),
                       "streamer": str(c.streamer), "saved_by": str(c.saved_by),
                       "server": getattr(self.bot.get_guild(c.guild), "name", ""),
                       "can_edit": self.can_edit(uid, c)} for c in clips]})

    async def edit(self, request: web.Request) -> web.Response:
        _, clip = self._clip(request, edit=True)
        try:
            data = await request.json()
        except (ValueError, UnicodeDecodeError):
            raise web.HTTPBadRequest(text="bad json")
        if "title" in data:
            title = " ".join(str(data["title"]).split())[:100]
            if not title:
                raise web.HTTPBadRequest(text="a title can't be empty")
            clip.title = title
        if "category" in data:
            if data["category"] not in CATEGORIES:
                raise web.HTTPBadRequest(text="no such shelf")
            clip.category = data["category"]
        self.shelf.save()
        return web.json_response({"ok": True, "title": clip.title, "category": clip.category})

    async def delete(self, request: web.Request) -> web.Response:
        uid, clip = self._clip(request, edit=True)
        self.shelf.remove(clip.id)
        log.info("Saved clip %s deleted from the gallery by %s", clip.id, uid)
        return web.json_response({"ok": True})

    async def video(self, request: web.Request) -> web.StreamResponse:
        _, clip = self._clip(request)
        return web.FileResponse(self.shelf.video(clip.id))  # handles Range, so the player can skip around

    async def thumb(self, request: web.Request) -> web.StreamResponse:
        _, clip = self._clip(request)
        path = self.shelf.thumb(clip.id)
        if not path.exists():
            raise web.HTTPNotFound()
        return web.FileResponse(path, headers={"Cache-Control": "private, max-age=86400"})


def entry_for(clip, category: str, saved_by: int) -> Saved:
    """A shelf entry for a clip from the clips archive (`clips.Clip`)."""
    return Saved(id=new_id(), clip=clip.id, guild=clip.guild,
                 title=clip.title or f"{clip.streamer_name or clip.author_name}'s clip",
                 category=category, asker=clip.author, asker_name=clip.author_name, streamer=clip.streamer,
                 streamer_name=clip.streamer_name, names=clip.names, saved_by=saved_by,
                 created=clip.posted or datetime.now(timezone.utc).isoformat())
