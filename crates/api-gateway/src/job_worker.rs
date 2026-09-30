//! Background worker for `recommendation_jobs`.
//!
//! Jobs live in Postgres, so they survive a restart and can be claimed by any
//! gateway replica: a worker takes one due `pending` job with
//! `FOR UPDATE SKIP LOCKED`, runs the same pipeline as the synchronous
//! endpoint, and records the outcome. Transient failures go back to `pending`
//! with a backoff until `max_attempts`; a `running` job whose lock has gone
//! stale (its worker died) is returned to `pending` or failed by the reaper.
//!
//! Concurrency is bounded by `AppState::ai_slots`, the same semaphore the
//! synchronous endpoint takes, so interactive requests and queued jobs share
//! one budget for the model server.

use std::time::Duration;

use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::AppError;
use sqlx::Row;
use uuid::Uuid;

use crate::routes::analyses::{generate_analysis_for_request, GenerateAnalysisRequest};
use crate::state::AppState;

/// Backoff before attempt 2, 3, ...; the last value repeats.
const RETRY_DELAYS_SECS: &[u64] = &[5, 30, 120];
/// How often the reaper looks for jobs whose worker died.
const REAP_INTERVAL: Duration = Duration::from_secs(30);
/// Slack past the job timeout before a `running` lock is treated as stale, so
/// a job that is merely finishing is not reclaimed underneath its worker.
const STALE_LOCK_SLACK_SECS: u64 = 60;

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Delay before the next attempt, given how many attempts have been made.
pub(crate) fn retry_delay(attempts: i32) -> Duration {
    let index = (attempts.max(1) as usize - 1).min(RETRY_DELAYS_SECS.len() - 1);
    Duration::from_secs(RETRY_DELAYS_SECS[index])
}

/// Whether trying again could help. A missing denial or a rejected request
/// will fail the same way every time; a database, upstream or internal error
/// may not.
pub(crate) fn is_transient(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Db(_) | AppError::Upstream(_) | AppError::Internal(_)
    )
}

struct ClaimedJob {
    id: Uuid,
    denial_id: Uuid,
    organization_id: Uuid,
    requested_by: Option<Uuid>,
    temperature: f32,
    attempts: i32,
    max_attempts: i32,
}

/// Start the worker loop. Returns immediately; the loop runs for the life of
/// the process.
pub fn spawn(state: AppState) {
    let poll = Duration::from_secs(env_u64("AI_JOB_POLL_SECS", 2));
    let timeout = Duration::from_secs(env_u64("AI_JOB_TIMEOUT_SECS", 300));
    tokio::spawn(async move { run(state, poll, timeout).await });
}

