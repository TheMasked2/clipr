//! The system tray icon and its menu.
//!
//! Menu clicks, icon clicks and status changes arrive on other threads; each one posts a wake-up to the main
//! thread's message loop, which then drains the queues here. All tray objects stay on the main thread.

use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clipr_core::status::{RecState, Status};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::{icon, win};

enum Wake {
    Menu(MenuEvent),
    Click,
    Status,
}

struct Items {
    status: MenuItem,
    open: MenuItem,
    gallery: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
}

/// Shows the tray icon and runs until the user picks Quit.
pub fn run(runtime: &tokio::runtime::Runtime, status: Arc<Status>, ui_url: &str) -> Result<()> {
    let main_thread = win::current_thread_id();
    let (tx, rx) = mpsc::channel::<Wake>();

    let items = Items {
        status: MenuItem::new(status.tray_line(), false, None),
        open: MenuItem::new("Open clipr", true, None),
        gallery: MenuItem::new("Gallery", true, None),
        settings: MenuItem::new("Settings", true, None),
        quit: MenuItem::new("Quit clipr", true, None),
    };
    // Quick actions show up now so the menu's shape is final; they work once capture exists (Phase 2).
    let save_clip = MenuItem::new("Save clip", false, None);
    let record = MenuItem::new("Start recording", false, None);
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &save_clip,
        &record,
        &PredefinedMenuItem::separator(),
        &items.open,
        &items.gallery,
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

    win::message_loop(|| {
        while let Ok(wake) = rx.try_recv() {
            match wake {
                Wake::Click => win::open_url(ui_url),
                Wake::Menu(event) => {
                    let id = event.id();
                    if id == items.open.id() {
                        win::open_url(ui_url);
                    } else if id == items.gallery.id() {
                        win::open_url(&format!("{ui_url}#/gallery"));
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
    });

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
