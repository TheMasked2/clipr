import asyncio

import httpx
import numpy as np

from pumpkin.stt import Provider, Transcriber, load_providers
from pumpkin.voice import (RATE, SPOKEN, Track, Utterances, mix, parse_command, to_16k, to_mono, wav_bytes)

FRAME = RATE // 50  # 20 ms


def tone(n=FRAME, level=3000):
    return np.full(n, level, dtype=np.int16)


def test_parse_command():
    assert parse_command("Pumpkin, clip that!") == ("clip", 30)
    assert parse_command("pumpkin clip the whole fight") == ("clip", 120)
    assert parse_command("Pumpkin clip the last 10 seconds") == ("clip", 10)
    assert parse_command("pumpkin, clip the last two minutes") == ("clip", 120)
    assert parse_command("pumpkin clip the last minute") == ("clip", 60)
    assert parse_command("pumpkin clip the last 2 seconds") == ("clip", 5)  # at least 5
    assert parse_command("Pumpkin, nobody saw that.") == ("undo", 0)
    assert parse_command("pumpkin knip dat") == ("clip", 30)
    assert parse_command("Pumkin clip it") == ("clip", 30)  # Whisper spelling
    assert parse_command("pumpkin post that now") is None  # no instant posting anymore
    assert parse_command("pumpkin clip the last 10 seconds right now") == ("clip", 10)
    assert parse_command("pumpkin clip the whole fight") == ("clip", 120)  # saved, full quality later
    assert parse_command("clip that") is None  # no name, no clip
    assert parse_command("pumpkin you're so dumb") is None
    assert parse_command("") is None


def test_offline_phrases_all_parse():
    for phrase in SPOKEN:
        if phrase not in ("pumpkin", "[unk]"):
            assert parse_command(phrase), phrase


def test_to_mono_and_16k():
    stereo = np.array([100, 300, -50, -150], dtype=np.int16).tobytes()
    assert list(to_mono(stereo)) == [200, -100]
    assert list(to_16k(np.array([3, 6, 9, 1, 1, 1, 7], dtype=np.int16))) == [6, 1]


def test_wav_bytes_header():
    data = wav_bytes(np.zeros(16000, np.int16), 16000)
    assert data[:4] == b"RIFF" and len(data) == 44 + 32000


def test_track_lays_bursts_end_to_end_and_forgets_old():
    t = Track(keep_s=1.0)
    t.add(10.0, tone())
    t.add(10.005, tone())  # jitter: still right after the first frame
    assert abs(t.frames[1][0] - (t.frames[0][0] + 0.02)) < 1e-9
    t.add(12.0, tone())  # a new stretch, and the old ones are past 1 s
    assert len(t.frames) == 1 and abs(t.frames[0][0] - 11.98) < 1e-9


def test_mix_places_and_sums():
    a, b = Track(), Track()
    a.add(1.02, tone(level=1000))  # covers 1.00-1.02
    b.add(1.02, tone(level=2000))
    out = mix([a, b], 1.0, 1.04)
    assert len(out) == 2 * FRAME
    assert out[0] == 3000 and out[-1] == 0


def test_mix_scales_instead_of_clipping():
    tracks = []
    for _ in range(3):
        t = Track()
        t.add(1.02, tone(level=20000))
        tracks.append(t)
    assert mix(tracks, 1.0, 1.02).max() >= 32766  # scaled to full, not wrapped around


def test_utterances_end_on_quiet_or_missing_packets():
    u = Utterances()
    now = 0.0
    for _ in range(30):  # 0.6 s of talking
        now += 0.02
        assert u.feed(now, tone()) is None
    got = None
    for _ in range(30):  # then quiet frames
        now += 0.02
        got = got if got is not None else u.feed(now, tone(level=0))
    assert got is not None and len(got) >= 30 * FRAME
    for _ in range(30):
        now += 0.02
        u.feed(now, tone())
    assert u.idle(now + 0.1) is None
    assert u.idle(now + 0.6) is not None  # packets just stopped
    for _ in range(5):  # a cough is too short to count
        now += 0.02
        u.feed(now, tone())
    assert u.idle(now + 1) is None


class Fake:
    def __init__(self, responses):
        self.responses = responses
        self.calls = []

    async def post(self, url, **kwargs):
        self.calls.append(url)
        status, body = self.responses.pop(0)
        return httpx.Response(status, json=body, request=httpx.Request("POST", url))


