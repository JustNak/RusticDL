//! Headless driver for one download through the production engine.
//!
//! Compiled by `prove-download-engine.sh` as a crate root that `#[path]`s the
//! app modules. The call is the same one `src/main.rs` makes: `spawn_engine`
//! with `FileJobStore`, then `EngineCommand::Add`.

#![allow(dead_code, unused_imports)]

#[path = "../../../../src/branding.rs"]
mod branding;
#[path = "../../../../src/download/mod.rs"]
mod download;
#[path = "../../../../src/extension_settings.rs"]
mod extension_settings;
#[path = "../../../../src/persistence.rs"]
mod persistence;
#[path = "../../../../src/settings/mod.rs"]
mod settings;
#[path = "../../../../src/updater/mod.rs"]
mod updater;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use download::{spawn_engine, EngineCommand, EngineEvent, EngineRuntimeConfig, FileJobStore};

fn require(flag: &str) -> String {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next().unwrap_or_else(|| {
                eprintln!("missing value for {flag}");
                std::process::exit(2);
            });
        }
    }
    eprintln!("missing {flag}");
    std::process::exit(2);
}

fn optional(flag: &str) -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
    }
    None
}

#[tokio::main]
async fn main() {
    let url = require("--url");
    let dest = PathBuf::from(require("--dest"));
    let filename = require("--filename");
    let data_dir = PathBuf::from(require("--data-dir"));
    let timeout_secs: u64 = optional("--timeout")
        .unwrap_or_else(|| "60".into())
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("--timeout must be a whole number of seconds");
            std::process::exit(2);
        });

    std::fs::create_dir_all(&dest).expect("create dest");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let paths = persistence::AppPaths {
        settings: data_dir.join("settings.json"),
        state: data_dir.join("state.json"),
        pending_whats_new: data_dir.join("pending_whats_new.json"),
        root: data_dir,
    };

    let (engine, mut events) = spawn_engine(
        Vec::new(),
        EngineRuntimeConfig::default(),
        Arc::new(FileJobStore { paths }),
    );

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    engine.send(EngineCommand::Add {
        url,
        filename: Some(filename),
        directory: dest,
        handoff_auth: None,
        conflict: download::FilenameConflictPolicy::Uniquify,
        total_bytes: None,
        reply: Some(reply_tx),
    });

    let outcome = match tokio::time::timeout(Duration::from_secs(15), reply_rx).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => {
            eprintln!("engine dropped the enqueue reply");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("timed out waiting for enqueue reply");
            std::process::exit(1);
        }
    };
    println!("enqueue_status={}", outcome.status.as_protocol());
    println!("job_id={}", outcome.job_id);
    println!("job_filename={}", outcome.filename);
    let job_id = outcome.job_id;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last: Option<download::Job> = None;
    let mut timed_out = false;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }
        match tokio::time::timeout(deadline.saturating_duration_since(now), events.recv()).await {
            Ok(Some(EngineEvent::JobsChanged(jobs))) => {
                if let Some(job) = jobs.iter().find(|job| job.id == job_id) {
                    let terminal = job.state.is_terminal();
                    last = Some(job.clone());
                    if terminal {
                        break;
                    }
                }
            }
            Ok(Some(EngineEvent::Toast(message))) => {
                println!("toast={}", message.replace('\n', " "));
            }
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }

    engine.send(EngineCommand::Shutdown);

    println!("timed_out={timed_out}");
    match last {
        Some(job) => {
            println!("state={}", job.state.label());
            println!(
                "error={}",
                job.error.as_deref().unwrap_or("-").replace('\n', " ")
            );
            println!(
                "failure_category={}",
                job.failure_category
                    .map(|category| format!("{category:?}"))
                    .unwrap_or_else(|| "-".into())
            );
            println!("downloaded_bytes={}", job.downloaded_bytes);
            println!("total_bytes={}", job.total_bytes);
            println!("resume_supported={}", job.resume_supported);
            println!("target_path={}", job.target_path.display());
            println!("target_exists={}", job.target_path.is_file());
            println!("temp_path={}", job.temp_path.display());
            println!("temp_exists={}", job.temp_path.is_file());
        }
        None => {
            println!("state=-");
            println!("error=-");
            println!("failure_category=-");
            println!("downloaded_bytes=0");
            println!("total_bytes=0");
            println!("resume_supported=false");
            println!("target_path=-");
            println!("target_exists=false");
            println!("temp_path=-");
            println!("temp_exists=false");
        }
    }
}
