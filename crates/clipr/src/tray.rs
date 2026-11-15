//! The system tray icon, its menu, and the global hotkeys.
//!
//! Menu clicks, icon clicks and status changes arrive on other threads; each one posts a wake-up to the main
//! thread's message loop, which then drains the queues here. Hotkeys are registered on the main thread and
//! arrive through the same loop. All tray objects stay on the main thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clipr_core::config::HotkeyConfig;
use clipr_core::status::{RecState, Status};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::engine::{Command, Engine};
use crate::{icon, win};

enum Wake {
    Menu(MenuEvent),
    Click,
    Status,
}

/// Hotkey ids, as registered with Windows.
#[derive(Clone, Copy, Debug)]
enum Action {
    SaveClip = 1,
    SaveLongClip = 2,
    ToggleBuffer = 3,
    ToggleRecording = 4,
    Mark = 5,
}

impl Action {
    const ALL: [Action; 5] = [
        Action::SaveClip,
        Action::SaveLongClip,
        Action::ToggleBuffer,
        Action::ToggleRecording,
        Action::Mark,
    ];

    fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|a| *a as i32 == id)
    }

    fn key<'a>(&self, keys: &'a HotkeyConfig) -> &'a str {
        match self {
            Action::SaveClip => &keys.save_clip,
            Action::SaveLongClip => &keys.save_long_clip,
            Action::ToggleBuffer => &keys.toggle_buffer,
            Action::ToggleRecording => &keys.toggle_recording,
            Action::Mark => &keys.mark,
        }
    }
}

/// Registers the configured hotkeys, replacing earlier ones. Returns a message per hotkey that failed.
pub fn register_hotkeys(keys: &HotkeyConfig) -> Vec<String> {
    let mut problems = Vec::new();
    for action in Action::ALL {
        win::unregister_hotkey(action as i32);
        let text = action.key(keys);
        match clipr_core::hotkey::parse(text) {
            Ok(None) => {}
            Ok(Some(key)) => {
                if let Err(e) = win::register_hotkey(action as i32, key) {
                    problems.push(format!("Hotkey {text} ({action:?}): {e:#}"));
                }
            }
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    for p in &problems {
        tracing::warn!("{p}");
    }
    problems
}

struct Items {
    status: MenuItem,
    save_clip: MenuItem,
    pause: MenuItem,
    open: MenuItem,
    gallery: MenuItem,
    folder: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
}

/// Shows the tray icon and runs until the user picks Quit.
pub fn run(
    runtime: &tokio::runtime::Runtime,
    status: Arc<Status>,
    ui_url: &str,
    engine: &Engine,
    hotkeys: &HotkeyConfig,
    clips_dir: impl Fn() -> PathBuf,
) -> Result<()> {
    let main_thread = win::current_thread_id();
    let (tx, rx) = mpsc::channel::<Wake>();
    register_hotkeys(hotkeys);

    let with_key = |label: &str, key: &str| {
        if key.is_empty() {
            label.to_string()
        } else {
            format!("{label}\t{key}")
        }
    };
    let items = Items {
        status: MenuItem::new(status.tray_line(), false, None),
        save_clip: MenuItem::new(with_key("Save clip", &hotkeys.save_clip), true, None),
        pause: MenuItem::new("Pause buffer", true, None),
        open: MenuItem::new("Open clipr", true, None),
        gallery: MenuItem::new("Gallery", true, None),
        folder: MenuItem::new("Open clips folder", true, None),
        settings: MenuItem::new("Settings", true, None),
        quit: MenuItem::new("Quit clipr", true, None),
    };
    // Full recordings come with Phase 5.
    let record = MenuItem::new("Start recording", false, None);
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &items.save_clip,
        &record,
        &items.pause,
        &PredefinedMenuItem::separator(),
        &items.open,
        &items.gallery,
        &items.folder,
        &items.settings,
        &PredefinedMenuItem::separator(),
        &items.quit,
    ])
    .context("can't build the tray menu")?;

    let mut state = status.activity().state;
    let tray = TrayIconBuilder::new()
        .with_tooltip(tooltip(&status))
        .with_icon(make_icon(state)?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .context("can't create the tray icon")?;

    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_tx.send(Wake::Menu(event));
        win::wake(main_thread);
    }));
    let click_tx = tx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = click_tx.send(Wake::Click);
            win::wake(main_thread);
        }
    }));
    let mut changes = status.subscribe();
    runtime.spawn(async move {
        while changes.changed().await.is_ok() {
            if tx.send(Wake::Status).is_err() {
                break;
            }
            win::wake(main_thread);
        }
    });

    let mut paused = false;
    win::message_loop(
        || {
            while let Ok(wake) = rx.try_recv() {
                match wake {
                    Wake::Click => win::open_url(ui_url),
                    Wake::Menu(event) => {
                        let id = event.id();
                        if id == items.save_clip.id() {
                            engine.send(Command::SaveClip(None));
                        } else if id == items.pause.id() {
                            paused = !paused;
                            engine.send(Command::SetBuffering(!paused));
                            items
                                .pause
                                .set_text(if paused { "Resume buffer" } else { "Pause buffer" });
                        } else if id == items.open.id() {
                            win::open_url(ui_url);
                        } else if id == items.gallery.id() {
                            win::open_url(&format!("{ui_url}#/gallery"));
                        } else if id == items.folder.id() {
                            let dir = clips_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            win::reveal(&dir);
                        } else if id == items.settings.id() {
                            win::open_url(&format!("{ui_url}#/settings"));
                        } else if id == items.quit.id() {
                            win::quit_loop();
                        }
                    }
                    Wake::Status => {
                        items.status.set_text(status.tray_line());
                        let _ = tray.set_tooltip(Some(tooltip(&status)));
                        let now = status.activity().state;
                        if now != state {
                            state = now;
                            match make_icon(state) {
                                Ok(icon) => {
                                    let _ = tray.set_icon(Some(icon));
                                }
                                Err(e) => tracing::warn!("{e:#}"),
                            }
                        }
                    }
                }
            }
        },
        |id| match Action::from_id(id) {
            Some(Action::SaveClip) => engine.send(Command::SaveClip(None)),
            Some(Action::SaveLongClip) => engine.send(Command::SaveLongClip),
            Some(Action::ToggleBuffer) => engine.send(Command::ToggleBuffering),
            Some(Action::Mark) => engine.send(Command::Mark),
            Some(Action::ToggleRecording) => tracing::info!("full recordings arrive in Phase 5"),
            None => {}
        },
    );

    for action in Action::ALL {
        win::unregister_hotkey(action as i32);
    }
    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    drop(tray);
    Ok(())
}

fn make_icon(state: RecState) -> Result<Icon> {
    Icon::from_rgba(icon::rgba(state), icon::SIZE, icon::SIZE).context("can't draw the tray icon")
}

fn tooltip(status: &Status) -> String {
    format!("clipr · {}", status.tray_line())
}
