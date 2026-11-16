//! Discord integration. Webhooks and the post queue for now; the bot on the user's own token, slash commands
//! and Rich Presence follow (see `docs/dev-plan.md`, Phase 6).

pub mod queue;
pub mod webhook;

pub use queue::{ClipInfo, Job, JobState, Poster};
