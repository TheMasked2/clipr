use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, header};
use clipr_core::config::DiscordConfig;
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
        discord: clipr_discord::Poster::start(
            tmp.path().join("discord.json"),
            tmp.path().join("discord-tmp"),
            DiscordConfig::default,
            || false,
            |_| Ok(None),
        ),
        cloud: clipr_cloud::Uploader::start(
            tmp.path().join("cloud.json"),
            clipr_core::config::CloudConfig::default,
            || false,
        ),
        library: Arc::new(library),
        lan: crate::Lan::new(0, false),
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
        let has_type = self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
        for (k, v) in self.headers {
            req = req.header(k, v);
        }
        if self.body.is_some() && !has_type {
            req = req.header("content-type", "application/json");
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
    assert!(a["url"].as_str().unwrap().starts_with("/media/Valorant/a.mp4?v="));
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
    assert!(
        clip["url"]
            .as_str()
            .unwrap()
            .starts_with("/media/G/Ace%20round.mp4?v=")
    );
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
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
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

    // The live preview shows the whole screen: logged-in devices only get it when the PC allows that.
    let (code, _, _) = call("GET", "/api/preview").send(&app).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let preview = || call("GET", "/api/preview").peer(lan).header("cookie", &token);
    let (code, _, body) = preview().send(&app).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .contains("Settings → Network")
    );
    env.control.config.lock().unwrap().web.lan_preview = true;
    let (code, _, _) = preview().send(&app).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn wrong_pins_are_rate_limited_per_address() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
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

const HOME: &str = "127.0.0.1:7845";

