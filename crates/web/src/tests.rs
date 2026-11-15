use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, header};
use clipr_core::status::{Activity, RecState};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: AppState,
    control: Arc<NoopControl>,
    _tx: watch::Sender<bool>,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("clips");
    fs::create_dir_all(&root).unwrap();
    let library = Library::open(root.clone(), tmp.path().join("data").join("library.db")).unwrap();
    library.set_recycle_bin(false);
    let control = Arc::new(NoopControl::default());
    control.config.lock().unwrap().clips.folder = root.to_string_lossy().into_owned();
    let (tx, rx) = watch::channel(false);
    let state = AppState {
        status: Arc::new(Status::new()),
        shutdown: rx,
        control: control.clone(),
        bus: Bus::new(),
        thumbs_dir: library.thumbs_dir().to_path_buf(),
        library: Arc::new(library),
    };
    Env {
        _tmp: tmp,
        root,
        state,
        control,
        _tx: tx,
    }
}

fn put(env: &Env, rel: &str, bytes: usize) {
    let path = env.root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; bytes]).unwrap();
}

struct Call<'a> {
    method: &'a str,
    uri: &'a str,
    body: Option<&'a str>,
    peer: Option<&'a str>,
    headers: Vec<(&'a str, &'a str)>,
}

fn call<'a>(method: &'a str, uri: &'a str) -> Call<'a> {
    Call {
        method,
        uri,
        body: None,
        peer: None,
        headers: Vec::new(),
    }
}

impl<'a> Call<'a> {
    fn body(mut self, b: &'a str) -> Self {
        self.body = Some(b);
        self
    }
    fn peer(mut self, p: &'a str) -> Self {
        self.peer = Some(p);
        self
    }
    fn header(mut self, k: &'a str, v: &'a str) -> Self {
        self.headers.push((k, v));
        self
    }
    async fn send(self, app: &Router) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().method(self.method).uri(self.uri);
        for (k, v) in self.headers {
            req = req.header(k, v);
        }
        if let Some(peer) = self.peer {
            let addr: SocketAddr = format!("{peer}:50000").parse().unwrap();
            req = req.extension(ConnectInfo(addr));
        }
        let body = self.body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let (code, headers) = (response.status(), response.headers().clone());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (code, headers, String::from_utf8_lossy(&bytes).into_owned())
    }
}

async fn get_body(app: Router, uri: &str) -> (StatusCode, String) {
    let (code, _, body) = call("GET", uri).send(&app).await;
    (code, body)
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON ({e}): {body:?}"))
}

#[tokio::test]
async fn status_endpoint_reports_activity() {
    let env = env();
    env.state.status.set(Activity {
        state: RecState::Buffering,
        game: Some("Minecraft".into()),
        ..Activity::default()
    });
    let (code, body) = get_body(router(env.state.clone()), "/api/status").await;
    assert_eq!(code, StatusCode::OK);
    let json = json(&body);
    assert_eq!(json["state"], "buffering");
    assert_eq!(json["game"], "Minecraft");
}

#[tokio::test]
async fn unknown_api_path_is_404_not_the_ui() {
    let env = env();
    let (code, body) = get_body(router(env.state.clone()), "/api/nope").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(json(&body)["error"].is_string());
}

#[tokio::test]
async fn client_routes_fall_back_to_html() {
    let env = env();
    let (code, body) = get_body(router(env.state.clone()), "/gallery/some/clip").await;
    assert_eq!(code, StatusCode::OK);
    assert!(body.contains("<"), "expected HTML, got {body:?}");
}

#[tokio::test]
async fn bind_falls_back_to_next_port() {
    // Port 0 means "any free port"; keep it occupied and ask for it again.
    let (first, taken) = bind(0).await.unwrap();
    assert_ne!(taken, 0);
    let (_second, got) = bind(taken).await.unwrap();
    assert_ne!(got, taken);
    drop(first);
}

