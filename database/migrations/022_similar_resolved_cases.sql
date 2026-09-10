-- Supports non-PHI similar-case retrieval, restricted to known successful
-- resubmission outcomes.
CREATE INDEX IF NOT EXISTS idx_feedback_loop_paid_created
    ON feedback_loop(created_at DESC)
    WHERE was_paid_on_resubmit IS TRUE;
