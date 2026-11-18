//! A small S3 client for the calls clipr needs: put a file (multipart when it's big), delete one, and read
//! one back for the connection test. Requests are signed with AWS Signature Version 4, which Cloudflare R2,
//! Amazon S3 and Backblaze B2 all take. Bodies are sent as `UNSIGNED-PAYLOAD`, so a file is streamed from
//! disk once instead of being hashed first.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Files up to this size go up in one request; bigger ones in parts of [`PART_SIZE`], so a dropped
/// connection only costs one part.
pub const SINGLE_PUT_MAX: u64 = 64 * 1024 * 1024;
/// R2 wants every part but the last to be the same size, at least 5 MiB.
pub const PART_SIZE: u64 = 16 * 1024 * 1024;
/// How often a failed part is tried again.
const PART_TRIES: u32 = 3;

/// Where clips go: one bucket on one S3-compatible service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    /// `https://<account>.r2.cloudflarestorage.com`, `https://s3.<region>.amazonaws.com`,
    /// `https://s3.<region>.backblazeb2.com`, ...
    pub endpoint: String,
    /// `auto` for R2.
    pub region: String,
    pub bucket: String,
    pub key_id: String,
    pub secret: String,
}

/// Reports how many bytes of the current file are up.
pub type Progress<'a> = &'a mut dyn FnMut(u64);

impl Bucket {
    fn agent(timeout: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .user_agent(format!("clipr/{}", clipr_core::VERSION))
            .http_status_as_error(false)
            .build()
            .into()
    }