#[tokio::test]
async fn cross_origin_writes_are_refused() {
    let env = env();
    let app = router(env.state.clone());
    for (method, uri, body) in [
        ("POST", "/api/mark", ""),
        ("POST", "/api/buffer", r#"{"on":true}"#),
        ("PUT", "/api/config", "{}"),
        ("DELETE", "/api/clips?id=G/a.mp4", ""),
    ] {
        let (code, _, body_out) = call(method, uri)
            .peer("127.0.0.1")
            .header("host", HOME)
            .header("origin", "http://evil.example")
            .body(body)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{method} {uri}: {body_out}");
        let (code, _, _) = call(method, uri)
            .peer("127.0.0.1")
            .header("host", HOME)
            .header("origin", "null")
            .body(body)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{method} {uri} with a null origin");
    }
    // Another port on the same host is another origin too.
    let (code, _, _) = call("POST", "/api/mark")
        .peer("127.0.0.1")
        .header("host", HOME)
        .header("origin", "http://127.0.0.1:9999")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    // The WebSocket upgrade is a GET but needs the same check.
    let (code, _, _) = call("GET", "/api/events")
        .peer("127.0.0.1")
        .header("host", HOME)
        .header("origin", "http://evil.example")
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    // Plain reads without an Origin are fine.
    let (code, _, _) = call("GET", "/api/status")
        .peer("127.0.0.1")
        .header("host", HOME)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn same_origin_writes_from_the_ui_work() {
    let env = env();
    let app = router(env.state.clone());
    for host in [HOME, "localhost:7845", "[::1]:7845"] {
        let origin = format!("http://{host}");
        let (code, _, body) = call("POST", "/api/buffer")
            .peer("127.0.0.1")
            .header("host", host)
            .header("origin", &origin)
            .header("content-type", "application/json")
            .body(r#"{"on":true}"#)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{host}: {body}");
        // A bodiless POST, as the UI sends for /api/mark.
        let (code, _, _) = call("POST", "/api/mark")
            .peer("127.0.0.1")
            .header("host", host)
            .header("origin", &origin)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{host}");
    }
}

#[tokio::test]
async fn unknown_hosts_are_refused_for_local_peers() {
    let env = env();
    let app = router(env.state.clone());
    for host in [
        "evil.example",
        "evil.example:7845",
        "127.0.0.1.evil.example",
        "localhost.evil.example:1",
    ] {
        for uri in ["/api/status", "/"] {
            let (code, _, _) = call("GET", uri)
                .peer("127.0.0.1")
                .header("host", host)
                .send(&app)
                .await;
            assert_eq!(code, StatusCode::FORBIDDEN, "{host} {uri}");
        }
    }
    // A LAN peer uses the PC's address; it still has the PIN to get through.
    let (code, _, _) = call("GET", "/api/status")
        .peer("192.168.1.50")
        .header("host", "192.168.1.5:7845")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn json_bodies_need_the_json_content_type() {
    let env = env();
    let app = router(env.state.clone());
    for ct in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "multipart/form-data",
    ] {
        let (code, _, _) = call("POST", "/api/buffer")
            .peer("127.0.0.1")
            .header("host", HOME)
            .header("content-type", ct)
            .body(r#"{"on":true}"#)
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
    }
    let (code, _, _) = call("POST", "/api/buffer")
        .peer("127.0.0.1")
        .header("host", HOME)
        .header("content-type", "application/json; charset=utf-8")
        .body(r#"{"on":true}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn lan_peers_cannot_change_the_folder_or_web_settings() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());
    let lan = "192.168.1.50";
    let (_, headers, _) = call("POST", "/api/login")
        .peer(lan)
        .header("host", "192.168.1.5:7845")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let put = |config: serde_json::Value| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            call("PUT", "/api/config")
                .peer(lan)
                .header("host", "192.168.1.5:7845")
                .header("cookie", &cookie)
                .body(&config.to_string())
                .send(&app)
                .await
        }
    };
    let mut config = serde_json::to_value(env.control.config()).unwrap();
    config["video"]["fps"] = 90.into();
    let (code, _, body) = put(config.clone()).await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let mut bad = config.clone();
    bad["clips"]["folder"] = r"C:\Users\Public\x".into();
    let (code, _, body) = put(bad).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{body}");
    let mut bad = config.clone();
    bad["web"]["lan_pin"] = "000000".into();
    assert_eq!(put(bad).await.0, StatusCode::FORBIDDEN);
    let mut bad = config.clone();
    bad["web"]["port"] = 9999.into();
    assert_eq!(put(bad).await.0, StatusCode::FORBIDDEN);
    assert_eq!(env.control.config().web.lan_pin, "123456");

    // The PC itself may.
    let mut ok = config;
    ok["clips"]["folder"] = env.root.to_string_lossy().into_owned().into();
    ok["web"]["port"] = 9999.into();
    let (code, _, body) = call("PUT", "/api/config")
        .peer("127.0.0.1")
        .body(&ok.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn invalid_clips_folder_is_rejected_over_the_api() {
    let env = env();
    let app = router(env.state.clone());
    let mut config = serde_json::to_value(env.control.config()).unwrap();
    for folder in [r"C:\", "relative", r"C:\Windows\System32"] {
        config["clips"]["folder"] = folder.into();
        let (code, _, body) = call("PUT", "/api/config")
            .body(&config.to_string())
            .send(&app)
            .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{folder}: {body}");
    }
}

#[tokio::test]
async fn sessions_end_when_the_pin_changes_or_lan_goes_off() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());
    let login = |pin: &'static str| {
        let app = app.clone();
        async move {
            let (code, headers, _) = call("POST", "/api/login")
                .peer("10.0.0.7")
                .body(&format!(r#"{{"pin":"{pin}"}}"#))
                .send(&app)
                .await;
            (code, headers)
        }
    };
    let status = |cookie: String| {
        let app = app.clone();
        async move {
            call("GET", "/api/status")
                .peer("10.0.0.7")
                .header("cookie", &cookie)
                .send(&app)
                .await
                .0
        }
    };
    let cookie_of = |h: &HeaderMap| {
        h[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };

    let (code, h) = login("123456").await;
    assert_eq!(code, StatusCode::OK);
    let first = cookie_of(&h);
    assert_eq!(status(first.clone()).await, StatusCode::OK);

    // A new PIN kills the old session, even if the PIN later goes back.
    env.control.config.lock().unwrap().web.lan_pin = "654321".into();
    assert_eq!(status(first.clone()).await, StatusCode::UNAUTHORIZED);
    env.control.config.lock().unwrap().web.lan_pin = "123456".into();
    assert_eq!(status(first).await, StatusCode::UNAUTHORIZED);

    // LAN off ends sessions and blocks logins.
    let (_, h) = login("123456").await;
    let second = cookie_of(&h);
    assert_eq!(status(second.clone()).await, StatusCode::OK);
    env.control.config.lock().unwrap().web.lan = false;
    assert_eq!(status(second.clone()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(login("123456").await.0, StatusCode::UNAUTHORIZED);
    env.control.config.lock().unwrap().web.lan = true;
    assert_eq!(status(second).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lan_peers_get_no_error_details() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    // A clip whose file can't be trimmed gives an internal error; MP4 only, so use a missing ffmpeg-less path:
    // the Noop control fails the trim.
    put(&env, "G/a.mp4", 10);
    let app = router(env.state.clone());
    get_body(app.clone(), "/api/clips").await;
    let trim = r#"{"id":"G/a.mp4","start":0,"end":5}"#;
    let (code, _, local) = call("POST", "/api/clips/trim")
        .peer("127.0.0.1")
        .body(trim)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR, "{local}");
    let (_, headers, _) = call("POST", "/api/login")
        .peer("10.0.0.7")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let (code, _, remote) = call("POST", "/api/clips/trim")
        .peer("10.0.0.7")
        .header("cookie", &cookie)
        .body(trim)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        json(&remote)["error"].as_str().unwrap().contains("log"),
        "{remote}"
    );
    assert_ne!(json(&remote)["error"], json(&local)["error"]);
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
        .write_all(
            format!("GET /api/clips HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
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

const HOOK: &str =
    "https://discord.com/api/webhooks/123456789012345678/abcdefghijklmnopqrstuvwxyz_-0123456789";

#[tokio::test]
async fn discord_settings_are_checked_and_urls_stay_on_the_pc() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());
    let local_put = |config: serde_json::Value| {
        let app = app.clone();
        async move {
            call("PUT", "/api/config")
                .body(&config.to_string())
                .send(&app)
                .await
        }
    };
    let mut config = serde_json::to_value(env.control.config()).unwrap();
    config["discord"]["webhooks"] = serde_json::json!([{ "name": " #clips ", "url": HOOK, "mode": "auto" }]);
    let (code, _, body) = local_put(config.clone()).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(env.control.config().discord.webhooks[0].name, "#clips");
    assert_eq!(
        json(&body)["discord"]["webhooks"][0]["url"],
        HOOK,
        "the PC sees its URLs"
    );

    for bad in [
        serde_json::json!([{ "name": "a", "url": "https://evil.example/api/webhooks/1/abcdefghijklmnopqrstuvwxyz" }]),
        serde_json::json!([{ "name": "a", "url": HOOK }, { "name": "A", "url": HOOK }]),
        serde_json::json!([{ "name": "", "url": HOOK }]),
    ] {
        let mut c = config.clone();
        c["discord"]["webhooks"] = bad;
        let (code, _, body) = local_put(c).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    }
    let mut c = config.clone();
    c["discord"]["upload_limit_mb"] = 0.into();
    assert_eq!(local_put(c).await.0, StatusCode::BAD_REQUEST);

    // Another device with the PIN: no URLs out, none in.
    let lan = "192.168.1.50";
    let (_, headers, _) = call("POST", "/api/login")
        .peer(lan)
        .header("host", "192.168.1.5:7845")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let lan_call = |method: &'static str, uri: &'static str, body: Option<String>| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            let c = call(method, uri)
                .peer(lan)
                .header("host", "192.168.1.5:7845")
                .header("cookie", &cookie);
            match body {
                Some(b) => c.body(&b).send(&app).await,
                None => c.send(&app).await,
            }
        }
    };
    let (code, _, body) = lan_call("GET", "/api/config", None).await;
    assert_eq!(code, StatusCode::OK);
    let mut seen = json(&body);
    assert_eq!(seen["discord"]["webhooks"][0]["url"], "");
    assert!(!body.contains("abcdefghijklmnop"), "{body}");

    seen["discord"]["webhooks"][0]["mode"] = "ask".into();
    let (code, _, body) = lan_call("PUT", "/api/config", Some(seen.to_string())).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(
        env.control.config().discord.webhooks[0].url,
        HOOK,
        "a blank URL keeps the saved one"
    );
    assert_eq!(json(&body)["discord"]["webhooks"][0]["url"], "");

    let mut changed = seen.clone();
    changed["discord"]["webhooks"][0]["url"] = HOOK.replace("123456789012345678", "999").into();
    assert_eq!(
        lan_call("PUT", "/api/config", Some(changed.to_string())).await.0,
        StatusCode::FORBIDDEN
    );
    let mut added = seen.clone();
    added["discord"]["webhooks"] =
        serde_json::json!([seen["discord"]["webhooks"][0], { "name": "new", "url": HOOK }]);
    assert_eq!(
        lan_call("PUT", "/api/config", Some(added.to_string())).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(env.control.config().discord.webhooks.len(), 1);
    // A renamed channel is a new one with a URL from elsewhere: no.
    let mut renamed = seen.clone();
    renamed["discord"]["webhooks"][0]["name"] = "#other".into();
    renamed["discord"]["webhooks"][0]["url"] = HOOK.into();
    assert_eq!(
        lan_call("PUT", "/api/config", Some(renamed.to_string())).await.0,
        StatusCode::FORBIDDEN
    );
    // A new channel without a URL is fine; its URL is added on the PC.
    added["discord"]["webhooks"][1]["url"] = "".into();
    let (code, _, body) = lan_call("PUT", "/api/config", Some(added.to_string())).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let saved = env.control.config().discord.webhooks;
    assert_eq!(
        (saved.len(), saved[0].url.as_str(), saved[1].url.as_str()),
        (2, HOOK, "")
    );

    // Nor can it plant or clear who's connected.
    let mut planted = json(&body);
    planted["discord"]["account"] = serde_json::json!({
        "id": "1", "username": "someone", "name": null, "avatar": "https://cdn.discordapp.com/avatars/1/x.png"
    });
    let (code, _, body) = lan_call("PUT", "/api/config", Some(planted.to_string())).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(env.control.config().discord.account, None);

    // The UI can tell it's on another device.
    let (_, _, body) = lan_call("GET", "/api/system", None).await;
    assert_eq!(json(&body)["remote"], true);
    let (_, _, body) = call("GET", "/api/system").send(&app).await;
    assert_eq!(json(&body)["remote"], false);
}

#[tokio::test]
async fn discord_queue_endpoints() {
    let env = env();
    put(&env, "Game/a.mp4", 1000);
    let app = router(env.state.clone());
    let (code, body) = get_body(app.clone(), "/api/clips").await;
    assert_eq!(code, StatusCode::OK);
    let id = json(&body)[0]["id"].as_str().unwrap().to_string();

    let (code, body) = get_body(app.clone(), "/api/discord/posts").await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, "[]"));

    let share = |b: String| {
        let app = app.clone();
        async move { call("POST", "/api/discord/share").body(&b).send(&app).await }
    };
    let (code, _, body) = share(serde_json::json!({ "id": "nope", "webhook": "#clips" }).to_string()).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    // This server's queue has no channels set up.
    let (code, _, body) = share(serde_json::json!({ "id": id, "webhook": "#clips" }).to_string()).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        json(&body)["error"].as_str().unwrap().contains("#clips"),
        "{body}"
    );

    let (code, _, _) = call("POST", "/api/discord/answer")
        .body(r#"{"job":7,"yes":true}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    // A URL that isn't Discord's is refused before anything is sent.
    let (code, _, body) = call("POST", "/api/discord/test")
        .body(r#"{"url":"https://example.com/hook"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    let (code, _, _) = call("POST", "/api/discord/test")
        .body(r#"{"name":"missing"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn updates_report_check_and_refuse_to_apply_here() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());

    let (code, body) = get_body(app.clone(), "/api/update").await;
    assert_eq!(code, StatusCode::OK);
    let status = json(&body);
    assert_eq!(status["current"], clipr_core::VERSION);
    assert_eq!(status["state"], "idle");

    let (code, _, _) = call("POST", "/api/update/check").send(&app).await;
    assert_eq!(code, StatusCode::ACCEPTED);

    // NoopControl can't install: the reason comes back as a conflict.
    let (code, _, body) = call("POST", "/api/update/apply").send(&app).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(json(&body)["error"].is_string());

    // A signed-in LAN device may look, but not install (it would restart clipr under the player).
    let (_, headers, _) = call("POST", "/api/login")
        .peer("192.168.1.50")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
    let token = set_cookie.split(';').next().unwrap().to_string();
    let (code, _, _) = call("GET", "/api/update")
        .peer("192.168.1.50")
        .header("cookie", &token)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
    let (code, _, _) = call("POST", "/api/update/apply")
        .peer("192.168.1.50")
        .header("cookie", &token)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn crash_reports_list_dismiss_and_reveal_rules() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());

    let (code, body) = get_body(app.clone(), "/api/crashes").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json(&body), serde_json::json!([]));

    let (code, _, body) = call("POST", "/api/crashes/dismiss")
        .body(r#"{"id":"123"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(json(&body)["error"].is_string());
    let (code, _, _) = call("POST", "/api/crashes/dismiss").body("{}").send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // Opening Explorer is for the PC itself, never a LAN device.
    let (_, headers, _) = call("POST", "/api/login")
        .peer("192.168.1.50")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
    let token = set_cookie.split(';').next().unwrap().to_string();
    let (code, _, _) = call("POST", "/api/crashes/reveal")
        .peer("192.168.1.50")
        .header("cookie", &token)
        .body(r#"{"id":"123"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
}

/// Saving `web.lan` opens and closes the local network listener, no restart needed.
#[tokio::test]
async fn lan_access_opens_and_closes_without_a_restart() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut env = env();
    env.state.lan = Lan::new(port, false);
    let lan = env.state.lan.clone();
    // No loopback listener: only the LAN one (0.0.0.0, which loopback reaches too) can answer.
    let server = tokio::spawn(serve_many(Vec::new(), env.state.clone()));

    let connects = || tokio::net::TcpStream::connect(("127.0.0.1", port));
    let settle = || tokio::time::sleep(Duration::from_millis(150));
    settle().await;
    assert!(connects().await.is_err(), "off at start");
    lan.set(true);
    settle().await;
    assert!(connects().await.is_ok(), "opens when switched on");
    assert_eq!(lan.error(), None);
    lan.set(false);
    settle().await;
    assert!(connects().await.is_err(), "closes when switched off");
    lan.set(true);
    settle().await;
    assert!(connects().await.is_ok(), "opens again");

    env._tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn connect_discord_stays_on_this_pc() {
    let env = env();
    // On another port Discord can't send the browser back: the UI is told why.
    let app = router(env.state.clone());
    let (code, _, body) = call("POST", "/api/discord/connect").body("{}").send(&app).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(json(&body)["error"].as_str().unwrap().contains("7845"));

    let mut state = env.state.clone();
    state.lan = crate::Lan::new(clipr_discord::account::REDIRECT_PORT, false);
    let app = router(state);
    let (code, _, body) = call("POST", "/api/discord/connect")
        .body(r#"{"channel":true}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK);
    let v = json(&body);
    let url = v["url"].as_str().unwrap();
    let st = v["state"].as_str().unwrap().to_string();
    assert!(url.contains("webhook.incoming") && url.contains(&format!("state={st}")));

    // Still on Discord's page.
    let result = format!("/api/discord/connect/{st}");
    let (code, _, _) = call("GET", &result).send(&app).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    // A redirect with someone else's state does nothing.
    let (code, _, body) = call("GET", "/discord/callback?code=x&state=forged")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(body.contains("expired"));
    // The user cancels on Discord: the UI hears it, also when it asks again later.
    let cancel = format!("/discord/callback?error=access_denied&state={st}");
    let (code, _, body) = call("GET", &cancel).send(&app).await;
    assert_eq!(code, StatusCode::OK);
    assert!(body.contains("cancelled"));
    for _ in 0..2 {
        let (code, _, body) = call("GET", &result).send(&app).await;
        assert_eq!(code, StatusCode::BAD_GATEWAY);
        assert!(json(&body)["error"].as_str().unwrap().contains("cancelled"));
    }
    // The redirect only counts once (a reload doesn't trade the code again).
    let (code, _, body) = call("GET", &cancel).send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(body.contains("expired"));
    let (code, _, _) = call("GET", "/api/discord/connect/unknown").send(&app).await;
    assert_eq!(code, StatusCode::NOT_FOUND);

    // Never from another device, not even with the PIN.
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let (code, _, _) = call("GET", "/discord/callback?code=x&state=y")
        .peer("192.168.1.9")
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cloud_settings_are_checked_and_the_bucket_stays_on_the_pc() {
    let env = env();
    {
        let mut c = env.control.config.lock().unwrap();
        c.web.lan = true;
        c.web.lan_pin = "123456".into();
    }
    let app = router(env.state.clone());
    let mut config = serde_json::to_value(env.control.config()).unwrap();
    config["cloud"] = serde_json::json!({
        "enabled": true, "provider": "r2", "endpoint": " https://acc.r2.cloudflarestorage.com ",
        "region": "auto", "bucket": "my-clips", "access_key_id": "key", "secret_access_key": "shh",
        "public_url": "https://pub-1.r2.dev", "prefix": "clipr/", "upload": "after_game",
        "max_mbps": 20, "recordings": false
    });
    let (code, _, body) = call("PUT", "/api/config")
        .body(&config.to_string())
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(
        env.control.config().cloud.endpoint,
        "https://acc.r2.cloudflarestorage.com"
    );
    assert_eq!(
        json(&body)["cloud"]["secret_access_key"],
        "shh",
        "the PC sees its own key"
    );

    for (field, bad) in [
        ("endpoint", serde_json::json!("http://evil.example")),
        ("bucket", serde_json::json!("No_Caps")),
        ("public_url", serde_json::json!("javascript:alert(1)")),
        ("prefix", serde_json::json!("../x")),
        ("secret_access_key", serde_json::json!("")),
    ] {
        let mut c = config.clone();
        c["cloud"][field] = bad;
        let (code, _, body) = call("PUT", "/api/config").body(&c.to_string()).send(&app).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{field}: {body}");
    }

    // Another device with the PIN: no secret out, no bucket changes in.
    let lan = "192.168.1.50";
    let (_, headers, _) = call("POST", "/api/login")
        .peer(lan)
        .header("host", "192.168.1.5:7845")
        .body(r#"{"pin":"123456"}"#)
        .send(&app)
        .await;
    let cookie = headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let lan_call = |method: &'static str, uri: &'static str, body: Option<String>| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            let c = call(method, uri)
                .peer(lan)
                .header("host", "192.168.1.5:7845")
                .header("cookie", &cookie);
            match body {
                Some(b) => c.body(&b).send(&app).await,
                None => c.send(&app).await,
            }
        }
    };
    let (_, _, body) = lan_call("GET", "/api/config", None).await;
    let mut seen = json(&body);
    assert_eq!(seen["cloud"]["secret_access_key"], "");
    assert!(!body.contains("shh"));
    // Timing and the speed limit are fine; the blank secret keeps the saved one.
    seen["cloud"]["upload"] = "now".into();
    seen["cloud"]["max_mbps"] = 5.into();
    let (code, _, body) = lan_call("PUT", "/api/config", Some(seen.to_string())).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let saved = env.control.config().cloud;
    assert_eq!((saved.max_mbps, saved.secret_access_key.as_str()), (5, "shh"));
    assert!(!body.contains("shh"));
    // Pointing uploads somewhere else is not.
    for (field, value) in [
        ("endpoint", "https://evil.example"),
        ("bucket", "their-bucket"),
        ("public_url", "https://evil.example"),
        ("secret_access_key", "other"),
    ] {
        let mut c = seen.clone();
        c["cloud"][field] = value.into();
        let (code, _, _) = lan_call("PUT", "/api/config", Some(c.to_string())).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{field}");
    }
    assert_eq!(env.control.config().cloud.bucket, "my-clips");
    let (code, _, _) = lan_call("POST", "/api/cloud/test", Some("{}".into())).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _, _) = lan_call("POST", "/api/cloud/remove", Some(r#"{"id":"x.mp4"}"#.into())).await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // The lists: nothing uploaded yet (the test queue has cloud storage off).
    let (code, _, body) = call("GET", "/api/cloud/status").send(&app).await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, "{}"));
    let (code, _, body) = call("GET", "/api/cloud/uploads").send(&app).await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, "[]"));
    let (code, _, body) = call("POST", "/api/cloud/test")
        .body(r#"{"endpoint":"https://acc.r2.cloudflarestorage.com"}"#)
        .send(&app)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn volume_endpoint_saves_one_source() {
    let env = env();
    let app = router(env.state.clone());
    let post = |body: &'static str| call("POST", "/api/audio/volume").body(body);

    let (code, _, body) = post(r#"{"source":"Game","volume":150}"#).send(&app).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json(&body)["audio"]["volumes"]["game"], 150);
    let (code, _, body) = post(r#"{"source":"mic","volume":40}"#).send(&app).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json(&body)["audio"]["mic_volume"], 40);
    assert_eq!(env.control.config.lock().unwrap().audio.volume("game"), 150);
    // Back to 100 % drops the entry.
    let (_, _, body) = post(r#"{"source":"game","volume":100}"#).send(&app).await;
    assert!(json(&body)["audio"]["volumes"].as_object().unwrap().is_empty());

    let (code, _, _) = post(r#"{"source":"game","volume":201}"#).send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, _, _) = post(r#"{"source":"C:\x","volume":50}"#).send(&app).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, _, _) = call("POST", "/api/levels/watch").send(&app).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn perf_endpoint_returns_samples() {
    let env = env();
    let (code, body) = get_body(router(env.state.clone()), "/api/perf").await;
    assert_eq!(code, StatusCode::OK);
    assert!(json(&body).as_array().is_some());
}
