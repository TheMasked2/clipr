//! A publishing RTMP client: the handshake, chunked messages both ways, and the command sequence that makes the
//! server accept a stream (`connect`, `releaseStream`, `FCPublish`, `createStream`, `publish`). `rtmps://`
//! runs the same over TLS.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::amf::{self, Amf};

/// Where to publish, from an `rtmp://host[:port]/app` or `rtmps://…` address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// The application, e.g. `app` (Twitch) or `live2` (YouTube); can hold slashes.
    pub app: String,
    /// The address without the key, as the server wants it in `connect`.
    pub tc_url: String,
}

impl Endpoint {
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let (tls, rest) = if let Some(r) = url.strip_prefix("rtmps://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("rtmp://") {
            (false, r)
        } else {
            bail!("the server address has to start with rtmp:// or rtmps://");
        };
        let (authority, app) = rest.split_once('/').unwrap_or((rest, ""));
        let app = app.trim_end_matches('/');
        if authority.is_empty() {
            bail!("the server address has no host name");
        }
        if app.is_empty() {
            bail!("the server address needs its application part, like rtmp://host/app");
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => (
                h,
                p.parse::<u16>()
                    .map_err(|_| anyhow!("the port in the server address isn't a number"))?,
            ),
            _ => (authority, if tls { 443 } else { 1935 }),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
        let scheme = if tls { "rtmps" } else { "rtmp" };
        Ok(Self {
            tls,
            tc_url: format!("{scheme}://{authority}/{app}"),
            host,
            port,
            app: app.to_owned(),
        })
    }
}

/// Message types.
pub const SET_CHUNK_SIZE: u8 = 1;
const ACK: u8 = 3;
const USER_CONTROL: u8 = 4;
const WINDOW_ACK_SIZE: u8 = 5;
const SET_PEER_BANDWIDTH: u8 = 6;
pub const AUDIO: u8 = 8;
pub const VIDEO: u8 = 9;
pub const DATA: u8 = 18;
pub const COMMAND: u8 = 20;

/// Chunk streams.
const CS_CONTROL: u32 = 2;
const CS_COMMAND: u32 = 3;
const CS_AUDIO: u32 = 4;
const CS_DATA: u32 = 5;
const CS_VIDEO: u32 = 6;

/// The chunk size clipr sends with (servers start at 128, which costs a header every 128 bytes).
const OUT_CHUNK: usize = 4096;
const OUR_WINDOW: u32 = 2_500_000;

/// One RTMP message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: u8,
    pub stream: u32,
    pub time: u32,
    pub body: Vec<u8>,
}

/// Writes `msg` as chunks of `chunk` bytes on chunk stream `cs` (full headers: simple and always valid).
pub fn write_chunks(cs: u32, msg: &Message, chunk: usize, out: &mut Vec<u8>) {
    let extended = msg.time >= 0xFF_FFFF;
    let stamp = if extended { 0xFF_FFFF } else { msg.time };
    out.push(cs as u8 & 0x3F);
    out.extend(&stamp.to_be_bytes()[1..]);
    out.extend(&(msg.body.len() as u32).to_be_bytes()[1..]);
    out.push(msg.kind);
    out.extend(msg.stream.to_le_bytes());
    if extended {
        out.extend(msg.time.to_be_bytes());
    }
    for (n, piece) in msg.body.chunks(chunk.max(1)).enumerate() {
        if n > 0 {
            out.push(0xC0 | (cs as u8 & 0x3F));
            if extended {
                out.extend(msg.time.to_be_bytes());
            }
        }
        out.extend(piece);
    }
}

/// What a chunk stream remembers from its last header.
#[derive(Default)]
struct ChunkStream {
    time: u32,
    delta: u32,
    len: usize,
    kind: u8,
    stream: u32,
    extended: bool,
    partial: Vec<u8>,
}

/// Turns received bytes into messages.
pub struct Reader {
    chunk: usize,
    streams: HashMap<u32, ChunkStream>,
    pub buf: Vec<u8>,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            chunk: 128,
            streams: HashMap::new(),
            buf: Vec::new(),
        }
    }
}

impl Reader {
    /// The next whole message in the buffer, if one is complete. Follows the peer's Set Chunk Size itself.
    pub fn next_message(&mut self) -> Result<Option<Message>> {
        loop {
            let Some((consumed, done)) = self.chunk_once()? else {
                return Ok(None);
            };
            self.buf.drain(..consumed);
            if let Some(m) = done {
                if m.kind == SET_CHUNK_SIZE && m.body.len() >= 4 {
                    let size = u32::from_be_bytes(m.body[..4].try_into().expect("4 bytes")) & 0x7FFF_FFFF;
                    self.chunk = (size as usize).clamp(1, 16 * 1024 * 1024);
                }
                return Ok(Some(m));
            }
        }
    }