def test_transcriber_falls_over_and_rests():
    providers = [Provider("groq turbo", "groq", "k", "whisper-large-v3-turbo"),
                 Provider("groq v3", "groq", "k", "whisper-large-v3"),
                 Provider("cf", "cloudflare", "t", "@cf/openai/whisper-large-v3-turbo", account="a")]
    fake = Fake([(429, {}), (500, {}), (200, {"result": {"text": "Pumpkin, clip that."}}),
                 (200, {"result": {"text": "again"}})])
    stt = Transcriber(providers, client=fake)
    assert asyncio.run(stt.transcribe(b"wav")) == "Pumpkin, clip that."
    assert "cloudflare" in fake.calls[2]
    assert asyncio.run(stt.transcribe(b"wav")) == "again"  # the first two are resting
    assert len(fake.calls) == 4


def test_transcriber_none_when_all_resting():
    stt = Transcriber([Provider("groq turbo", "groq", "k", "m")], client=Fake([(429, {})]))
    assert asyncio.run(stt.transcribe(b"wav")) is None
    assert asyncio.run(stt.transcribe(b"wav")) is None  # resting, not even asked


def test_load_providers():
    env = {"GROQ_API_KEY": "g", "CLOUDFLARE_API_TOKEN": "t", "CLOUDFLARE_ACCOUNT_ID": "a"}
    assert [p.name for p in load_providers(env)] == ["groq turbo", "groq v3", "cloudflare whisper"]
    assert [p.name for p in load_providers({"CLOUDFLARE_API_TOKEN": "t"})] == []  # no account id


def test_offline_guess_only_trusted_when_clean():
    from pumpkin.voice import offline_sure
    assert offline_sure("pumpkin clip") and offline_sure("pumpkin nobody saw that")
    assert not offline_sure("thing clip pumpkin [unk]")  # "I think the pumpkin pie was great"
    assert not offline_sure("pumpkin [unk]") and not offline_sure("pumpkin")


def test_patched_decoder_tries_every_key_and_survives_bad_frames():
    import discord
    from types import SimpleNamespace
    from discord.ext.voice_recv import opus as recv_opus
    from pumpkin.voice import DAVE_STATS, patch_decoder
    patch_decoder()
    added = {}

    class Dave:
        ready, user_id = True, 1

        def get_user_ids(self):
            return ["1", "2", "3"]

        def decrypt(self, uid, kind, frame):
            if uid != 3:
                raise ValueError("wrong key")
            return b"opus:" + frame

    vc = SimpleNamespace(_connection=SimpleNamespace(dave_session=Dave(), dave_protocol_version=1),
                         _get_id_from_ssrc=lambda ssrc: None, _add_ssrc=lambda uid, ssrc: added.update({ssrc: uid}))
    d = recv_opus.PacketDecoder.__new__(recv_opus.PacketDecoder)
    d.router = SimpleNamespace(sink=SimpleNamespace(voice_client=vc))
    d.ssrc, d._cached_id = 99, None
    packet = SimpleNamespace(decrypted_data=b"x")
    d._dave_decrypt(packet)
    assert packet.decrypted_data == b"opus:x" and d._cached_id == 3 and added == {99: 3}

    class Broken:
        def decode(self, data, fec=False):
            raise discord.opus.OpusError.__new__(discord.opus.OpusError)  # no opus lib needed

    d._decoder = Broken()
    before = DAVE_STATS.get("undecodable", 0)
    assert d._decode_packet(SimpleNamespace(decrypted_data=b"junk"))[1] == b"\x00" * 3840
    assert DAVE_STATS["undecodable"] == before + 1


