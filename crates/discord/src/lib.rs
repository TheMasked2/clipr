//! Discord integration: webhooks, the post queue and Rich Presence. No bot: posts go through webhooks, and
//! the "post this?" buttons live in clipr itself.

pub mod account;
pub mod presence;
pub mod queue;
pub mod webhook;

pub use presence::{Presence, Status as PresenceStatus};
pub use queue::{ClipInfo, Job, JobState, Poster};
