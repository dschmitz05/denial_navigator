-- Scope recommendation jobs to an organization.
--
-- Jobs were readable by job id alone, so any signed-in user holding another
-- organization's job UUID could read its result (analysis output for that
-- organization's denial), and a job could be queued for a denial the caller
-- does not own. Recording the organization lets both be checked directly.
--
-- IF NOT EXISTS / guarded throughout: the snapshot-tail startup path re-runs
-- every migration's raw SQL on a fresh database (see crates/db/src/migrations.rs).
ALTER TABLE recommendation_jobs
    ADD COLUMN IF NOT EXISTS organization_id UUID REFERENCES organizations(id) ON DELETE CASCADE;

-- Every job's denial is NOT NULL and cascades, so the organization is always
-- derivable from denial -> claim.
UPDATE recommendation_jobs j
   SET organization_id = c.organization_id
  FROM denials d
  JOIN claims c ON c.id = d.claim_id
 WHERE d.id = j.denial_id
   AND j.organization_id IS NULL;

ALTER TABLE recommendation_jobs ALTER COLUMN organization_id SET NOT NULL;

CREATE INDEX IF NOT EXISTS idx_recommendation_jobs_organization
    ON recommendation_jobs(organization_id, created_at DESC);