def test_agent_link_round_trip():
    """The bot side of the screen link: a fake agent gets the orders and answers a clip."""
    import json
    import struct
    import aiohttp
    from pumpkin import agent_link
    from pumpkin.agent_link import AgentLink

    async def go():
        agent_link.PORT, agent_link.OFF_GRACE_S = 18765, 0.05
        link = AgentLink("secret", owner_id=9)
        await link.start()
        async with aiohttp.ClientSession() as http:
            bad = await http.get("http://127.0.0.1:18765/agent", params={"token": "nope"})
            assert bad.status == 403
            async with http.ws_connect("http://127.0.0.1:18765/agent", params={"token": "secret"}) as ws:
                for _ in range(50):
                    if link.connected(9):
                        break
                    await asyncio.sleep(0.02)
                await link.record(9, True)
                assert json.loads((await ws.receive()).data) == {"op": "record", "on": True}
                asking = asyncio.create_task(link.clip(9, 30))
                order = json.loads((await ws.receive()).data)
                assert order["op"] == "clip" and order["seconds"] == 30
                await ws.send_bytes(struct.pack(">Q", order["id"]) + b"mp4!")
                assert await asking == b"mp4!"
                asking = asyncio.create_task(link.clip(9, 30))  # a newer agent: paced pieces
                order = json.loads((await ws.receive()).data)
                await ws.send_bytes(struct.pack(">Q", order["id"] | agent_link.MORE) + b"one,")
                await ws.send_bytes(struct.pack(">Q", order["id"] | agent_link.MORE) + b"two,")
                await ws.send_bytes(struct.pack(">Q", order["id"]) + b"three")
                assert await asking == b"one,two,three" and not link.agents[9].parts
                await link.record(9, False)  # a blip: on again inside the grace time, nothing is sent
                await link.record(9, True)
                assert link.recording(9) and not link._stopping
                await link.record(9, False)
                assert json.loads((await ws.receive()).data) == {"op": "record", "on": False}
                assert await link.clip(9, 30) is None  # not recording: nothing asked
        await link._runner.cleanup()

    try:
        asyncio.run(go())
    finally:
        agent_link.PORT, agent_link.OFF_GRACE_S = 8765, 20


def test_pairing_writes_a_tunnel_only_key(tmp_path):
    from pumpkin.agent_link import AgentLink
    keys = tmp_path / "authorized_keys"
    keys.write_text("ssh-ed25519 AAAAmine me@pc\n", encoding="utf-8")
    link = AgentLink("owner-secret", 9, tmp_path / "agents.json", keys)
    pub = "ssh-ed25519 " + "A" * 68 + " friend@pc"
    try:
        link.pair(5, "ssh-rsa AAAA nope")
        assert False
    except ValueError:
        pass
    token = link.pair(5, pub)
    assert link.who(token) == 5 and link.who("owner-secret") == 9 and link.who("guess") is None
    lines = keys.read_text(encoding="utf-8").splitlines()
    assert lines[0] == "ssh-ed25519 AAAAmine me@pc"  # other keys untouched
    assert 'permitopen="127.0.0.1:8765"' in lines[1] and 'command="/bin/false"' in lines[1]
    assert lines[1].endswith(" pumpkin-agent-5") and token not in (tmp_path / "agents.json").read_text()
    again = AgentLink("", 9, tmp_path / "agents.json", keys)
    assert again.who(token) == 5 and again.paired(5) and not again.paired(9)
    assert again.unpair(5) and keys.read_text(encoding="utf-8").splitlines() == ["ssh-ed25519 AAAAmine me@pc"]


def test_screen_follows_whoever_asks():
    from types import SimpleNamespace as NS
    from pumpkin.bot import Pumpkin

    def member(uid, name, live):
        return NS(id=uid, display_name=name, bot=False, voice=NS(self_stream=live))

    owner, friend = member(9, "Pieter", True), member(5, "Sam", True)
    asked = []

    async def save(uid, seconds):
        asked.append(uid)
        return 77, "valorant" if uid == 9 else ""

    async def record(uid, on):
        pass

    agents = {9}
    link = NS(paired=lambda uid: uid in agents, connected=lambda uid: uid in agents,
              recording=lambda uid: uid in agents, save=save, record=record,
              agents={9: NS(can_save=True), 5: NS(can_save=True), 3: NS(can_save=True)})
    prefs = {}
    bot = NS(agent=link, personal=NS(get=lambda uid: prefs.setdefault(uid, {})))
    bot.streamer_for = lambda session, asker: Pumpkin.streamer_for(bot, session, asker)
    session = NS(vc=NS(channel=NS(members=[owner, friend])))
    shot = lambda asker: asyncio.run(Pumpkin.screen_save(bot, session, 10, asker))
    sid, _, why = shot(5)  # Sam asked while both stream: Sam's screen, which has no agent
    assert sid is None and "Sam doesn't run the Pumpkin agent" in why and asked == []
    agents.add(5)
    assert shot(5) == (77, 5, "") and asked == [5]
    assert shot(9) == (77, 9, "valorant") and asked == [5, 9]
    listener = member(3, "Kim", False)
    session.vc.channel.members.append(listener)
    sid, _, why = shot(3)
    assert sid is None and "several people" in why  # Kim isn't streaming and two are: nobody's screen is guessed
    session.vc.channel.members.remove(friend)
    assert shot(3)[:2] == (77, 9)  # one streamer: theirs
    session.vc.channel.members.remove(owner)
    assert "nobody here is streaming" in shot(3)[2]
    agents.add(3)
    assert "/agent incall" in shot(3)[2]  # has the agent, forgot to go live: told about incall
    prefs[3] = {"agent_incall": True}
    assert shot(3)[:2] == (77, 3)  # Kim's own screen, recorded because Pumpkin is in the call
    session.vc.channel.members.append(owner)
    assert shot(3)[:2] == (77, 3)  # Kim asked: Kim's screen, not the streamer's
    assert shot(5)[:2] == (77, 9)  # anyone else gets the streamer: a screen Kim isn't showing is Kim's to clip


