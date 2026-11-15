//! `clipr.exe`: wires the pieces together and owns the app's lifecycle.
//!
//! Threads: the main thread runs the Windows message loop for the tray icon (it has to: tray icons belong
//! to the thread that made them). A small tokio runtime runs everything async, starting with the web server.
//!
//! Launching clipr while it already runs opens the web UI of the running copy and exits, so the desktop
//! shortcut and Start menu entry double as "open clipr".

// Release builds are a tray app with no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod tray;
mod win;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clipr_core::config::Config;
use clipr_core::events::{Bus, Event};
use clipr_core::paths::Paths;
use clipr_core::status::Status;
use clipr_web::AppState;

fn main() {
    if let Err(err) = run() {
        tracing::error!("{err:#}");
        win::error_box(&format!("clipr couldn't start:\n\n{err:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let paths = Paths::detect()?;

    let Some(_instance) = win::SingleInstance::acquire()? else {
        // Another clipr is running: open its UI instead of starting a second one.
        let port = std::fs::read_to_string(paths.port_file())
            .ok()
            .and_then(|p| p.trim().parse().ok());
        let port = port.unwrap_or(clipr_core::config::DEFAULT_PORT);
        win::open_url(&format!("http://127.0.0.1:{port}/"));
        return Ok(());
    };

    let _log_guard = clipr_core::logging::init(&paths.log_dir())?;
    tracing::info!("clipr {} starting", clipr_core::VERSION);

    let config = Config::load_or_recover(&paths.config_file());
    if !paths.config_file().exists()
        && let Err(e) = config.save(&paths.config_file())
    {
        tracing::warn!("couldn't write the default settings file: {e:#}");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("clipr-async")
        .enable_all()
        .build()
        .context("can't start the async runtime")?;

    let status = Arc::new(Status::new());
    let bus = Bus::new();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let (listener, port) = runtime.block_on(clipr_web::bind(config.web.port))?;
    let ui_url = format!("http://127.0.0.1:{port}/");
    std::fs::write(paths.port_file(), port.to_string()).context("can't write the port file")?;
    tracing::info!("web UI at {ui_url}");

    let server = runtime.spawn(clipr_web::serve(
        listener,
        AppState {
            status: status.clone(),
            shutdown: shutdown_rx,
        },
    ));

    // A tray app gives no sign it started, so show the UI, unless Windows started us at login.
    let background = std::env::args().any(|a| a == "--background");
    if config.general.open_ui_on_start && !background {
        win::open_url(&ui_url);
    }

    // The tray lives on this thread until the user quits.
    let tray_result = tray::run(&runtime, status, &ui_url);

    tracing::info!("shutting down");
    bus.emit(Event::ShutdownRequested);
    let _ = shutdown_tx.send(true);
    runtime.block_on(async {
        match tokio::time::timeout(Duration::from_secs(3), server).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => tracing::warn!("{e:#}"),
            Ok(Err(e)) => tracing::warn!("web server task failed: {e}"),
            Err(_) => tracing::warn!("web server didn't stop within 3 s"),
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let _ = std::fs::remove_file(paths.port_file());
    tracing::info!("bye");
    tray_result
}