async fn run(state: AppState, poll: Duration, timeout: Duration) {
    tracing::info!(
        poll_secs = poll.as_secs(),
        timeout_secs = timeout.as_secs(),
        "recommendation job worker started"
    );
    // `None` so the first pass reaps immediately; subtracting from `now()`
    // would panic on a host that booted less than REAP_INTERVAL ago.
    let mut last_reap: Option<tokio::time::Instant> = None;
    loop {
        if last_reap.is_none_or(|at| at.elapsed() >= REAP_INTERVAL) {
            last_reap = Some(tokio::time::Instant::now());
            if let Err(error) = reap_stale(&state, timeout).await {
                tracing::warn!("recommendation job reaper failed: {error}");
            }
        }

        // Hold a slot before claiming, so a claimed job always has capacity
        // and nothing sits in `running` waiting for one.
        let permit = match state.ai_slots.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return, // semaphore closed: shutting down
        };
        match claim_next(&state).await {
            Ok(Some(job)) => {
                let worker_state = state.clone();
                tokio::spawn(async move {
                    process(worker_state, job, timeout).await;
                    drop(permit);
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(poll).await;
            }
            Err(error) => {
                drop(permit);
                tracing::warn!("could not claim a recommendation job: {error}");
                tokio::time::sleep(poll.max(Duration::from_secs(5))).await;
            }
        }
    }
}

async fn claim_next(state: &AppState) -> Result<Option<ClaimedJob>, sqlx::Error> {
    let row = sqlx::query(
        "UPDATE recommendation_jobs \
            SET status = 'running', started_at = NOW(), locked_at = NOW(), \
                attempts = attempts + 1 \
          WHERE id = ( \
                SELECT id FROM recommendation_jobs \
                 WHERE status = 'pending' AND run_after <= NOW() \
                 ORDER BY run_after, created_at \
                 FOR UPDATE SKIP LOCKED LIMIT 1) \
      RETURNING id, denial_id, organization_id, requested_by, temperature, attempts, max_attempts",
    )
    .fetch_optional(&state.pool)
    .await?;
    row.map(|row| {
        Ok(ClaimedJob {
            id: row.try_get("id")?,
            denial_id: row.try_get("denial_id")?,
            organization_id: row.try_get("organization_id")?,
            requested_by: row.try_get("requested_by")?,
            temperature: row.try_get("temperature")?,
            attempts: row.try_get("attempts")?,
            max_attempts: row.try_get("max_attempts")?,
        })
    })
    .transpose()
}

/// The identity a job runs as: the user who queued it, in their organization.
/// If that user has since been deleted the job still runs (it was authorised
/// when queued) with no user attributed in the audit trail.
async fn job_principal(state: &AppState, job: &ClaimedJob) -> Principal {
    let username = match job.requested_by {
        Some(user_id) => {
            sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    Principal {
        kind: PrincipalKind::User,
        user_id: job.requested_by.map(|id| id.to_string()),
        username: username.unwrap_or_else(|| "recommendation-job".to_string()),
        organization_id: Some(job.organization_id.to_string()),
        ..Principal::anonymous()
    }
}

async fn process(state: AppState, job: ClaimedJob, timeout: Duration) {
    let principal = job_principal(&state, &job).await;
    let request = GenerateAnalysisRequest {
        denial_id: job.denial_id.to_string(),
        temperature: job.temperature,
    };
    let outcome = match tokio::time::timeout(
        timeout,
        generate_analysis_for_request(state.clone(), request, Some(principal)),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(AppError::Upstream(format!(
            "recommendation timed out after {}s",
            timeout.as_secs()
        ))),
    };

    let written = match outcome {
        Ok(result) => {
            sqlx::query(
                "UPDATE recommendation_jobs \
                    SET status = 'completed', result = $2::jsonb, error_message = NULL, \
                        locked_at = NULL, completed_at = NOW() \
                  WHERE id = $1",
            )
            .bind(job.id)
            .bind(result.to_string())
            .execute(&state.pool)
            .await
        }
        Err(error) if is_transient(&error) && job.attempts < job.max_attempts => {
            let delay = retry_delay(job.attempts);
            tracing::warn!(job_id = %job.id, attempt = job.attempts, "recommendation job will retry: {error}");
            sqlx::query(
                "UPDATE recommendation_jobs \
                    SET status = 'pending', error_message = $2, locked_at = NULL, \
                        run_after = NOW() + make_interval(secs => $3) \
                  WHERE id = $1",
            )
            .bind(job.id)
            .bind(error.to_string())
            .bind(delay.as_secs() as f64)
            .execute(&state.pool)
            .await
        }
        Err(error) => {
            tracing::warn!(job_id = %job.id, attempt = job.attempts, "recommendation job failed: {error}");
            sqlx::query(
                "UPDATE recommendation_jobs \
                    SET status = 'failed', error_message = $2, locked_at = NULL, \
                        completed_at = NOW() \
                  WHERE id = $1",
            )
            .bind(job.id)
            .bind(error.to_string())
            .execute(&state.pool)
            .await
        }
    };
    // If this write is lost the lock goes stale and the reaper recovers the job.
    if let Err(error) = written {
        tracing::error!(job_id = %job.id, "could not record recommendation job outcome: {error}");
    }
}

/// Recover jobs whose worker died mid-run: back to `pending` while attempts
/// remain, otherwise failed.
async fn reap_stale(state: &AppState, timeout: Duration) -> Result<(), sqlx::Error> {
    let stale_after = (timeout.as_secs() + STALE_LOCK_SLACK_SECS) as f64;
    let reaped = sqlx::query(
        "UPDATE recommendation_jobs \
            SET status = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'pending' END, \
                error_message = 'worker stopped before finishing (attempt ' || attempts || ')', \
                locked_at = NULL, run_after = NOW(), \
                completed_at = CASE WHEN attempts >= max_attempts THEN NOW() END \
          WHERE status = 'running' AND locked_at < NOW() - make_interval(secs => $1)",
    )
    .bind(stale_after)
    .execute(&state.pool)
    .await?
    .rows_affected();
    if reaped > 0 {
        tracing::warn!(reaped, "recovered stale recommendation jobs");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_backs_off_then_holds() {
        assert_eq!(retry_delay(1), Duration::from_secs(5));
        assert_eq!(retry_delay(2), Duration::from_secs(30));
        assert_eq!(retry_delay(3), Duration::from_secs(120));
        assert_eq!(retry_delay(9), Duration::from_secs(120));
        // Defensive: a zero or negative count still yields the first delay.
        assert_eq!(retry_delay(0), Duration::from_secs(5));
        assert_eq!(retry_delay(-4), Duration::from_secs(5));
    }

    #[test]
    fn only_infrastructure_errors_are_retried() {
        assert!(is_transient(&AppError::Upstream("llm down".into())));
        assert!(is_transient(&AppError::Internal("boom".into())));
        assert!(is_transient(&AppError::Db(sqlx::Error::PoolTimedOut)));
        assert!(!is_transient(&AppError::NotFound));
        assert!(!is_transient(&AppError::Forbidden));
        assert!(!is_transient(&AppError::BadRequest("bad".into())));
        assert!(!is_transient(&AppError::Conflict("dup".into())));
    }
}
