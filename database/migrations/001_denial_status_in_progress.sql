-- ============================================================
-- 001 — denials.status gains 'in_progress'
--
-- Queueing a denial as a corrected claim, a records request, a payer call or
-- a write-off used to set the denial to 'in_appeal', because that was the
-- only non-open status the CHECK allowed. None of that work is an appeal.
-- 'in_progress' says what is actually true: the denial is being worked, but
-- no appeal has been filed.
--
-- Idempotent: safe on a fresh database (where init.sql already carries the
-- value) and on an existing one.
-- ============================================================

ALTER TABLE denials DROP CONSTRAINT IF EXISTS denials_status_check;

ALTER TABLE denials ADD CONSTRAINT denials_status_check
    CHECK (status IN ('open', 'analyzed', 'in_progress', 'in_appeal',
                      'appealed', 'overruled', 'resolved', 'written_off'));

-- Re-home denials parked in 'in_appeal' by a queue item that is not an appeal.
UPDATE denials d
   SET status = 'in_progress', updated_at = NOW()
  FROM appeals_queue aq
 WHERE aq.denial_id = d.id
   AND d.status = 'in_appeal'
   AND aq.resolution_type IS DISTINCT FROM 'appeal_letter'
   AND (aq.outcome_status IS NULL
        OR aq.outcome_status NOT IN ('approved', 'overruled', 'resolved',
                                     'denied_again', 'cancelled'));
