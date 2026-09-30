-- Durable recommendation job queue: retries with backoff and stuck-job
-- recovery.
--
-- run_after   - earliest time a pending job may be claimed (retry backoff)
-- locked_at   - when a worker claimed the job; a `running` job whose lock is
--               older than the job timeout belongs to a worker that died
-- max_attempts - attempts allowed before the job is failed for good
--
-- IF NOT EXISTS throughout: the snapshot-tail startup path re-runs every
-- migration's raw SQL on a fresh database (see crates/db/src/migrations.rs).
ALTER TABLE recommendation_jobs
    ADD COLUMN IF NOT EXISTS run_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ADD COLUMN IF NOT EXISTS locked_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS max_attempts INTEGER NOT NULL DEFAULT 3;

-- Jobs that were pending or running when this shipped were owned by an
-- in-process task that no longer exists. Make them claimable (the worker
-- retries them) rather than leaving them stuck forever.
UPDATE recommendation_jobs
   SET status = 'pending', run_after = NOW()
 WHERE status = 'running' AND locked_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_recommendation_jobs_due
    ON recommendation_jobs (run_after, created_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_recommendation_jobs_running
    ON recommendation_jobs (locked_at) WHERE status = 'running';