    /// The URL of `key` (path-style: `<endpoint>/<bucket>/<key>`, which all three services take).
    fn url(&self, key: &str, query: &[(&str, &str)]) -> Result<(String, String, String)> {
        let (scheme, endpoint_host) = split_endpoint(&self.endpoint)?;
        // Amazon wants the bucket in the host name (virtual-hosted style); R2, B2 and the rest take it in
        // the path. A bucket name with dots can't be a host name under Amazon's certificate, so it stays in
        // the path there too.
        let (host, path) = if endpoint_host.ends_with(".amazonaws.com") && !self.bucket.contains('.') {
            (
                format!("{}.{endpoint_host}", self.bucket),
                format!("/{}", encode_path(key)),
            )
        } else {
            (
                endpoint_host,
                format!("/{}/{}", encode_path(&self.bucket), encode_path(key)),
            )
        };
        let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (encode(k), encode(v))).collect();
        q.sort();
        let query = q
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let url = if query.is_empty() {
            format!("{scheme}://{host}{path}")
        } else {
            format!("{scheme}://{host}{path}?{query}")
        };
        Ok((url, host.to_string(), format!("{path}\n{query}")))
    }

    /// The headers that sign a request (`Authorization`, `x-amz-date`, `x-amz-content-sha256`).
    fn sign(&self, method: &str, host: &str, path_and_query: &str, now: SystemTime) -> Vec<(String, String)> {
        let (date, time) = amz_time(now);
        let headers = [
            ("host".to_string(), host.to_string()),
            ("x-amz-content-sha256".to_string(), "UNSIGNED-PAYLOAD".to_string()),
            ("x-amz-date".to_string(), time.clone()),
        ];
        let auth = authorization(
            &self.key_id,
            &self.secret,
            &self.region,
            method,
            path_and_query,
            &headers,
            "UNSIGNED-PAYLOAD",
            &date,
            &time,
        );
        vec![
            ("Authorization".into(), auth),
            ("x-amz-date".into(), time),
            ("x-amz-content-sha256".into(), "UNSIGNED-PAYLOAD".into()),
        ]
    }

    /// Sends one request; returns the status and the body (for errors and XML answers).
    fn call(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        body: Option<(&mut dyn Read, u64)>,
        content_type: Option<&str>,
        timeout: Duration,
    ) -> Result<(u16, String, Option<String>)> {
        let (url, host, pq) = self.url(key, query)?;
        let agent = Self::agent(timeout);
        let mut req = ureq::http::Request::builder().method(method).uri(&url);
        for (k, v) in self.sign(method, &host, &pq, SystemTime::now()) {
            req = req.header(k, v);
        }
        if let Some(ct) = content_type {
            req = req.header("Content-Type", ct);
        }
        let mut response = match body {
            Some((reader, len)) => {
                let req = req
                    .header("Content-Length", len.to_string())
                    .body(ureq::SendBody::from_reader(reader))
                    .context("can't build the request")?;
                agent.run(req)
            }
            None => {
                let req = req.body(()).context("can't build the request")?;
                agent.run(req)
            }
        }
        .with_context(|| format!("can't reach {host}"))?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .unwrap_or_default();
        Ok((status, text, etag))
    }

    /// Uploads `path` as `key`. `max_bytes_per_sec` (0 = no limit) caps the speed.
    pub fn put_file(
        &self,
        key: &str,
        path: &Path,
        content_type: &str,
        max_bytes_per_sec: u64,
        progress: Progress<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let size = std::fs::metadata(path)
            .with_context(|| format!("can't read {}", path.display()))?
            .len();
        let mut throttle = Throttle::new(max_bytes_per_sec);
        if size <= SINGLE_PUT_MAX {
            let mut file = File::open(path).with_context(|| format!("can't open {}", path.display()))?;
            let mut reader = Counted::new(&mut file, &mut throttle, progress, 0, cancelled);
            let (status, text, _) = self.call(
                "PUT",
                key,
                &[],
                Some((&mut reader, size)),
                Some(content_type),
                body_timeout(size, max_bytes_per_sec),
            )?;
            if cancelled() {
                bail!("cancelled");
            }
            return check(status, &text);
        }
        self.put_multipart(key, path, size, content_type, &mut throttle, progress, cancelled)
    }

    #[allow(clippy::too_many_arguments)]
    fn put_multipart(
        &self,
        key: &str,
        path: &Path,
        size: u64,
        content_type: &str,
        throttle: &mut Throttle,
        progress: Progress<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        // An empty body, but with its length: some services refuse a POST without one.
        let mut empty: &[u8] = &[];
        let (status, text, _) = self.call(
            "POST",
            key,
            &[("uploads", "")],
            Some((&mut empty, 0)),
            Some(content_type),
            Duration::from_secs(60),
        )?;
        check(status, &text)?;
        let upload_id = xml_value(&text, "UploadId").context("the bucket didn't start the upload")?;
        let result = self
            .send_parts(key, path, size, &upload_id, throttle, progress, cancelled)
            .and_then(|etags| self.complete(key, &upload_id, &etags));
        if result.is_err() {
            // Leaves no half upload taking up space in the bucket. (If the finish did go through and only
            // its answer was lost, this does nothing.)
            let _ = self.call(
                "DELETE",
                key,
                &[("uploadId", &upload_id)],
                None,
                None,
                Duration::from_secs(30),
            );
        }
        result
    }

    /// Puts the uploaded parts together into the object.
    fn complete(&self, key: &str, upload_id: &str, etags: &[String]) -> Result<()> {
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (i, etag) in etags.iter().enumerate() {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                i + 1,
                xml_escape(etag)
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let len = xml.len() as u64;
        let mut body = xml.as_bytes();
        let (status, text, _) = self.call(
            "POST",
            key,
            &[("uploadId", upload_id)],
            Some((&mut body, len)),
            Some("application/xml"),
            Duration::from_secs(300),
        )?;
        // S3 can answer 200 with an error inside.
        if status == 200 && text.contains("<Error>") {
            bail!("the bucket refused the finished upload: {}", s3_error(&text));
        }
        check(status, &text)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_parts(
        &self,
        key: &str,
        path: &Path,
        size: u64,
        upload_id: &str,
        throttle: &mut Throttle,
        progress: Progress<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<String>> {
        let mut file = File::open(path).with_context(|| format!("can't open {}", path.display()))?;
        let parts = size.div_ceil(PART_SIZE);
        let mut etags = Vec::with_capacity(parts as usize);
        for n in 0..parts {
            let start = n * PART_SIZE;
            let len = PART_SIZE.min(size - start);
            let number = (n + 1).to_string();
            let mut tries = 0;
            let etag = loop {
                if cancelled() {
                    bail!("cancelled");
                }
                tries += 1;
                let rate = throttle.rate;
                file.seek(SeekFrom::Start(start))?;
                let mut part = (&mut file).take(len);
                let mut reader = Counted::new(&mut part, throttle, progress, start, cancelled);
                let sent = self.call(
                    "PUT",
                    key,
                    &[("partNumber", &number), ("uploadId", upload_id)],
                    Some((&mut reader, len)),
                    None,
                    body_timeout(len, rate),
                );
                match sent {
                    Ok((200, _, Some(etag))) => break etag,
                    Ok((status, text, _)) if tries >= PART_TRIES => {
                        return Err(check(status, &text).err().unwrap_or_else(|| {
                            anyhow::anyhow!("the bucket sent no ETag for part {number}")
                        }));
                    }
                    Err(e) if tries >= PART_TRIES => return Err(e),
                    _ => std::thread::sleep(Duration::from_secs(2 * u64::from(tries))),
                }
            };
            etags.push(etag);
        }
        Ok(etags)
    }

    /// Deletes `key` (fine if it's already gone).
    pub fn delete(&self, key: &str) -> Result<()> {
        let (status, text, _) = self.call("DELETE", key, &[], None, None, Duration::from_secs(30))?;
        if status == 404 {
            return Ok(());
        }
        check(status, &text)
    }

    /// Reads `key` back (for the connection test).
    pub fn get_text(&self, key: &str) -> Result<String> {
        let (status, text, _) = self.call("GET", key, &[], None, None, Duration::from_secs(30))?;
        check(status, &text)?;
        Ok(text)
    }

    /// Uploads a small text (for the connection test).
    pub fn put_text(&self, key: &str, text: &str) -> Result<()> {
        let len = text.len() as u64;
        let mut body = text.as_bytes();
        let (status, answer, _) = self.call(
            "PUT",
            key,
            &[],
            Some((&mut body, len)),
            Some("text/plain; charset=utf-8"),
            Duration::from_secs(30),
        )?;
        check(status, &answer)
    }
}

/// The scheme and host of an endpoint: `https://` and a host name, or `http://` for this PC only (a local
/// MinIO, tests).
pub fn split_endpoint(endpoint: &str) -> Result<(&'static str, String)> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let (scheme, host) = if let Some(h) = endpoint.strip_prefix("https://") {
        ("https", h)
    } else if let Some(h) = endpoint.strip_prefix("http://") {
        let name = h.rsplit_once(':').map_or(h, |(n, _)| n);
        if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
            bail!("The endpoint must start with https://");
        }
        ("http", h)
    } else {
        bail!("The endpoint must start with https://");
    };
    if host.is_empty() || host.contains(['/', '@', '?', '#', ' ']) {
        bail!("The endpoint must be https:// and a host name, like https://abc.r2.cloudflarestorage.com");
    }
    Ok((scheme, host.to_string()))
}

