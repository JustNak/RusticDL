//! Add / enqueue command handler.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{oneshot, Mutex};

use super::super::super::duplicates::find_active_duplicate;
use super::super::super::filesystem::{
    allocate_download_paths, derive_filename_from_url, sanitize_filename, FilenameConflictPolicy,
};
use super::super::super::handoff::{EnqueueOutcome, EnqueueStatus, HandoffAuth};
use super::super::super::job::Job;
use super::super::super::urls::extract_http_urls;
use super::super::{emit_jobs_locked, emit_toast, EngineInner};

pub(super) async fn handle(
    inner: &Arc<Mutex<EngineInner>>,
    url: String,
    filename: Option<String>,
    directory: PathBuf,
    handoff_auth: Option<HandoffAuth>,
    conflict: FilenameConflictPolicy,
    reply: Option<oneshot::Sender<EnqueueOutcome>>,
) {
    // Split newlines/spaces and glued pastes (…tokenhttps://other…).
    let mut urls = extract_http_urls(&url);
    if urls.is_empty() {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            emit_toast(inner, "URL is empty.".into()).await;
            return;
        }
        urls.push(trimmed);
    }

    let mut added = 0u32;
    let mut skipped = 0u32;
    let mut last_error: Option<String> = None;
    // Insert newest-first while preserving paste order (first URL ends up on top).
    let mut new_jobs = Vec::new();
    // First successfully enqueued job in this Add (Queued reply when added > 0).
    let mut first_outcome: Option<EnqueueOutcome> = None;
    // First active-duplicate identity (pure-dup reply/toast when added == 0).
    let mut first_dup: Option<(String, String)> = None;

    {
        let mut guard = inner.lock().await;
        let mut occupied_targets: Vec<PathBuf> = guard
            .jobs
            .iter()
            .map(|job| job.target_path.clone())
            .collect();
        let mut occupied_temps: Vec<PathBuf> =
            guard.jobs.iter().map(|job| job.temp_path.clone()).collect();
        occupied_temps.extend(guard.pending_partial_deletes.values().cloned());

        for url in urls {
            if url::Url::parse(&url).is_err() {
                last_error = Some(format!("Invalid URL: {url}"));
                continue;
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                last_error = Some("Only HTTP and HTTPS URLs are supported.".into());
                continue;
            }

            // Active exact request-URL match: skip (Paused counts as active).
            // Also check jobs created earlier in this batch.
            if let Some(existing) = find_active_duplicate(&guard.jobs, &url)
                .or_else(|| find_active_duplicate(&new_jobs, &url))
            {
                skipped += 1;
                if first_dup.is_none() {
                    first_dup = Some((existing.id.clone(), existing.filename.clone()));
                }
                continue;
            }

            // Suggested filename applies to the first successfully enqueued job only.
            let preferred = if first_outcome.is_none() {
                filename
                    .as_ref()
                    .map(|f| sanitize_filename(f.trim()))
                    .filter(|f| !f.is_empty())
                    .or_else(|| derive_filename_from_url(&url))
                    .unwrap_or_else(|| "download.bin".into())
            } else {
                derive_filename_from_url(&url).unwrap_or_else(|| "download.bin".into())
            };

            // Overwrite applies to the first successfully enqueued job only.
            let policy = if first_outcome.is_none() {
                conflict
            } else {
                FilenameConflictPolicy::Uniquify
            };
            let (name, target, temp, replace_existing) = allocate_download_paths(
                &directory,
                &preferred,
                &occupied_targets,
                &occupied_temps,
                &guard.jobs,
                &new_jobs,
                policy,
            );
            if replace_existing && temp.exists() {
                let _ = std::fs::remove_file(&temp);
            }
            occupied_targets.push(target.clone());
            occupied_temps.push(temp.clone());

            let mut job = Job::new(url, name, target, temp);
            job.replace_existing = replace_existing;
            if first_outcome.is_none() {
                first_outcome = Some(EnqueueOutcome {
                    job_id: job.id.clone(),
                    filename: job.filename.clone(),
                    status: EnqueueStatus::Queued,
                });
            }
            new_jobs.push(job);
            added += 1;
        }

        if added == 0 {
            drop(guard);
            if skipped > 0 {
                // Pure-dup path: always reply when oneshot present (never drop).
                // first_dup is set on every skip that increments skipped.
                if let Some((dup_id, dup_name)) = first_dup {
                    if let Some(reply) = reply {
                        let _ = reply.send(EnqueueOutcome {
                            job_id: dup_id,
                            filename: dup_name.clone(),
                            status: EnqueueStatus::DuplicateExistingJob,
                        });
                    }
                    let message = if skipped == 1 {
                        format!("Already downloading: {dup_name}")
                    } else {
                        format!("Skipped {skipped} duplicate(s).")
                    };
                    emit_toast(inner, message).await;
                } else {
                    // Defensive: skipped > 0 without identity — treat like invalid.
                    emit_toast(
                        inner,
                        last_error.unwrap_or_else(|| "No valid download URLs found.".into()),
                    )
                    .await;
                }
                return;
            }
            emit_toast(
                inner,
                last_error.unwrap_or_else(|| "No valid download URLs found.".into()),
            )
            .await;
            // Leave reply dropped: IPC already validates URLs; UI Add path has no reply.
            return;
        }

        // Reverse so the first pasted URL is the first job in the list (insert 0 order).
        for job in new_jobs.into_iter().rev() {
            if let Some(auth) = handoff_auth.as_ref() {
                // Attach browser session auth only to the first successfully
                // enqueued job in this Add (matches first_outcome).
                if first_outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.job_id == job.id)
                {
                    guard.handoff_auth.insert(job.id.clone(), auth.clone());
                }
            }
            guard.jobs.insert(0, job);
        }
        emit_jobs_locked(&guard);
        guard.wake.notify_one();
    }

    if let Some(reply) = reply {
        if let Some(outcome) = first_outcome {
            let _ = reply.send(outcome);
        }
    }

    if skipped > 0 {
        emit_toast(
            inner,
            format!("Skipped {skipped} duplicate(s); added {added}."),
        )
        .await;
    } else if added > 1 {
        emit_toast(
            inner,
            format!("Added {added} downloads (split multi-URL paste)."),
        )
        .await;
    }
}