#[tokio::test]
async fn list_filter_games_and_shelves() {
    let env = env();
    put(&env, "Valorant/a.mp4", 10);
    put(&env, "Valorant/b_recording.mp4", 20);
    put(&env, "Minecraft/c.mp4", 30);
    let app = router(env.state.clone());

    let (code, body) = get_body(app.clone(), "/api/clips").await;
    assert_eq!(code, StatusCode::OK);
    let clips = json(&body);
    assert_eq!(clips.as_array().unwrap().len(), 3);
    let a = clips
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "Valorant/a.mp4")
        .unwrap();
    assert_eq!(a["url"], "/media/Valorant/a.mp4");
    assert_eq!(a["thumb"], serde_json::Value::Null);
    assert_eq!(a["sprite_frames"], 10);

    let (_, body) = get_body(app.clone(), "/api/clips?game=Minecraft&q=&shelf=").await;
    assert_eq!(json(&body).as_array().unwrap().len(), 1);
    let (_, body) = get_body(app.clone(), "/api/clips?kind=recording").await;
    assert_eq!(json(&body)[0]["id"], "Valorant/b_recording.mp4");
    let (code, _) = get_body(app.clone(), "/api/clips?kind=nope").await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    let (_, _, body) = call("PATCH", "/api/clips")
        .body(r#"{"id":"Minecraft/c.mp4","favorite":true,"shelves":["funny"]}"#)
        .send(&app)
        .await;
    assert_eq!(json(&body)["favorite"], true);
    let (_, body) = get_body(app.clone(), "/api/clips?favorite=true&shelf=funny").await;
    assert_eq!(json(&body).as_array().unwrap().len(), 1);
    let (_, body) = get_body(app.clone(), "/api/shelves").await;
    assert_eq!(json(&body), serde_json::json!(["funny"]));
    let (_, body) = get_body(app.clone(), "/api/games").await;
    let games = json(&body);
    assert_eq!(games[1]["name"], "Valorant");
    assert_eq!(games[1]["clips"], 2);
    assert_eq!(games[1]["size_bytes"], 30);
    let (code, body) = get_body(app, "/api/disk").await;
    assert_eq!(code, StatusCode::OK);
    assert!(json(&body)["total_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn patch_renames_and_delete_removes() {
    let env = env();
    put(&env, "G/old.mp4", 10);
    let app = router(env.state.clone());
    get_body(app.clone(), "/api/clips").await;

    let (code, _, body) = call("PATCH", "/api/clips")
        .body(r#"{"id":"G/old.mp4","title":"Ace round"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let clip = json(&body);
    assert_eq!(clip["id"], "G/Ace round.mp4");
    assert_eq!(clip["title"], "Ace round");
    assert_eq!(clip["url"], "/media/G/Ace%20round.mp4");
    assert!(env.root.join("G/Ace round.mp4").exists());
    assert!(!env.root.join("G/old.mp4").exists());

    let (code, _, body) = call("PATCH", "/api/clips")
        .body(r#"{"id":"G/Ace round.mp4","title":"bad/name"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(json(&body)["error"].is_string());
    let (code, _, _) = call("PATCH", "/api/clips")
        .body(r#"{"id":"G/nope.mp4","favorite":true}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _, _) = call("PATCH", "/api/clips").body("not json").send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    let (code, _, _) = call("DELETE", "/api/clips?id=G/Ace%20round.mp4").send(&app).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert!(!env.root.join("G/Ace round.mp4").exists());
    let (code, _, _) = call("DELETE", "/api/clips?id=G/Ace%20round.mp4").send(&app).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (_, body) = get_body(app, "/api/clips").await;
    assert!(json(&body).as_array().unwrap().is_empty());
}

#[tokio::test]
async fn media_supports_range_requests() {
    let env = env();
    put(&env, "G/a b.mp4", 1000);
    let app = router(env.state.clone());

    let (code, headers, body) = call("GET", "/media/G/a%20b.mp4")
        .header("range", "bytes=0-9")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 10);
    assert_eq!(headers[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-9/1000");
    let (code, _, body) = call("GET", "/media/G/a%20b.mp4").send(&app).await;
    assert_eq!((code, body.len()), (StatusCode::OK, 1000));
    let (code, _) = get_body(app, "/media/G/missing.mp4").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn paths_that_escape_the_clips_folder_are_refused() {
    let env = env();
    fs::write(env.root.parent().unwrap().join("secret.mp4"), "secret").unwrap();
    let app = router(env.state.clone());
    for uri in [
        "/media/..%2Fsecret.mp4",
        "/media/G/..%2F..%2Fsecret.mp4",
        "/media/%2E%2E/secret.mp4",
        "/media/C%3A/secret.mp4",
        "/media/G%5C..%5C..%5Csecret.mp4",
        "/thumbs/..%2Fsecret.mp4.jpg",
    ] {
        let (code, _, body) = call("GET", uri).send(&app).await;
        assert!(
            matches!(code, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND),
            "{uri} gave {code}"
        );
        assert!(
            !body.contains("secret") || body.contains("error"),
            "{uri} leaked {body}"
        );
    }
    let (code, _, _) = call("DELETE", "/api/clips?id=../secret.mp4").send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, _, _) = call("POST", "/api/clips/reveal")
        .body(r#"{"id":"../secret.mp4"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(env.root.parent().unwrap().join("secret.mp4").exists());
}

#[tokio::test]
async fn thumbnails_are_served_once_they_exist() {
    let env = env();
    put(&env, "G/a.mp4", 10);
    let app = router(env.state.clone());
    let (code, _) = get_body(app.clone(), "/thumbs/G/a.mp4.jpg").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    fs::write(
        env.state
            .thumbs_dir
            .join(clipr_library::thumb_file_name("G/a.mp4", false)),
        b"jpegdata",
    )
    .unwrap();
    let (code, headers, body) = call("GET", "/thumbs/G/a.mp4.jpg").send(&app).await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, "jpegdata"));
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
}

#[tokio::test]
async fn config_round_trip_and_validation() {
    let env = env();
    let app = router(env.state.clone());
    let (_, _, body) = call("GET", "/api/config").send(&app).await;
    let mut config = json(&body);
    assert_eq!(config["video"]["fps"], 60);

    config["video"]["fps"] = 144.into();
    let (code, _, body) = call("PUT", "/api/config")
        .body(&config.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["video"]["fps"], 144);
    assert_eq!(env.control.config().video.fps, 144);

    config["video"]["fps"] = 5.into();
    let (code, _, body) = call("PUT", "/api/config")
        .body(&config.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(json(&body)["error"].as_str().unwrap().contains("Frame rate"));
    assert_eq!(
        env.control.config().video.fps,
        144,
        "a rejected config must not be applied"
    );

    config["video"]["fps"] = 60.into();
    config["hotkeys"]["mark"] = "Alt+F9".into(); // same as toggle_recording
    let (code, _, body) = call("PUT", "/api/config")
        .body(&config.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(json(&body)["error"].as_str().unwrap().contains("both use"));

    config["hotkeys"]["mark"] = "".into();
    config["web"]["lan"] = true.into();
    let (code, _, body) = call("PUT", "/api/config")
        .body(&config.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let pin = json(&body)["web"]["lan_pin"].as_str().unwrap().to_string();
    assert_eq!(pin.len(), 6);
    assert!(pin.bytes().all(|b| b.is_ascii_digit()));
}

#[tokio::test]
async fn actions_reach_control() {
    let env = env();
    let app = router(env.state.clone());
    let (code, _, _) = call("POST", "/api/clip").send(&app).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let (code, _, _) = call("POST", "/api/clip")
        .body(r#"{"seconds":45}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let (code, _, _) = call("POST", "/api/clip")
        .body(r#"{"seconds":0}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    for uri in ["/api/buffer", "/api/recording"] {
        let (code, _, _) = call("POST", uri).body(r#"{"on":true}"#).send(&app).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
    }
    let (code, _, _) = call("POST", "/api/mark").send(&app).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _, body) = call("POST", "/api/cleanup").send(&app).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json(&body)["removed"], 0);
    let (code, _, body) = call("POST", "/api/clips/trim")
        .body(r#"{"id":"G/a.mp4","start":5,"end":2}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn lan_peers_need_the_pin() {
    let env = env();
    env.control.config.lock().unwrap().web.lan_pin = "123456".into();
    let app = router(env.state.clone());
    let lan = "192.168.1.50";

    // Loopback never needs it (with or without the peer address recorded).
    let (code, _, _) = call("GET", "/api/status").peer("127.0.0.1").send(&app).await;
    assert_eq!(code, StatusCode::OK);
    let (code, _, _) = call("GET", "/api/status").send(&app).await;
    assert_eq!(code, StatusCode::OK);

    // The UI and the login call stay reachable, everything else is locked.
    let (code, _, _) = call("GET", "/").peer(lan).send(&app).await;
    assert_eq!(code, StatusCode::OK);
    for uri in [
        "/api/status",
        "/api/clips",
        "/media/G/a.mp4",
        "/thumbs/G/a.mp4.jpg",
        "/api/nope",
    ] {
        let (code, _, body) = call("GET", uri).peer(lan).send(&app).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(json(&body)["error"].is_string());
    }
    let (code, _, _) = call("GET", "/api/status")
        .peer(lan)
        .header("cookie", "clipr_session=forged")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    let (code, _, _) = call("POST", "/api/login")
        .peer(lan)
        .body(r#"{"pin":"000000"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    let (code, headers, _) = call("POST", "/api/login")
        .peer(lan)
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(
        set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"),
        "{set_cookie}"
    );
    assert!(set_cookie.contains("Max-Age=2592000"));
    let token = set_cookie.split(';').next().unwrap().to_string();
    assert!(token.starts_with("clipr_session=") && token.len() == "clipr_session=".len() + 64);

    let (code, _, _) = call("GET", "/api/status")
        .peer(lan)
        .header("cookie", &token)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
    // Another address with the same cookie is fine too: the cookie is the credential.
    let (code, _, _) = call("GET", "/api/clips")
        .peer("10.0.0.7")
        .header("cookie", &token)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn wrong_pins_are_rate_limited_per_address() {
    let env = env();
    env.control.config.lock().unwrap().web.lan_pin = "123456".into();
    let app = router(env.state.clone());
    for _ in 0..5 {
        let (code, _, _) = call("POST", "/api/login")
            .peer("10.1.1.1")
            .body(r#"{"pin":"1"}"#)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }
    // Locked out, even with the right PIN.
    let (code, headers, _) = call("POST", "/api/login")
        .peer("10.1.1.1")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    // Someone else is not affected.
    let (code, _, _) = call("POST", "/api/login")
        .peer("10.1.1.2")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn a_lan_without_a_pin_lets_nobody_in() {
    let env = env();
    let app = router(env.state.clone());
    let (code, _, _) = call("POST", "/api/login")
        .peer("10.1.1.1")
        .body(r#"{"pin":""}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn serve_runs_end_to_end_over_a_socket() {
    let env = env();
    put(&env, "G/a.mp4", 10);
    let (listener, port) = bind(0).await.unwrap();
    let state = env.state.clone();
    let server = tokio::spawn(serve(listener, state));
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET /api/clips HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("G/a.mp4"), "{out}");
    env._tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