/// Long enough for `len` bytes at the capped speed (or a slow 1 Mbit/s without a cap), plus a minute.
fn body_timeout(len: u64, rate: u64) -> Duration {
    let rate = if rate == 0 { 125_000 } else { rate.min(125_000) };
    Duration::from_secs(60 + len / rate)
}

fn check(status: u16, text: &str) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => bail!(
            "the bucket refused the key ({status}): {}. Check the access key, the secret and that the key may write to this bucket",
            s3_error(text)
        ),
        404 => bail!(
            "the bucket wasn't found (404): {}. Check the endpoint and the bucket name",
            s3_error(text)
        ),
        _ => bail!("the bucket answered {status}: {}", s3_error(text)),
    }
}

/// `Code: Message` from an S3 error body.
fn s3_error(text: &str) -> String {
    match (xml_value(text, "Code"), xml_value(text, "Message")) {
        (Some(c), Some(m)) => format!("{c}: {m}"),
        (Some(c), None) => c,
        _ if text.trim().is_empty() => "no details".into(),
        _ => text.chars().take(200).collect(),
    }
}

/// The text of the first `<tag>` in `xml` (enough for S3's flat answers).
fn xml_value(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    Some(
        xml[start..end]
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Counts bytes for the progress bar, enforces the speed cap, and stops when cancelled.
struct Counted<'a, R: Read> {
    inner: R,
    throttle: &'a mut Throttle,
    progress: &'a mut dyn FnMut(u64),
    done: u64,
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a, R: Read> Counted<'a, R> {
    fn new(
        inner: R,
        throttle: &'a mut Throttle,
        progress: &'a mut dyn FnMut(u64),
        done: u64,
        cancelled: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            inner,
            throttle,
            progress,
            done,
            cancelled,
        }
    }
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if (self.cancelled)() {
            return Err(std::io::Error::other("cancelled"));
        }
        // Small reads keep the cap smooth.
        let max = buf.len().min(64 * 1024);
        let n = self.inner.read(&mut buf[..max])?;
        self.throttle.took(n as u64);
        self.done += n as u64;
        (self.progress)(self.done);
        Ok(n)
    }
}

