-- ============================================================
-- 006 — appeal filing deadlines
--
-- denials.appeal_deadline existed and was never populated by anything. The
-- dashboard's "Priority Denials" panel selects on it, so it was permanently
-- empty and read "No urgent denials" regardless of what was outstanding -
-- reassurance rather than an empty state.
--
-- An 835 does not carry a filing deadline: a remittance states the payer's
-- adjudication, not your window to contest it. The deadline is a function of
-- the payer's own rules, so it is configuration, and it lives here.
--
-- The row with payer_name '*' is the default for payers without their own
-- entry. 90 days is the common Medicare figure; commercial payers vary from
-- 60 to 365, which is exactly why this is a table and not a constant.
--
-- Idempotent.
-- ============================================================

CREATE TABLE IF NOT EXISTS payer_appeal_policies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    -- Matched case-insensitively against claims.payer_name. '*' is the default.
    payer_name VARCHAR(255) NOT NULL,
    appeal_window_days INTEGER NOT NULL CHECK (appeal_window_days BETWEEN 1 AND 3650),
    notes TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_payer_appeal_policies_name
    ON payer_appeal_policies (lower(payer_name));

INSERT INTO payer_appeal_policies (payer_name, appeal_window_days, notes)
SELECT '*', 90, 'Default filing window for payers without a specific policy.'
 WHERE NOT EXISTS (SELECT 1 FROM payer_appeal_policies WHERE payer_name = '*');

-- One definition of the deadline, used by ingestion, by the backfill below,
-- and by the recompute that runs when a window is edited. Three copies of this
-- arithmetic would be three chances to disagree.
CREATE OR REPLACE FUNCTION appeal_deadline_for(p_payer TEXT, p_base DATE)
RETURNS DATE
LANGUAGE sql
STABLE
AS $$
    SELECT COALESCE(p_base, CURRENT_DATE) + (
        COALESCE(
            (SELECT appeal_window_days FROM payer_appeal_policies
              WHERE lower(payer_name) = lower(COALESCE(p_payer, '')) LIMIT 1),
            (SELECT appeal_window_days FROM payer_appeal_policies
              WHERE payer_name = '*' LIMIT 1),
            90
        ) || ' days'
    )::INTERVAL;
$$;

COMMENT ON FUNCTION appeal_deadline_for IS
    'Filing deadline for a denial: the payer window, else the default, applied to the remittance date.';

-- Backfill denials that predate this. Only ones still open: a closed denial's
-- deadline is history and inventing one would put finished work back on the
-- dashboard's urgent list.
UPDATE denials d
   SET appeal_deadline = appeal_deadline_for(c.payer_name, d.denial_date),
       updated_at = NOW()
  FROM claims c
 WHERE c.id = d.claim_id
   AND d.appeal_deadline IS NULL
   AND d.status IN ('open', 'analyzed');