    /// Parses one chunk if it's all there: the bytes it took and the message it completed, if any.
    fn chunk_once(&mut self) -> Result<Option<(usize, Option<Message>)>> {
        let b = &self.buf;
        let Some(&first) = b.first() else {
            return Ok(None);
        };
        let fmt = first >> 6;
        let (cs, mut at) = match first & 0x3F {
            0 if b.len() >= 2 => (64 + u32::from(b[1]), 2),
            1 if b.len() >= 3 => (64 + u32::from(b[1]) + 256 * u32::from(b[2]), 3),
            0 | 1 => return Ok(None),
            n => (u32::from(n), 1),
        };
        let header = [11, 7, 3, 0][usize::from(fmt)];
        if b.len() < at + header {
            return Ok(None);
        }
        let h = &b[at..at + header];
        let u24 = |s: &[u8]| u32::from_be_bytes([0, s[0], s[1], s[2]]);
        let st = self.streams.entry(cs).or_default();
        let new_message = st.partial.is_empty();
        let mut stamp = None;
        if fmt <= 2 {
            stamp = Some(u24(&h[0..3]));
        }
        if fmt <= 1 {
            st.len = u24(&h[3..6]) as usize;
            st.kind = h[6];
        }
        if fmt == 0 {
            st.stream = u32::from_le_bytes(h[7..11].try_into().expect("4 bytes"));
        }
        at += header;
        let extended = match stamp {
            Some(s) => s == 0xFF_FFFF,
            None => st.extended,
        };
        let mut value = stamp.unwrap_or(st.delta);
        if extended {
            if b.len() < at + 4 {
                return Ok(None);
            }
            value = u32::from_be_bytes(b[at..at + 4].try_into().expect("4 bytes"));
            at += 4;
        }
        if st.len > 64 * 1024 * 1024 {
            bail!("the server sent a message too big to be real");
        }
        let want = (st.len - st.partial.len()).min(self.chunk);
        if b.len() < at + want {
            return Ok(None);
        }
        st.extended = extended;
        if new_message {
            match fmt {
                0 => st.time = value,
                1 | 2 => {
                    st.delta = value;
                    st.time = st.time.wrapping_add(value);
                }
                _ => st.time = st.time.wrapping_add(st.delta),
            }
        }
        st.partial.extend(&b[at..at + want]);
        at += want;
        if st.partial.len() < st.len {
            return Ok(Some((at, None)));
        }
        let msg = Message {
            kind: st.kind,
            stream: st.stream,
            time: st.time,
            body: std::mem::take(&mut st.partial),
        };
        Ok(Some((at, Some(msg))))
    }
}

/// Plain TCP or TLS, with the socket underneath (its timeouts have to be set on the very handle that reads).
trait Io: Read + Write + Send {
    fn tcp(&self) -> &TcpStream;
}

impl Io for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl Io for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        &self.sock
    }
}

/// A connection that published a stream: send audio and video with [`Connection::send`].
pub struct Connection {
    io: Box<dyn Io>,
    reader: Reader,
    stream_id: u32,
    received: u64,
    acked: u64,
    window: u64,
    /// Bytes sent so far.
    pub sent: u64,
    txn: f64,
    key: String,
}

