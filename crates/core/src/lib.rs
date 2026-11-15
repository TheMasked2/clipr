//! Shared foundations for every clipr crate: where files live ([`paths`]), the settings file ([`config`]),
//! log files ([`logging`]), and what clipr is doing right now plus the events around it ([`status`],
//! [`events`]).

pub mod config;
pub mod events;
pub mod logging;
pub mod paths;
pub mod status;

/// clipr's version, as shown in the UI and the tray.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
