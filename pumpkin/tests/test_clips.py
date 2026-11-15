import asyncio
from datetime import datetime, timezone
from types import SimpleNamespace

from pumpkin.ai import Brain
from pumpkin.bot import Pumpkin
from pumpkin.clips import Clip, ClipStore, fallback_title, is_clip
from pumpkin.llm import ProviderError, Router
from pumpkin.settings import Settings


def clip(i, author=1, posted="2026-11-03T12:00:00", keeper=False, guild=7):
    return Clip(id=i, guild=guild, channel=5, author=author, author_name=f"P{author}", file=f"7/{i}.mp4",
                title=f"clip {i}", size=10, posted=posted, keeper=keeper, url=f"https://x/{i}")


def test_which_files_count_as_clips():
    assert is_clip("gg.mp4", None) and is_clip("x", "video/webm") and is_clip("lol.GIF", None)
    assert not is_clip("meme.png", "image/png")


def test_fallback_titles():
    assert fallback_title("Sam", "insane_clutch-1v4.mp4") == "insane clutch 1v4"
    assert fallback_title("Sam", "20261103_120102.mp4") == "a Sam moment"


def test_store_toggles_lists_and_stars(tmp_path):
    store = ClipStore(tmp_path / "clips")
    assert store.toggle(5) and store.archiving(5)
    store.add(clip(1, posted="2026-11-01T10:00:00"))
    store.add(clip(2, author=2, posted="2026-11-02T10:00:00"))
    store.add(clip(3, guild=8))
    assert [c.id for c in store.latest(7)] == [2, 1]
    assert [c.id for c in store.latest(7, author=1)] == [1]
    assert store.star(1) and not store.star(99)
    again = ClipStore(tmp_path / "clips")  # saved
    assert [c.id for c in again.latest(7, keepers=True)] == [1] and again.archiving(5)
    assert not again.toggle(5) and not ClipStore(tmp_path / "clips").archiving(5)
    assert store.total_bytes(7) == 20


class Titler:
    def __init__(self, answer='"the 1v4 that never happened"', error=None):
        self.answer, self.error = answer, error

    async def run(self, model, turn):
        if self.error:
            raise self.error
        return self.answer


def make_brain(tmp_path, titler):
    settings = Settings(tmp_path / "s.json", 1)
    settings.values.update(ai_light="fake:a", ai_strong="fake:a")
    return Brain(Router({"fake": titler}, clock=lambda: 0.0), settings, clock=lambda: 0.0)


def test_brain_titles_clips(tmp_path):
    assert asyncio.run(make_brain(tmp_path, Titler()).title_clip("Sam", "a.mp4", "1v4")) == "the 1v4 that never happened"
    broken = make_brain(tmp_path, Titler(error=ProviderError(429)))
    assert asyncio.run(broken.title_clip("Sam", "clutch.mp4", "")) == "clutch"


class Attachment:
    def __init__(self, filename, content_type="video/mp4", size=1000):
        self.filename, self.content_type, self.size = filename, content_type, size

    async def save(self, path):
        path.write_bytes(b"video")


def test_archive_saves_titles_and_reacts(tmp_path):
    store = ClipStore(tmp_path / "clips")
    reactions = []

    async def react(emoji):
        reactions.append(emoji)

    message = SimpleNamespace(
        id=42, guild=SimpleNamespace(id=7), channel=SimpleNamespace(id=5), content="1v4 baby",
        author=SimpleNamespace(id=1, display_name="Sam"), created_at=datetime(2026, 11, 3, 12, tzinfo=timezone.utc),
        jump_url="https://discord.com/channels/7/5/42", add_reaction=react,
        attachments=[Attachment("clutch.mp4"), Attachment("meme.png", "image/png")],
    )
    async def badge(*args):
        pass

    bot = SimpleNamespace(clips=store, brain=make_brain(tmp_path, Titler()), badge=badge)
    asyncio.run(Pumpkin.archive(bot, message))
    [saved] = store.latest(7)
    assert saved.title == "the 1v4 that never happened" and saved.author_name == "Sam"
    assert (store.root / saved.file).read_bytes() == b"video" and saved.file.startswith("7/2026-11-03-42-")
    assert reactions == ["📼"]


def test_old_clips_are_cleaned_but_keepers_stay(tmp_path):
    store = ClipStore(tmp_path / "clips")
    for i, posted, keeper in [(1, "2026-09-01T10:00:00+00:00", False), (2, "2026-09-01T10:00:00+00:00", True),
                              (3, "2026-11-02T10:00:00+00:00", False)]:
        c = clip(i, posted=posted, keeper=keeper)
        (store.root / "7").mkdir(parents=True, exist_ok=True)
        (store.root / c.file).write_bytes(b"x")
        store.add(c)
    now = datetime(2026, 11, 3, 12, tzinfo=timezone.utc)
    assert store.clean(now, 0) == []
    assert [c.id for c in store.clean(now, 30)] == [1]
    assert not (store.root / "7/1.mp4").exists() and (store.root / "7/2.mp4").exists()
    assert sorted(c.id for c in ClipStore(tmp_path / "clips").latest(7)) == [2, 3]


def test_search_stats_and_week(tmp_path):
    from pumpkin.clips import Clip, ClipStore, pick_of_the_week
    store = ClipStore(tmp_path / "clips")

    def clip(i, who, title, posted, keeper=False):
        return Clip(id=i, guild=7, channel=5, author=who, author_name=["", "Sam", "Mira"][who], file="f", title=title,
                    size=1, posted=posted, keeper=keeper)

    for c in (clip(1, 1, "fell off the map again", "2026-11-01T20:00:00"),
              clip(2, 2, "1v4 clutch", "2026-11-02T20:00:00", True),
              clip(3, 1, "map clutch of the century", "2026-10-01T20:00:00", True)):
        store.add(c)
    assert [c.id for c in store.search(7, "map clutch")] == [3, 2, 1] and store.search(8, "map") == []
    assert [c.id for c in store.search(7, "mira")] == [2]
    assert store.stats(7) == [("Sam", 2, 1), ("Mira", 1, 1)]
    now = datetime(2026, 11, 3, 19, 0)
    week = store.this_week(5, now)
    assert {c.id for c in week} == {1, 2}
    assert pick_of_the_week(week, {1: 3, 2: 3}).id == 2 and pick_of_the_week(week, {}) is None
    assert not store.weekly_done(5, now)
    store.mark_weekly(5, now)
    assert store.weekly_done(5, now) and not store.weekly_done(5, datetime(2026, 11, 10, 19, 0))
