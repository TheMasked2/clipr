"""Speech to text for voice commands: a pool of free providers, tried in order, each resting when it says "slow down".

Only short clips go here (a few seconds after the offline spotter heard "pumpkin"), so free tiers go a long way:
Groq gives each Whisper model 2000 requests a day and Cloudflare Workers AI has its own daily free allowance.
(Azure Speech was dropped: its free tier doesn't last.)
"""

import base64
import logging
import os
import time
from dataclasses import dataclass, field

import httpx

log = logging.getLogger(__name__)

REST_S = 120  # after a 429 or an error, skip that provider this long
PROMPT = "Pumpkin, clip that."  # nudges Whisper to spell the wake word right


class Limited(Exception):
    """The provider is out of quota for now."""


@dataclass
class Provider:
    name: str
    kind: str  # groq | cloudflare
    key: str
    model: str = ""
    account: str = ""
    resting_until: float = 0.0
    used: int = field(default=0)


def load_providers(env=os.environ) -> list[Provider]:
    found: list[Provider] = []
    groq = env.get("GROQ_API_KEY", "").strip()
    if groq:
        found += [Provider("groq turbo", "groq", groq, "whisper-large-v3-turbo"),
                  Provider("groq v3", "groq", groq, "whisper-large-v3")]
    token, account = env.get("CLOUDFLARE_API_TOKEN", "").strip(), env.get("CLOUDFLARE_ACCOUNT_ID", "").strip()
    if token and account:
        found.append(Provider("cloudflare whisper", "cloudflare", token, "@cf/openai/whisper-large-v3-turbo",
                              account=account))
    return found


class Transcriber:
    def __init__(self, providers: list[Provider], client: httpx.AsyncClient | None = None):
        self.providers = providers
        self.client = client or httpx.AsyncClient(timeout=20)

    def names(self) -> list[str]:
        return [p.name for p in self.providers]

    async def transcribe(self, wav: bytes) -> str | None:
        """Text of a 16 kHz mono WAV, or None when every provider is resting or failing."""
        now = time.monotonic()
        for p in self.providers:
            if p.resting_until > now:
                continue
            try:
                text = await self._ask(p, wav)
                p.used += 1
                return text.strip()
            except Limited:
                log.info("STT %s is rate limited; resting it", p.name)
            except Exception as e:  # network, 5xx, odd JSON: try the next one
                log.warning("STT %s failed: %s", p.name, e)
            p.resting_until = now + REST_S
        return None

    async def _ask(self, p: Provider, wav: bytes) -> str:
        if p.kind == "groq":
            r = await self.client.post(
                "https://api.groq.com/openai/v1/audio/transcriptions",
                headers={"Authorization": f"Bearer {p.key}"},
                files={"file": ("clip.wav", wav, "audio/wav")},
                data={"model": p.model, "prompt": PROMPT, "response_format": "json", "temperature": "0"})
            check(r)
            return r.json().get("text", "")
        if p.kind == "cloudflare":
            r = await self.client.post(
                f"https://api.cloudflare.com/client/v4/accounts/{p.account}/ai/run/{p.model}",
                headers={"Authorization": f"Bearer {p.key}"},
                json={"audio": base64.b64encode(wav).decode(), "initial_prompt": PROMPT})
            check(r)
            return (r.json().get("result") or {}).get("text", "")
        raise ValueError(f"unknown speech provider kind {p.kind}")


def check(r: httpx.Response) -> None:
    if r.status_code in (402, 403, 429):
        raise Limited()
    r.raise_for_status()