def test_agent_records_while_pumpkin_is_in_the_call_only_if_chosen():
    from types import SimpleNamespace as NS
    from pumpkin.bot import Pumpkin
    prefs = {}
    kim = NS(id=3)
    session = NS(vc=NS(channel=NS(members=[kim])))
    bot = NS(personal=NS(get=lambda uid: prefs.setdefault(uid, {})), streaming=lambda uid: None,
             voice=NS(sessions={1: session}))
    bot.in_call_with_pumpkin = lambda uid: Pumpkin.in_call_with_pumpkin(bot, uid)
    assert not Pumpkin.wants_recording(bot, 3)  # not live, not chosen: nothing records
    prefs[3] = {"agent_incall": True}
    assert Pumpkin.wants_recording(bot, 3)
    session.vc.channel.members.clear()
    assert not Pumpkin.wants_recording(bot, 3)  # Pumpkin isn't in their call: off again


def test_agent_update_offered_to_friends_only():
    import json
    import aiohttp
    from pumpkin import agent_link
    from pumpkin.agent_link import AgentLink

    async def go():
        agent_link.PORT = 18766
        link = AgentLink("secret", owner_id=9)
        link.people["5"] = {"token": agent_link.digest("friend"), "key": "k"}
        assert link.version and link.code.startswith("# /// script")
        await link.start()
        async with aiohttp.ClientSession() as http:
            async with http.ws_connect("http://127.0.0.1:18766/agent", params={"token": "friend"}) as ws:
                await ws.send_str(json.dumps({"op": "hi", "version": "old", "autostart": True}))
                order = json.loads((await ws.receive()).data)
                assert order["op"] == "update" and order["code"] == link.code
            async with http.ws_connect("http://127.0.0.1:18766/agent", params={"token": "secret"}) as ws:
                await ws.send_str(json.dumps({"op": "hi", "version": "old", "autostart": False}))
                for _ in range(50):
                    if link.connected(9) and link.agents[9].autostart is False:
                        break
                    await asyncio.sleep(0.02)
                assert link.agents[9].version == "old"  # the owner's repo copy is never overwritten
                assert await link.send(9, {"op": "quit"})
                assert json.loads((await ws.receive()).data) == {"op": "quit"}
        await link._runner.cleanup()

    try:
        asyncio.run(go())
    finally:
        agent_link.PORT = 8765


