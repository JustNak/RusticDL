pub mod bandwidth;
pub mod client;
pub mod conn_budget;
pub mod drop_files;
pub mod duplicates;
pub mod engine;
pub mod eta;
pub mod filesystem;
pub mod handoff;
pub mod http;
pub mod job;
pub mod multi;
pub mod preflight;
pub mod segment;
pub mod segment_io;
pub mod transfer;
pub mod urls;
pub mod verify;

pub use drop_files::extract_urls_from_dropped_paths;
pub use duplicates::find_active_duplicate;
pub use engine::{
    open_path, reveal_in_folder, spawn_engine, EngineCommand, EngineEvent, EngineHandle,
    EngineRuntimeConfig,
};
pub use handoff::{EnqueueOutcome, EnqueueStatus, HandoffAuth, HandoffAuthHeader};
pub use job::{fallback_reason_label, Job, JobState};
pub use urls::extract_http_urls;