/// Sleeps as needed to stay under `rate` bytes per second (0 = no limit).
struct Throttle {
    rate: u64,
    start: Instant,
    sent: u64,
}

impl Throttle {
    fn new(rate: u64) -> Self {
        Self {
            rate,
            start: Instant::now(),
            sent: 0,
        }
    }

    fn took(&mut self, n: u64) {
        if self.rate == 0 {
            return;
        }
        self.sent += n;
        let due = Duration::from_secs_f64(self.sent as f64 / self.rate as f64);
        let elapsed = self.start.elapsed();
        if due > elapsed {
            std::thread::sleep(due - elapsed);
        }
        // Start over now and then, so a pause (between parts, a retry) isn't made up for with a burst.
        if elapsed > Duration::from_secs(10) {
            self.start = Instant::now();
            self.sent = 0;
        }
    }
}

// ---- Signature Version 4 ---------------------------------------------------------------------------------

/// `YYYYMMDD` and `YYYYMMDDTHHMMSSZ` (UTC).
fn amz_time(t: SystemTime) -> (String, String) {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let date = format!("{y:04}{m:02}{d:02}");
    let time = format!("{date}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60);
    (date, time)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The `Authorization` header. `path_and_query` is the canonical path, a newline, and the canonical
/// query; `headers` are the signed ones, lowercase names, sorted.
#[allow(clippy::too_many_arguments)]
fn authorization(
    key_id: &str,
    secret: &str,
    region: &str,
    method: &str,
    path_and_query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
    date: &str,
    time: &str,
) -> String {
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical = format!("{method}\n{path_and_query}\n{canonical_headers}\n{signed}\n{payload_hash}");
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{time}\n{scope}\n{}",
        hex(&Sha256::digest(canonical))
    );
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, b"s3");
    let k = hmac(&k, b"aws4_request");
    let signature = hex(&hmac(&k, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={signed}, Signature={signature}")
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// URI-encodes everything but S3's unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// [`encode`] for each `/`-separated part of a key.
pub fn encode_path(s: &str) -> String {
    s.split('/').map(encode).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than a block.
        assert_eq!(
            hex(&hmac(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signature_matches_the_aws_example() {
        // "Example: GET Object" from the S3 docs (Signature Version 4, Authorization header).
        let headers = [
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("range".to_string(), "bytes=0-9".to_string()),
            (
                "x-amz-content-sha256".to_string(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            ),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let auth = authorization(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "GET",
            "/test.txt\n",
            &headers,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "20130524",
            "20130524T000000Z",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn dates_and_urls() {
        let t = UNIX_EPOCH + Duration::from_secs(1_369_353_600); // 2013-05-24 00:00:00 UTC
        assert_eq!(amz_time(t), ("20130524".into(), "20130524T000000Z".into()));
        let t = UNIX_EPOCH + Duration::from_secs(1_709_251_199); // 2024-02-29 23:59:59 UTC
        assert_eq!(amz_time(t).1, "20240229T235959Z");

        let b = Bucket {
            endpoint: "https://acc.r2.cloudflarestorage.com/".into(),
            region: "auto".into(),
            bucket: "clips".into(),
            key_id: "k".into(),
            secret: "s".into(),
        };
        let (url, host, pq) = b
            .url(
                "clipr/Rocket League/my clip+1.mp4",
                &[("uploadId", "a/b"), ("partNumber", "2")],
            )
            .unwrap();
        assert_eq!(host, "acc.r2.cloudflarestorage.com");
        assert_eq!(
            url,
            "https://acc.r2.cloudflarestorage.com/clips/clipr/Rocket%20League/my%20clip%2B1.mp4?partNumber=2&uploadId=a%2Fb"
        );
        assert_eq!(
            pq,
            "/clips/clipr/Rocket%20League/my%20clip%2B1.mp4\npartNumber=2&uploadId=a%2Fb"
        );
        assert_eq!(
            b.url("x", &[("uploads", "")]).unwrap().0,
            "https://acc.r2.cloudflarestorage.com/clips/x?uploads="
        );
        let aws = Bucket {
            endpoint: "https://s3.eu-west-1.amazonaws.com".into(),
            ..b.clone()
        };
        let (url, host, pq) = aws.url("a b.mp4", &[]).unwrap();
        assert_eq!(url, "https://clips.s3.eu-west-1.amazonaws.com/a%20b.mp4");
        assert_eq!(
            (host.as_str(), pq.as_str()),
            ("clips.s3.eu-west-1.amazonaws.com", "/a%20b.mp4\n")
        );
        let dotted = Bucket {
            bucket: "my.clips".into(),
            ..aws.clone()
        };
        assert_eq!(
            dotted.url("k", &[]).unwrap().0,
            "https://s3.eu-west-1.amazonaws.com/my.clips/k"
        );
        assert_eq!(
            split_endpoint("http://127.0.0.1:9000").unwrap(),
            ("http", "127.0.0.1:9000".to_string())
        );
        for bad in [
            "http://x.com",
            "http://127.0.0.1.evil.com",
            "https://",
            "https://a.com/path",
            "https://u@a.com",
        ] {
            let b = Bucket {
                endpoint: bad.into(),
                ..b.clone()
            };
            assert!(b.url("x", &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn s3_answers_read_safely() {
        let ok = "<InitiateMultipartUploadResult><Bucket>b</Bucket><UploadId>abc&amp;1</UploadId></InitiateMultipartUploadResult>";
        assert_eq!(xml_value(ok, "UploadId").as_deref(), Some("abc&1"));
        let err = "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
        assert_eq!(s3_error(err), "AccessDenied: Access Denied");
        assert!(check(403, err).unwrap_err().to_string().contains("AccessDenied"));
        assert!(check(200, "").is_ok());
        assert_eq!(s3_error(""), "no details");
    }

    #[test]
    fn the_cap_slows_reads_down() {
        let data = vec![0u8; 300_000];
        let mut throttle = Throttle::new(1_000_000);
        let mut seen = 0;
        let mut progress = |n| seen = n;
        let never = || false;
        let start = Instant::now();
        let mut r = Counted::new(&data[..], &mut throttle, &mut progress, 0, &never);
        std::io::copy(&mut r, &mut std::io::sink()).unwrap();
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(seen, 300_000);
    }
}