def test_clip_is_video_or_an_error_and_says_clipped():
    from types import SimpleNamespace as NS
    from pumpkin.voice import Voice

    async def go():
        sent, kept, told = [], {}, []

        async def send(text, **_):
            sent.append(text)
            return NS(id=len(sent), text=text)

        async def tell(text, **_):
            told.append(text)

        voice = Voice(lambda uid: True, clips=None, stt=None)
        star = NS(display_name="zye")
        session = NS(channel=NS(send=send, id=4, guild=NS(id=1, get_member=lambda uid: star)), vc=None,
                     clip=lambda seconds, only=None: (np.full(RATE, 1000, np.int16), [1]))
        assert "R2" in await voice.post_clip(session, 30, "TheMasked", asker=9)  # no storage: no clip at all
        voice.later = NS(add=lambda sid, entry, audio, own: kept.update({sid: entry}), index=kept)
        ask = NS(followup=NS(send=tell))

        async def nobody(session, seconds, asker):
            return None, 0, "nobody here is streaming"
        voice.screen_save = nobody
        assert (await voice.post_clip(session, 30, "TheMasked", asker=9)).startswith("can't clip: nobody")
        assert not sent and not kept  # no sound-only clip

        async def in_game(session, seconds, asker):
            return 77, 5, "valorant"
        voice.screen_save = in_game
        assert await voice.post_clip(session, 30, "TheMasked", asker=9) == ""  # said out loud: a button
        assert sent[0].startswith("🎬 clipped") and kept[77]["asker"] == 9 and session.last_clip[2] == 77
        assert not told and "valorant" in kept[77]["hidden"] and "once the game's closed" in kept[77]["hidden"]
        from pumpkin.voice import ClipInfo
        button = ClipInfo(voice)
        for clicker, expect in ((9, "valorant"), (3, "TheMasked's clip")):
            got = []

            async def answer(text, **kw):
                got.append((text, kw))
            click = NS(message=NS(id=1), user=NS(id=clicker), response=NS(send_message=answer))
            await button.info.callback(click)
            assert expect in got[0][0] and got[0][1]["ephemeral"]  # only the asker hears where it is

        async def desktop(session, seconds, asker):
            return 78, 5, ""
        voice.screen_save = desktop
        await voice.post_clip(session, 30, "TheMasked", asker=9, interaction=ask)  # /voice clip: a hidden reply
        assert "uploading now" in told[0]
    asyncio.run(go())


def test_later_keeps_audio_until_done_and_expires(tmp_path):
    from pumpkin.later import Later
    later = Later(tmp_path)
    later.add(7, {"who": "zye", "streamer": 5}, np.arange(10, dtype=np.int16), None)
    again = Later(tmp_path)  # survives a restart
    assert again.get(7)["who"] == "zye" and again.waiting(5) == [7]
    call, own = again.audio(7)
    assert list(call) == list(range(10)) and own is None
    assert again.expire(now=again.get(7)["created"] + 8 * 86400) == [7]
    assert again.get(7) is None and not list(tmp_path.glob("7-*"))


def test_r2_clips_leave_their_key_in_the_trash(tmp_path):
    from pumpkin.clips import Clip, ClipStore
    store = ClipStore(tmp_path)
    store.add(Clip(id=1, guild=1, channel=1, author=0, author_name="zye", file="", title="t", size=1,
                   posted="2026-01-01T00:00:00", remote="clips/1/a.mp4"))
    store.remove(1)  # no local file to delete, and nothing breaks over the empty name
    assert ClipStore(tmp_path).index.trash == ["clips/1/a.mp4"]


def test_gate_mic_keeps_only_what_discord_heard_and_finds_the_delay():
    from pumpkin.voice import gate_mic
    n = 6 * RATE
    mic = np.zeros(n, np.int16)
    t = np.arange(RATE // 2)
    burst = (3000 * np.sin(t / 7)).astype(np.int16)
    mic[RATE:RATE + len(burst)] = burst  # said on the mic and sent
    mic[4 * RATE:4 * RATE + len(burst)] = burst  # said while muted: Discord never got it
    own = np.zeros(n, np.int16)
    late = int(0.6 * RATE)  # Discord's copy runs 0.6 s behind
    own[RATE + late:RATE + late + len(burst)] = burst
    out = gate_mic(mic, own)
    assert np.abs(out[RATE:RATE + len(burst)]).max() > 2500  # in sync, from the PC
    assert np.abs(out[4 * RATE:4 * RATE + len(burst)]).max() == 0  # muted talk stays out
    assert np.abs(gate_mic(mic, None)).max() == 0  # not heard by Pumpkin at all (no opt-in): no mic


def test_owner_agent_note_says_how_to_start_it():
    from types import SimpleNamespace as NS
    from pumpkin.agent_commands import owner_note
    up = {"on": False}
    bot = NS(settings=NS(owner_id=1), agent=NS(connected=lambda uid: up["on"]))
    assert "nothing to pair" in owner_note(bot) and "agent.ps1" in owner_note(bot)
    up["on"] = True
    assert "connected now" in owner_note(bot)
