//! Terminal-job notification policy: edge detect, dual pipelines (in-app + OS),
//! OS burst coalesce, and balloon click context mapping.
//!
//! See design plan A1 (Windows completion notifications).

// Re-exports preserve the former `notifications.rs` public surface.
#![allow(unused_imports)]

mod balloon;
mod edges;
mod in_app;
mod os_buffer;
mod policy;
mod types;

pub use balloon::{compose_balloon, BalloonContextMap};
pub use edges::terminal_edges;
pub use in_app::in_app_summary_messages;
pub use os_buffer::OsNotifyBuffer;
pub use policy::{
    filter_notify_edges, filter_pending_by_toggles, hard_os_eligible, soft_os_eligible,
};
pub use types::{
    BalloonClickContext, BalloonOutcome, BalloonPayload, InAppToastKind, PendingOsTerminal,
    TerminalEdge, TerminalKind, BALLOON_CONTEXT_CAP, OS_BURST_WINDOW, OS_HIGH_WATER,
};
