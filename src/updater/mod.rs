//! Auto-updater backed by GitHub Releases.
//! Channel selection (`UpdateChannel`):
//! - **Stable** — `GET …/releases/latest` (GitHub’s latest non-prerelease).
//! - **Nightly** — list releases and pick the newest published `vX.Y.Z-nightly.*`
//!   pre-release that includes the setup installer.
//! Staged flow (main app UI in `update_flow`):
//! 1. Query the GitHub Releases API for the latest tag + assets on the chosen channel.
//! 2. Offer that channel’s current build when this install is not already it
//!    (channel switch is not a semver “newer” check). Toast stages:
//!    Checking → You're up to date | Update available [Update].
//! 3. On Update, flush app state, spawn **RusticDL Updater** with the setup
//!    download URL, then quit. The updater downloads, closes this app if it is
//!    still running, runs NSIS `/S` (no `/R`), and relaunches once after replace.

// Re-exports preserve the former `updater.rs` public surface.
#![allow(unused_imports)]

mod github;
mod launch;
mod version;

pub use github::{
    check_for_update, latest_release_api, latest_release_page, open_release_page, open_url,
    releases_list_api, releases_page, UpdateCheck, UpdateInfo,
};
pub use launch::{launch_updater, updater_exe_path, LaunchUpdaterOpts};
pub use version::{is_newer, is_nightly_version, normalize_version, should_offer_on_channel};