/// The server answered, but no: the key or address is wrong. Not worth retrying.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// A send that makes no progress this long means the connection is gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// A few random-looking bytes for the handshake (servers don't check them).
fn noise(n: usize) -> Vec<u8> {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
        | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

impl Connection {
    /// Connects, shakes hands and publishes `key`. A [`Refused`] error means the server said no.
    pub fn publish(endpoint: &Endpoint, key: &str) -> Result<Self> {
        let mut addrs: Vec<_> = (endpoint.host.as_str(), endpoint.port)
            .to_socket_addrs()
            .with_context(|| format!("can't find {}", endpoint.host))?
            .collect();
        // Every address in turn, IPv4 first: a network with broken IPv6 shouldn't stop the stream.
        addrs.sort_by_key(|a| a.is_ipv6());
        let mut last = None;
        let mut socket = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
                Ok(s) => {
                    socket = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let socket = match (socket, last) {
            (Some(s), _) => s,
            (None, Some(e)) => {
                return Err(e).with_context(|| format!("can't reach {}:{}", endpoint.host, endpoint.port));
            }
            (None, None) => bail!("can't find {}", endpoint.host),
        };
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(REPLY_TIMEOUT))?;
        socket.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let io: Box<dyn Io> = if endpoint.tls {
            let name = rustls::pki_types::ServerName::try_from(endpoint.host.clone())
                .map_err(|_| anyhow!("{} isn't a valid server name", endpoint.host))?;
            let tls = rustls::ClientConnection::new(tls_config(), name)?;
            Box::new(rustls::StreamOwned::new(tls, socket))
        } else {
            Box::new(socket)
        };
        let mut c = Self {
            io,
            reader: Reader::default(),
            stream_id: 0,
            received: 0,
            acked: 0,
            window: u64::from(OUR_WINDOW),
            sent: 0,
            txn: 0.0,
            key: key.to_owned(),
        };
        c.handshake().context("the RTMP handshake failed")?;
        c.start(endpoint, key)?;
        // From now on reads only pick up what's there (acks, pings) between sends.
        c.io.tcp().set_read_timeout(Some(Duration::from_millis(1)))?;
        Ok(c)
    }

    fn handshake(&mut self) -> Result<()> {
        let mut c1 = vec![0u8; 8];
        c1.extend(noise(1528));
        let mut hello = vec![3u8];
        hello.extend(&c1);
        self.io.write_all(&hello)?;
        self.io.flush()?;
        let mut s0s1 = vec![0u8; 1537];
        self.io.read_exact(&mut s0s1)?;
        if s0s1[0] != 3 {
            bail!("the server speaks RTMP version {}, not 3", s0s1[0]);
        }
        // C2 echoes S1; S2 (an echo of C1) is read and ignored.
        self.io.write_all(&s0s1[1..])?;
        self.io.flush()?;
        let mut s2 = vec![0u8; 1536];
        self.io.read_exact(&mut s2)?;
        self.sent += 1 + 1536 + 1536;
        Ok(())
    }

    fn command(&mut self, stream: u32, values: &[Amf]) -> Result<()> {
        let msg = Message {
            kind: COMMAND,
            stream,
            time: 0,
            body: amf::encode(values),
        };
        self.write(CS_COMMAND, &msg)
    }

    fn next_txn(&mut self) -> f64 {
        self.txn += 1.0;
        self.txn
    }

    fn start(&mut self, endpoint: &Endpoint, key: &str) -> Result<()> {
        self.write(
            CS_CONTROL,
            &Message {
                kind: SET_CHUNK_SIZE,
                stream: 0,
                time: 0,
                body: (OUT_CHUNK as u32).to_be_bytes().to_vec(),
            },
        )?;
        let txn = self.next_txn();
        self.command(
            0,
            &[
                Amf::str("connect"),
                Amf::Number(txn),
                Amf::Object(vec![
                    ("app".into(), Amf::str(&endpoint.app)),
                    ("type".into(), Amf::str("nonprivate")),
                    ("flashVer".into(), Amf::str("FMLE/3.0 (compatible; clipr)")),
                    ("tcUrl".into(), Amf::str(&endpoint.tc_url)),
                ]),
            ],
        )?;
        self.result(txn, "connect")?;

        let key = Amf::str(key);
        let t = self.next_txn();
        self.command(
            0,
            &[Amf::str("releaseStream"), Amf::Number(t), Amf::Null, key.clone()],
        )?;
        let t = self.next_txn();
        self.command(
            0,
            &[Amf::str("FCPublish"), Amf::Number(t), Amf::Null, key.clone()],
        )?;
        let txn = self.next_txn();
        self.command(0, &[Amf::str("createStream"), Amf::Number(txn), Amf::Null])?;
        let reply = self.result(txn, "createStream")?;
        self.stream_id = reply.get(3).and_then(Amf::as_number).unwrap_or(1.0) as u32;

        self.command(
            self.stream_id,
            &[
                Amf::str("publish"),
                Amf::Number(0.0),
                Amf::Null,
                key,
                Amf::str("live"),
            ],
        )?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let msg = match self.receive(deadline) {
                Ok(m) => m,
                // Twitch, for one, just hangs up on a wrong key.
                Err(e) if is_closed(&e) => {
                    return Err(Refused(
                        "the server closed the connection when clipr started the stream: is the stream key right?"
                            .into(),
                    )
                    .into());
                }
                Err(e) => return Err(e),
            };
            if msg.kind != COMMAND {
                continue;
            }
            let values = amf::decode(&msg.body).unwrap_or_default();
            if values.first().and_then(Amf::as_str) != Some("onStatus") {
                continue;
            }
            let info = values.get(3);
            let code = info
                .and_then(|i| i.get("code"))
                .and_then(Amf::as_str)
                .unwrap_or("");
            let level = info
                .and_then(|i| i.get("level"))
                .and_then(Amf::as_str)
                .unwrap_or("");
            if code == "NetStream.Publish.Start" {
                return Ok(());
            }
            if level == "error"
                || code.contains("BadName")
                || code.contains("Failed")
                || code.contains("Rejected")
            {
                let why = info
                    .and_then(|i| i.get("description"))
                    .and_then(Amf::as_str)
                    .unwrap_or(code);
                return Err(Refused(format!("the server refused the stream: {why}")).into());
            }
        }
    }

    /// Waits for the `_result` of transaction `txn`.
    fn result(&mut self, txn: f64, what: &str) -> Result<Vec<Amf>> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let msg = self.receive(deadline)?;
            if msg.kind != COMMAND {
                continue;
            }
            let values = amf::decode(&msg.body).unwrap_or_default();
            let name = values.first().and_then(Amf::as_str).unwrap_or("");
            if values.get(1).and_then(Amf::as_number) != Some(txn) {
                continue;
            }
            match name {
                "_result" => return Ok(values),
                "_error" => {
                    let why = values
                        .get(3)
                        .and_then(|i| i.get("description").or(i.get("code")))
                        .and_then(Amf::as_str)
                        .unwrap_or("no reason given");
                    return Err(Refused(format!("the server refused {what}: {why}")).into());
                }
                _ => {}
            }
        }
    }

    /// The next message, answering the server's control messages on the way. Blocks until `deadline`.
    fn receive(&mut self, deadline: Instant) -> Result<Message> {
        loop {
            if let Some(m) = self.reader.next_message()? {
                self.handle(&m)?;
                return Ok(m);
            }
            if Instant::now() >= deadline {
                bail!("the server didn't answer in time");
            }
            if !self.fill()? {
                continue;
            }
        }
    }

    /// Reads what has arrived into the reader. `false` when nothing came within the socket's timeout.
    fn fill(&mut self) -> Result<bool> {
        let mut tmp = [0u8; 16 * 1024];
        match self.io.read(&mut tmp) {
            Ok(0) => Err(std::io::Error::from(ErrorKind::UnexpectedEof).into()),
            Ok(n) => {
                self.reader.buf.extend(&tmp[..n]);
                self.received += n as u64;
                if self.received - self.acked >= self.window {
                    self.acked = self.received;
                    let body = (self.received as u32).to_be_bytes().to_vec();
                    self.write(
                        CS_CONTROL,
                        &Message {
                            kind: ACK,
                            stream: 0,
                            time: 0,
                            body,
                        },
                    )?;
                }
                Ok(true)
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn handle(&mut self, m: &Message) -> Result<()> {
        match m.kind {
            WINDOW_ACK_SIZE if m.body.len() >= 4 => {
                self.window = u64::from(u32::from_be_bytes(m.body[..4].try_into().expect("4 bytes"))).max(1);
            }
            SET_PEER_BANDWIDTH => {
                let body = OUR_WINDOW.to_be_bytes().to_vec();
                self.write(
                    CS_CONTROL,
                    &Message {
                        kind: WINDOW_ACK_SIZE,
                        stream: 0,
                        time: 0,
                        body,
                    },
                )?;
            }
            // A ping: answer with a pong carrying the same time.
            USER_CONTROL if m.body.len() >= 6 && m.body[..2] == [0, 6] => {
                let mut body = vec![0, 7];
                body.extend(&m.body[2..6]);
                self.write(
                    CS_CONTROL,
                    &Message {
                        kind: USER_CONTROL,
                        stream: 0,
                        time: 0,
                        body,
                    },
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Answers whatever the server sent meanwhile, without waiting. Call now and then while streaming.
    pub fn poll(&mut self) -> Result<()> {
        while self.fill()? {
            while let Some(m) = self.reader.next_message()? {
                self.handle(&m)?;
                if m.kind == COMMAND {
                    let values = amf::decode(&m.body).unwrap_or_default();
                    let info = values.get(3);
                    let level = info.and_then(|i| i.get("level")).and_then(Amf::as_str);
                    if level == Some("error") {
                        let code = info
                            .and_then(|i| i.get("code"))
                            .and_then(Amf::as_str)
                            .unwrap_or("");
                        bail!("the server stopped the stream ({code})");
                    }
                }
            }
        }
        Ok(())
    }

    fn write(&mut self, cs: u32, msg: &Message) -> Result<()> {
        let mut out = Vec::with_capacity(msg.body.len() + 32);
        write_chunks(
            cs,
            msg,
            if msg.kind == SET_CHUNK_SIZE {
                128
            } else {
                OUT_CHUNK
            },
            &mut out,
        );
        self.io.write_all(&out)?;
        self.io.flush()?;
        self.sent += out.len() as u64;
        Ok(())
    }

    /// Sends an audio or video tag body, or stream metadata, at `time` milliseconds.
    pub fn send(&mut self, kind: u8, time: u32, body: Vec<u8>) -> Result<()> {
        let cs = match kind {
            AUDIO => CS_AUDIO,
            VIDEO => CS_VIDEO,
            _ => CS_DATA,
        };
        let msg = Message {
            kind,
            stream: self.stream_id,
            time,
            body,
        };
        self.write(cs, &msg)
    }

    /// Ends the stream politely (the connection closes when this is dropped).
    pub fn close(mut self) {
        let key = Amf::str(&self.key);
        let _ = self.command(
            self.stream_id,
            &[Amf::str("FCUnpublish"), Amf::Number(0.0), Amf::Null, key],
        );
        let id = f64::from(self.stream_id);
        let _ = self.command(
            0,
            &[
                Amf::str("deleteStream"),
                Amf::Number(0.0),
                Amf::Null,
                Amf::Number(id),
            ],
        );
        let _ = self.io.tcp().shutdown(std::net::Shutdown::Both);
    }
}

fn is_closed(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let e = Endpoint::parse("rtmps://live.twitch.tv:443/app").unwrap();
        assert_eq!(
            (e.tls, e.host.as_str(), e.port, e.app.as_str()),
            (true, "live.twitch.tv", 443, "app")
        );
        assert_eq!(e.tc_url, "rtmps://live.twitch.tv:443/app");
        let e = Endpoint::parse(" rtmp://a.rtmp.youtube.com/live2/ ").unwrap();
        assert_eq!((e.tls, e.port, e.app.as_str()), (false, 1935, "live2"));
        assert_eq!(e.tc_url, "rtmp://a.rtmp.youtube.com/live2");
        let e = Endpoint::parse("rtmps://host/app/sub").unwrap();
        assert_eq!((e.port, e.app.as_str()), (443, "app/sub"));
        assert!(Endpoint::parse("http://x/app").is_err());
        assert!(Endpoint::parse("rtmp://host").is_err());
        assert!(Endpoint::parse("rtmp://host:abc/app").is_err());
    }

    #[test]
    fn chunks_round_trip() {
        let msgs = [
            Message {
                kind: VIDEO,
                stream: 1,
                time: 40,
                body: (0..10_000u32).map(|i| i as u8).collect(),
            },
            Message {
                kind: AUDIO,
                stream: 1,
                time: 0x0100_0000,
                body: vec![1, 2, 3],
            },
            Message {
                kind: COMMAND,
                stream: 0,
                time: 0,
                body: vec![],
            },
        ];
        let mut bytes = Vec::new();
        for m in &msgs {
            write_chunks(6, m, 128, &mut bytes);
        }
        let mut r = Reader::default();
        // Arrives in dribs and drabs.
        let mut got = Vec::new();
        for piece in bytes.chunks(37) {
            r.buf.extend(piece);
            while let Some(m) = r.next_message().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, msgs);
    }

    #[test]
    fn set_chunk_size_is_followed() {
        let mut bytes = Vec::new();
        write_chunks(
            2,
            &Message {
                kind: SET_CHUNK_SIZE,
                stream: 0,
                time: 0,
                body: 4096u32.to_be_bytes().to_vec(),
            },
            128,
            &mut bytes,
        );
        let big = Message {
            kind: VIDEO,
            stream: 1,
            time: 5,
            body: vec![9; 5000],
        };
        write_chunks(6, &big, 4096, &mut bytes);
        let mut r = Reader {
            buf: bytes,
            ..Default::default()
        };
        assert_eq!(r.next_message().unwrap().unwrap().kind, SET_CHUNK_SIZE);
        assert_eq!(r.next_message().unwrap().unwrap(), big);
    }
}
