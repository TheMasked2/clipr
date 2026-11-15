"""Links to PC agents (agent/pumpkin_agent.py), one per person, for screen clips.

An agent keeps a rolling buffer of its person's screen while they Go Live. When a clip is made, Pumpkin asks the
right person's agent for the same stretch of video and posts it with the call audio. Agents reach this websocket
through an SSH tunnel, so it only listens on the server's localhost, and each brings its own token:
PUMPKIN_AGENT_TOKEN in .env is the owner's; anyone else gets one from /agent link (stored hashed in
data/agents.json, with the tunnel-only SSH key they paired).
"""

import asyncio
import hashlib
import hmac
import json
import logging
import re
import secrets
import struct
from dataclasses import dataclass, field
from pathlib import Path

from aiohttp import WSMsgType, web

log = logging.getLogger(__name__)

PORT = 8765
AGENT_FILE = Path(__file__).resolve().parent.parent / "agent" / "pumpkin_agent.py"  # what friends' agents update to
MAX_MSG = 32 * 1024 * 1024
MORE = 1 << 63  # a clip piece with more to come
CLIP_TIMEOUT_S = 90  # the agent cuts at idle priority, so a busy game makes it slower
# Discord drops self_stream for a second or two now and then (switching what you share, a hiccup): wait this long
# before telling an agent to stop, so a blip doesn't throw its buffer away.
OFF_GRACE_S = 20
PUBLIC_KEY = re.compile(r"ssh-ed25519 [A-Za-z0-9+/]{68}")
# A paired key can only open the tunnel to the agent port: no shell, no other forwarding.
KEY_OPTIONS = ('restrict,port-forwarding,permitopen="127.0.0.1:{port}",permitlisten="127.0.0.1:1",'
               'command="/bin/false"')


def digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


@dataclass
class Agent:
    user_id: int
    ws: web.WebSocketResponse
    recording: bool = False
    version: str = ""
    autostart: bool | None = None  # None: an agent too old to say
    can_save: bool = False  # keeps clips for after the stream (newer agents)
    pending: dict[int, asyncio.Future] = field(default_factory=dict)
    parts: dict[int, list[bytes]] = field(default_factory=dict)


