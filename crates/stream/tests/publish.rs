//! The whole publish flow against a small fake RTMP server on localhost.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clipr_stream::amf::{self, Amf};
use clipr_stream::rtmp::{self, Message, Reader};
use clipr_stream::{Settings, State, Streamer};

/// How the fake server answers `publish`.
#[derive(Clone, Copy)]
enum Answer {
    Start,
    BadName,
    HangUp,
}

fn reply(sock: &mut TcpStream, stream: u32, values: &[Amf]) {
    let mut out = Vec::new();
    let msg = Message {
        kind: rtmp::COMMAND,
        stream,
        time: 0,
        body: amf::encode(values),
    };
    rtmp::write_chunks(3, &msg, 128, &mut out);
    sock.write_all(&out).unwrap();
}

/// Serves every client in turn; every message after `publish` goes to `tx`.
fn serve(listener: TcpListener, answer: Answer, tx: mpsc::Sender<Message>) {
    for sock in listener.incoming() {
        let Ok(sock) = sock else { return };
        serve_one(sock, answer, &tx);
    }
}

fn serve_one(mut sock: TcpStream, answer: Answer, tx: &mpsc::Sender<Message>) {
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut c0c1 = vec![0u8; 1537];
    sock.read_exact(&mut c0c1).unwrap();
    assert_eq!(c0c1[0], 3);
    let mut hello = vec![3u8];
    hello.extend(vec![0u8; 1536]);
    hello.extend(&c0c1[1..]);
    sock.write_all(&hello).unwrap();
    let mut c2 = vec![0u8; 1536];
    sock.read_exact(&mut c2).unwrap();

    let mut reader = Reader::default();
    let mut published = false;
    let mut tmp = [0u8; 8192];
    loop {
        while let Some(m) = reader.next_message().unwrap() {
            if published {
                if tx.send(m).is_err() {
                    return;
                }
                continue;
            }
            if m.kind != rtmp::COMMAND {
                continue;
            }
            let v = amf::decode(&m.body).unwrap();
            let name = v[0].as_str().unwrap().to_owned();
            let txn = v[1].clone();
            match name.as_str() {
                "connect" => {
                    assert_eq!(v[2].get("app").and_then(Amf::as_str), Some("app"));
                    assert_eq!(
                        v[2].get("tcUrl")
                            .and_then(Amf::as_str)
                            .map(|u| u.ends_with("/app")),
                        Some(true)
                    );
                    reply(
                        &mut sock,
                        0,
                        &[Amf::str("_result"), txn, Amf::Null, Amf::Object(vec![])],
                    );
                }
                "createStream" => reply(
                    &mut sock,
                    0,
                    &[Amf::str("_result"), txn, Amf::Null, Amf::Number(1.0)],
                ),
                "publish" => {
                    assert_eq!(v[3].as_str(), Some("secret-key"));
                    let code = match answer {
                        Answer::Start => "NetStream.Publish.Start",
                        Answer::BadName => "NetStream.Publish.BadName",
                        Answer::HangUp => return,
                    };
                    let level = if matches!(answer, Answer::Start) {
                        "status"
                    } else {
                        "error"
                    };
                    let info = Amf::Object(vec![
                        ("level".into(), Amf::str(level)),
                        ("code".into(), Amf::str(code)),
                        // Like servers that echo the stream name back: it must not show.
                        ("description".into(), Amf::str("nope: secret-key")),
                    ]);
                    reply(
                        &mut sock,
                        1,
                        &[Amf::str("onStatus"), Amf::Number(0.0), Amf::Null, info],
                    );
                    published = true;
                }
                _ => {}
            }
        }
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => reader.buf.extend(&tmp[..n]),
        }
    }
}

fn start(answer: Answer) -> (Streamer, mpsc::Receiver<Message>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve(listener, answer, tx));
    let streamer = Streamer::start(Settings {
        url: format!("rtmp://127.0.0.1:{port}/app"),
        key: "secret-key".into(),
        width: 1280,
        height: 720,
        fps: 60,
        video_kbps: 4500,
        audio_kbps: 160,
    });
    (streamer, rx)
}

