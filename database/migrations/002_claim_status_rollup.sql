-- ============================================================
-- 002 — backfill claims.status from the state of their denials
--
-- Ingestion set a claim to 'denied' / 'partially_paid' and nothing ever moved
-- it again, so a claim whose denials had all been resolved through Appeals or
-- the Worklist still read 'denied' on the Claims tab. The API now derives this
-- on every denial status change (api_gateway/services/claim_status.py); this
-- brings existing rows in line with that rule.
--
-- Idempotent: it computes the same answer every time it runs.
-- ============================================================

UPDATE claims c
   SET status = CASE
           WHEN s.open_denials = 0 THEN 'resolved'
           WHEN c.total_paid > 0   THEN 'partially_paid'
           ELSE 'denied'
       END,
       updated_at = NOW()
  FROM (
       SELECT d.claim_id,
              COUNT(*) AS total,
              COUNT(*) FILTER (
                  WHERE d.status NOT IN ('appealed', 'overruled', 'resolved', 'written_off')
              ) AS open_denials
         FROM denials d
        GROUP BY d.claim_id
       ) s
 WHERE c.id = s.claim_id
   AND c.status IS DISTINCT FROM CASE
           WHEN s.open_denials = 0 THEN 'resolved'
           WHEN c.total_paid > 0   THEN 'partially_paid'
           ELSE 'denied'
       END;
