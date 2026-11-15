"""Cloudflare R2 for full-quality clips: put a file, delete a file, the public link to it.

R2 speaks the S3 API. Requests are signed with AWS Signature V4 by hand (a few lines) so Pumpkin needs no AWS
library. Settings in .env: R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET, R2_PUBLIC_URL (the bucket's r2.dev
address, which is what Discord plays) and R2_S3_API (the S3 endpoint, with or without the bucket on the end).
"""

import hashlib
import hmac
import os
from datetime import datetime, timezone
from urllib.parse import quote, urlsplit

import httpx

TIMEOUT_S = 120


class R2:
    def __init__(self, endpoint: str, bucket: str, key_id: str, secret: str, public_url: str):
        endpoint = endpoint.rstrip("/")
        if endpoint.endswith("/" + bucket):
            endpoint = endpoint[: -len(bucket) - 1]
        self.endpoint, self.bucket, self.key_id, self.secret = endpoint, bucket, key_id, secret
        self.public_url = public_url.rstrip("/")
        self.host = urlsplit(endpoint).netloc

    @classmethod
    def from_env(cls, env=os.environ) -> "R2 | None":
        names = ["R2_S3_API", "R2_BUCKET", "R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY", "R2_PUBLIC_URL"]
        values = [env.get(n, "").strip() for n in names]
        return cls(*values) if all(values) else None

    def url(self, key: str) -> str:
        """The public link (what goes in Discord)."""
        return f"{self.public_url}/{quote(key)}"

    def _signed(self, method: str, key: str, body: bytes, content_type: str = "") -> tuple[str, dict]:
        now = datetime.now(timezone.utc)
        stamp, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
        path = f"/{self.bucket}/{quote(key)}"
        payload = hashlib.sha256(body).hexdigest()
        headers = {"host": self.host, "x-amz-content-sha256": payload, "x-amz-date": stamp}
        if content_type:
            headers["content-type"] = content_type
        names = sorted(headers)
        canonical = "\n".join([method, path, "", *[f"{n}:{headers[n]}" for n in names], "", ";".join(names), payload])
        scope = f"{day}/auto/s3/aws4_request"
        to_sign = "\n".join(["AWS4-HMAC-SHA256", stamp, scope, hashlib.sha256(canonical.encode()).hexdigest()])
        k = f"AWS4{self.secret}".encode()
        for part in (day, "auto", "s3", "aws4_request"):
            k = hmac.new(k, part.encode(), hashlib.sha256).digest()
        signature = hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest()
        headers["authorization"] = (f"AWS4-HMAC-SHA256 Credential={self.key_id}/{scope}, "
                                    f"SignedHeaders={';'.join(names)}, Signature={signature}")
        del headers["host"]
        return self.endpoint + path, headers

    async def put(self, key: str, data: bytes, content_type: str = "video/mp4") -> str:
        """Upload and return the public link."""
        url, headers = self._signed("PUT", key, data, content_type)
        async with httpx.AsyncClient(timeout=TIMEOUT_S) as http:
            r = await http.put(url, content=data, headers=headers)
        if r.status_code >= 300:
            raise RuntimeError(f"R2 upload failed ({r.status_code}): {r.text[:200]}")
        return self.url(key)

    async def delete(self, key: str) -> None:
        url, headers = self._signed("DELETE", key, b"")
        async with httpx.AsyncClient(timeout=TIMEOUT_S) as http:
            r = await http.delete(url, headers=headers)
        if r.status_code >= 300 and r.status_code != 404:
            raise RuntimeError(f"R2 delete failed ({r.status_code}): {r.text[:200]}")