class AgentLink:
    def __init__(self, owner_token: str, owner_id: int = 0, store: Path | None = None,
                 authorized_keys: Path | None = None):
        self.owner_token, self.owner_id = owner_token, owner_id
        self.store = store
        self.authorized_keys = authorized_keys
        self.people: dict[str, dict] = json.loads(store.read_text(encoding="utf-8")) \
            if store and store.exists() else {}  # user id -> {"token": sha256, "key": public key}
        self.agents: dict[int, Agent] = {}
        self.wanted: dict[int, bool] = {}  # who should be recording right now (sent again on reconnect)
        self._stopping: dict[int, asyncio.Task] = {}  # pending "off" orders, still inside the grace time
        self.next_id = 1
        self.on_connect = None  # async callback(user_id): tell that agent whether to record right now
        # Clips saved for after the stream arrive here; async callback(user_id, clip id, mp4 path or None, why)
        self.inbox: Path | None = None
        self.on_saved_clip = None
        self.acks: dict[int, asyncio.Future] = {}
        self._runner: web.AppRunner | None = None
        self.code = AGENT_FILE.read_text(encoding="utf-8") if AGENT_FILE.exists() else ""
        self.version = hashlib.sha256(self.code.encode()).hexdigest()[:12] if self.code else ""

    # Who may connect

    def _save(self) -> None:
        if self.store:
            self.store.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.store.with_suffix(".tmp")
            tmp.write_text(json.dumps(self.people, indent=1), encoding="utf-8")
            tmp.replace(self.store)

    def who(self, token: str) -> int | None:
        if self.owner_token and hmac.compare_digest(token, self.owner_token):
            return self.owner_id
        hashed = digest(token)
        for uid, person in self.people.items():
            if hmac.compare_digest(hashed, person["token"]):
                return int(uid)
        return None

    def pair(self, user_id: int, public_key: str) -> str:
        """Allow this person's agent: its key may open the tunnel, and the returned token logs it in.
        Pairing again replaces their old key and token."""
        key = PUBLIC_KEY.search(public_key or "")
        if not key:
            raise ValueError("that isn't an ed25519 public key (it starts with ssh-ed25519)")
        token = secrets.token_urlsafe(32)
        self.people[str(user_id)] = {"token": digest(token), "key": key.group(0)}
        self._save()
        self._write_keys()
        return token

    def unpair(self, user_id: int) -> bool:
        gone = self.people.pop(str(user_id), None) is not None
        if gone:
            self._save()
            self._write_keys()
        if agent := self.agents.get(user_id):
            asyncio.ensure_future(agent.ws.close())
        return gone

    def paired(self, user_id: int) -> bool:
        return user_id == self.owner_id and bool(self.owner_token) or str(user_id) in self.people

    def _write_keys(self) -> None:
        """Pumpkin's own block in authorized_keys, rewritten from data/agents.json. Other lines stay as they are."""
        if not self.authorized_keys:
            return
        path = self.authorized_keys
        old = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
        kept = [line for line in old if not re.search(r" pumpkin-agent-\d+$", line.rstrip())]
        ours = [f"{KEY_OPTIONS.format(port=PORT)} {p['key']} pumpkin-agent-{uid}" for uid, p in self.people.items()]
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_name(path.name + ".tmp")
        tmp.write_text("\n".join(kept + ours) + "\n", encoding="utf-8")
        tmp.chmod(0o600)
        tmp.replace(path)

    # The websocket

    def connected(self, user_id: int) -> bool:
        agent = self.agents.get(user_id)
        return agent is not None and not agent.ws.closed

    def recording(self, user_id: int) -> bool:
        return self.connected(user_id) and self.agents[user_id].recording

    async def start(self) -> None:
        if not self.owner_token and not self.people:
            log.info("No PUMPKIN_AGENT_TOKEN and nobody paired; screen clips wait for /agent link")
        app = web.Application()
        app.router.add_get("/agent", self._handle)
        self._runner = web.AppRunner(app, access_log=None)
        await self._runner.setup()
        await web.TCPSite(self._runner, "127.0.0.1", PORT).start()
        log.info("Waiting for PC agents on 127.0.0.1:%s", PORT)

    async def _handle(self, request: web.Request) -> web.StreamResponse:
        uid = self.who(request.query.get("token", ""))
        if uid is None:
            return web.Response(status=403)
        ws = web.WebSocketResponse(max_msg_size=MAX_MSG, heartbeat=30)
        await ws.prepare(request)
        if old := self.agents.get(uid):
            await old.ws.close()  # a fresh agent replaces a stale one
        agent = self.agents[uid] = Agent(uid, ws)
        log.info("PC agent connected (%s)", uid)
        if self.on_connect:
            await self.on_connect(uid)
        try:
            async for msg in ws:
                if msg.type == WSMsgType.BINARY and len(msg.data) >= 8:
                    (rid,) = struct.unpack(">Q", msg.data[:8])
                    if (rid & ~MORE) not in agent.pending:  # not an answer: a saved clip arriving after the stream
                        self._upload_piece(agent, rid, msg.data[8:])
                        continue
                    if rid & MORE:  # newer agents send a clip in paced pieces; the last one has the plain id
                        agent.parts.setdefault(rid & ~MORE, []).append(msg.data[8:])
                        continue
                    data = b"".join(agent.parts.pop(rid, [])) + msg.data[8:]
                    future = agent.pending.pop(rid, None)
                    if future and not future.done():
                        future.set_result(data or None)
                elif msg.type == WSMsgType.TEXT:
                    await self._said(agent, msg.data)
        finally:
            if self.agents.get(uid) is agent:
                del self.agents[uid]
                log.info("PC agent disconnected (%s)", uid)
        return ws

    def _upload_piece(self, agent: Agent, rid: int, data: bytes) -> None:
        sid = rid & ~MORE
        if self.inbox is None:
            return
        self.inbox.mkdir(parents=True, exist_ok=True)
        part = self.inbox / f"{sid}.part"
        if data:
            with open(part, "ab") as f:
                f.write(data)
        if rid & MORE:
            return
        if not part.exists():  # an empty last piece: the clip failed, and saved_clip already said so
            return
        done = part.with_suffix(".mp4")
        part.replace(done)
        log.info("Saved clip %s from %s arrived (%.1f MB)", sid, agent.user_id, done.stat().st_size / 1e6)
        if self.on_saved_clip:
            asyncio.ensure_future(self.on_saved_clip(agent.user_id, sid, done, ""))

    async def save(self, user_id: int, seconds: int) -> tuple[int | None, str]:
        """Ask this person's agent to keep the last `seconds`: (clip id, the game they're in or "") or (None, why)."""
        if not self.recording(user_id) or not self.agents[user_id].can_save:
            return None, "their agent can't save clips"
        sid = secrets.randbits(62)  # survives restarts on both ends, so never a small counter that starts over
        future = asyncio.get_running_loop().create_future()
        self.acks[sid] = future
        try:
            await self.agents[user_id].ws.send_str(json.dumps({"op": "save", "id": sid, "seconds": seconds}))
            answer = await asyncio.wait_for(future, 20)
        except (asyncio.TimeoutError, ConnectionError) as e:
            return None, f"no answer from the agent ({type(e).__name__})"
        finally:
            self.acks.pop(sid, None)
        return (sid, str(answer.get("game", ""))[:60]) if answer.get("ok") else \
            (None, str(answer.get("why", ""))[:200])

    async def _said(self, agent: Agent, text: str) -> None:
        try:
            said = json.loads(text)
        except ValueError:
            said = None
        if isinstance(said, dict) and said.get("op") == "saved":
            if (future := self.acks.get(int(said.get("id", 0)))) and not future.done():
                future.set_result(said)
            return
        if isinstance(said, dict) and said.get("op") == "saved_clip":
            if not said.get("ok"):
                log.info("PC agent %s couldn't make saved clip %s: %s", agent.user_id, said.get("id"), said.get("why"))
                if self.on_saved_clip:
                    asyncio.ensure_future(self.on_saved_clip(agent.user_id, int(said["id"]), None,
                                                             str(said.get("why", ""))))
            elif self.inbox:
                (self.inbox / f"{int(said['id'])}.part").unlink(missing_ok=True)  # a resend starts clean
            return
        if not isinstance(said, dict) or said.get("op") != "hi":
            log.info("PC agent %s: %s", agent.user_id, text[:300])
            return
        agent.version, agent.autostart = str(said.get("version", "")), bool(said.get("autostart"))
        agent.can_save = bool(said.get("save"))
        log.info("PC agent %s: encoder %s, version %s", agent.user_id, said.get("encoder"), agent.version)
        # The owner runs the repo copy (never overwritten from here); everyone else gets the newest one.
        if agent.user_id != self.owner_id and self.version and agent.version != self.version:
            log.info("Updating PC agent %s to %s", agent.user_id, self.version)
            await agent.ws.send_str(json.dumps({"op": "update", "code": self.code}))

    async def send(self, user_id: int, order: dict) -> bool:
        if not self.connected(user_id):
            return False
        await self.agents[user_id].ws.send_str(json.dumps(order))
        return True

    async def record(self, user_id: int, on: bool, now: bool = False) -> None:
        if pending := self._stopping.pop(user_id, None):
            pending.cancel()
        if not on and not now and self.wanted.get(user_id):
            self._stopping[user_id] = asyncio.ensure_future(self._stop_later(user_id))
            return
        if on != self.wanted.get(user_id, False):
            log.info("Screen recording %s for %s (agent %s)", "on" if on else "off", user_id,
                     "connected" if self.connected(user_id) else "not connected")
        self.wanted[user_id] = on
        if self.connected(user_id) and self.agents[user_id].recording != on:
            agent = self.agents[user_id]
            agent.recording = on
            await agent.ws.send_str(json.dumps({"op": "record", "on": on}))

    async def _stop_later(self, user_id: int) -> None:
        await asyncio.sleep(OFF_GRACE_S)
        self._stopping.pop(user_id, None)
        await self.record(user_id, False, now=True)

    async def clip(self, user_id: int, seconds: int) -> bytes | None:
        """The last `seconds` of this person's screen as an mp4 without sound, or None."""
        if not self.recording(user_id):
            return None
        agent = self.agents[user_id]
        rid, self.next_id = self.next_id, self.next_id + 1
        future = asyncio.get_running_loop().create_future()
        agent.pending[rid] = future
        await agent.ws.send_str(json.dumps({"op": "clip", "id": rid, "seconds": seconds}))
        try:
            return await asyncio.wait_for(future, CLIP_TIMEOUT_S)
        except asyncio.TimeoutError:
            log.warning("PC agent %s took too long for a %ss clip", user_id, seconds)
            return None
        finally:
            agent.pending.pop(rid, None)
            agent.parts.pop(rid, None)