fn wait_for(streamer: &Streamer, want: impl Fn(&State) -> bool) -> State {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = streamer.health().state;
        if want(&state) || Instant::now() > deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const MS: i64 = 10_000;

#[test]
fn a_stream_goes_out_in_order() {
    let (streamer, rx) = start(Answer::Start);
    // Published: the metadata comes first. Not live until a picture goes out.
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!((first.kind, first.time, first.body[0]), (rtmp::DATA, 0, 2));
    assert_eq!(streamer.health().state, State::Connecting);
    let sink = streamer.sink();
    let t0 = 5_000_000_000i64;
    // A picture before the first keyframe and audio before it don't go out.
    sink.video(t0 - 16 * MS, false, Arc::from(&[0, 0, 0, 1, 0x41, 1][..]));
    sink.audio(t0 - 5 * MS, Arc::from(&[9u8][..]));
    let mut key = vec![
        0, 0, 0, 1, 0x67, 0x64, 0, 0x28, 0xAC, 0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80,
    ];
    key.extend([0, 0, 0, 1, 0x65, 0x88, 0x84]);
    sink.video(t0, true, key.into());
    sink.audio(t0 + 10 * MS, Arc::from(&[1u8, 2][..]));
    sink.video(t0 + 16 * MS, false, Arc::from(&[0, 0, 0, 1, 0x41, 0x9A][..]));

    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 4 && Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
            got.push(m);
        }
    }
    let summary: Vec<(u8, u32, u8, u8)> = got
        .iter()
        .map(|m| {
            (
                m.kind,
                m.time,
                m.body.first().copied().unwrap_or(0),
                m.body.get(1).copied().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (rtmp::AUDIO, 0, 0xAF, 0),  // AAC sequence header
            (rtmp::VIDEO, 0, 0x17, 0),  // AVC sequence header
            (rtmp::VIDEO, 0, 0x17, 1),  // the keyframe
            (rtmp::AUDIO, 10, 0xAF, 1), // audio 10 ms in
        ]
    );
    let next = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((next.kind, next.time, next.body[0]), (rtmp::VIDEO, 16, 0x27));
    let health = streamer.health();
    assert_eq!(health.state, State::Live);
    assert!(health.since.is_some());
    assert_eq!(health.dropped, 0);
    streamer.stop();
}

#[test]
fn a_refused_key_is_not_retried() {
    let (streamer, _rx) = start(Answer::BadName);
    let state = wait_for(&streamer, |s| matches!(s, State::Failed(_)));
    assert!(
        matches!(&state, State::Failed(why) if why.contains("nope") && !why.contains("secret-key")),
        "{state:?}"
    );
    streamer.stop();
}

#[test]
fn a_hang_up_on_publish_points_at_the_key() {
    let (streamer, _rx) = start(Answer::HangUp);
    let state = wait_for(&streamer, |s| matches!(s, State::Failed(_)));
    assert!(
        matches!(&state, State::Failed(why) if why.contains("stream key")),
        "{state:?}"
    );
    streamer.stop();
}

#[test]
fn video_waiting_for_a_server_is_not_counted_as_dropped() {
    // Nothing listens there: clipr keeps trying, and the pictures it can't send yet aren't the connection's fault.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let streamer = Streamer::start(Settings {
        url: format!("rtmp://127.0.0.1:{port}/app"),
        key: "secret-key".into(),
        width: 1280,
        height: 720,
        fps: 60,
        video_kbps: 4500,
        audio_kbps: 160,
    });
    let sink = streamer.sink();
    for i in 0..300i64 {
        // Five seconds of 60 fps, a keyframe every two.
        sink.video(i * 166_667, i % 120 == 0, Arc::from(&[0, 0, 0, 1, 0x41, 1][..]));
    }
    assert!(matches!(
        wait_for(&streamer, |s| matches!(s, State::Reconnecting(_))),
        State::Reconnecting(_)
    ));
    assert_eq!(streamer.health().dropped, 0);
    streamer.stop();
}
