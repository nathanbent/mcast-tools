//! Intercom control panel for `mcast serve`.
//!
//! Zones (sets of speakers), announcements (audio, chimes, and the zones they
//! go to) and a runner that plays an announcement by starting one sender per
//! multicast group through the mcast HTTP API.
//!
//! This crate is a client of that API and nothing else: it doesn't link the
//! other mcast crates, so it can be split out on its own. See
//! `docs/intercom-plan.md` for where it's going.

pub mod backend;
pub mod cli;
pub mod model;
pub mod runner;
pub mod server;
pub mod store;

pub use backend::{Backend, BackendError, HttpBackend};
pub use model::{Announcement, Audio, Config, Priority, Target, Zone};
pub use runner::{
    BackendStatus, Change, PlayError, Run, RunId, RunState, Runner, TargetRun, TargetState, Trigger,
};

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
