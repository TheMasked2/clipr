import asyncio
from types import SimpleNamespace as NS

from aiohttp import CookieJar
from aiohttp.test_utils import TestClient, TestServer

from pumpkin.clips import Clip
from pumpkin.gallery import Gallery, Shelf, entry_for, gallery_url

OWNER, KIM, SAM, STRANGER = 1, 3, 5, 9


def clip(**kw):
    base = dict(id=100, guild=7, channel=8, author=KIM, author_name="Kim", file="", title="clip of Sam's screen",
                size=0, posted="2026-11-14T20:00:00+00:00", remote="clips/x.mp4", streamer=SAM,
                streamer_name="Sam", names="Kim, Sam")
    base.update(kw)
    return Clip(**base)


def make(tmp_path):
    shelf = Shelf(tmp_path / "saved")
    members = {7: {OWNER, KIM, SAM}}
    guild = lambda gid: NS(name="Catlovers", get_member=lambda uid: uid in members.get(gid, set()) or None) \
        if gid in members else None
    bot = NS(get_guild=guild, settings=NS(is_owner=lambda uid: uid == OWNER))
    return shelf, Gallery(bot, shelf, 0)


def test_keys_are_per_person_and_stay(tmp_path):
    shelf, _ = make(tmp_path)
    key = shelf.key_for(KIM)
    assert shelf.key_for(KIM) == key and shelf.key_for(SAM) != key
    assert Shelf(tmp_path / "saved").user_for(key) == KIM  # kept on disk
    assert shelf.user_for("nope") is None and shelf.user_for(None) is None


def test_gallery_shows_members_and_only_clipper_streamer_or_owner_edit(tmp_path):
    shelf, gallery = make(tmp_path)
    saved = asyncio.run(shelf.add(entry_for(clip(), "aim", KIM), b"not really a video"))
    assert saved.title == "clip of Sam's screen" and saved.category == "aim" and shelf.video(saved.id).exists()

    async def go():
        async with TestClient(TestServer(gallery.app()), cookie_jar=CookieJar(unsafe=True)) as http:
            assert (await http.get("/api/clips")).status == 401  # no key: nothing
            assert "your own link" in await (await http.get("/")).text()
            r = await http.get(f"/g?k={shelf.key_for(STRANGER)}", allow_redirects=False)
            assert r.status == 302
            assert (await (await http.get("/api/clips")).json())["clips"] == []  # not in that server
            assert (await http.get(f"/v/{saved.id}.mp4")).status == 404

            for uid, edit in ((KIM, True), (SAM, True), (OWNER, True)):
                await http.get(f'/g?k={shelf.key_for(uid)}', allow_redirects=False)
                listed = (await (await http.get("/api/clips")).json())["clips"]
                assert listed[0]["id"] == saved.id and listed[0]["can_edit"] is edit
                assert (await (await http.get(f"/v/{saved.id}.mp4")).read()) == b"not really a video"

            await http.get(f'/g?k={shelf.key_for(KIM)}', allow_redirects=False)
            r = await http.post(f"/api/clips/{saved.id}", json={"title": "  the  1v5  ", "category": "bs"})
            assert (await r.json())["title"] == "the 1v5" and shelf.clips[saved.id].category == "bs"
            assert (await http.post(f"/api/clips/{saved.id}", json={"category": "nope"})).status == 400
            assert (await http.post(f"/api/clips/{saved.id}", json={"title": " "})).status == 400

            members_only = shelf.key_for(10)
            gallery.bot.get_guild(7)  # 10 isn't a member, so it gets nothing either
            await http.get(f'/g?k={members_only}', allow_redirects=False)
            assert (await http.delete(f"/api/clips/{saved.id}")).status == 404

            await http.get(f'/g?k={shelf.key_for(OWNER)}', allow_redirects=False)
            assert (await http.delete(f"/api/clips/{saved.id}")).status == 200
            assert saved.id not in shelf.clips and not shelf.video(saved.id).exists()
    asyncio.run(go())


def test_someone_in_the_server_who_didnt_clip_it_can_watch_but_not_edit(tmp_path):
    shelf, gallery = make(tmp_path)
    saved = asyncio.run(shelf.add(entry_for(clip(author=SAM, author_name="Sam"), "funny", SAM), b"v"))

    async def go():
        async with TestClient(TestServer(gallery.app()), cookie_jar=CookieJar(unsafe=True)) as http:
            await http.get(f'/g?k={shelf.key_for(KIM)}', allow_redirects=False)
            listed = (await (await http.get("/api/clips")).json())["clips"]
            assert listed[0]["can_edit"] is False
            assert (await http.post(f"/api/clips/{saved.id}", json={"title": "mine now"})).status == 403
    asyncio.run(go())


def test_gallery_url_comes_from_the_server_address():
    assert gallery_url({"PUMPKIN_PUBLIC_SSH": "creator@1.2.3.4"}) == "http://1.2.3.4:8080"
    assert gallery_url({"PUMPKIN_PUBLIC_SSH": "creator@1.2.3.4", "GALLERY_PORT": "9000"}) == "http://1.2.3.4:9000"
    assert gallery_url({"GALLERY_URL": "https://clips.example/"}) == "https://clips.example"
