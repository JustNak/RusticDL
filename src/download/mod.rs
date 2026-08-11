pub mod client;
pub mod drop_files;
pub mod duplicates;
pub mod engine;
pub mod filesystem;
pub mod handoff;
pub mod http;
pub mod job;
pub mod urls;

pub use drop_files::extract_urls_from_dropped_paths;
pub use duplicates::find_active_duplicate;
pub use engine::{
    open_path, reveal_in_folder, spawn_engine, EngineCommand, EngineEvent, EngineHandle,
};
pub use handoff::{EnqueueOutcome, EnqueueStatus, HandoffAuth, HandoffAuthHeader};
pub use job::{Job, JobState};
pub use urls::extract_http_urls;
