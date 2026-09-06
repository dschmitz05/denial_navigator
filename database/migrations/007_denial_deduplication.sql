-- ============================================================
-- 007 — stop denials duplicating on re-ingest
--
-- ingestion_log records a sha256 of every file and nothing ever compared it to
-- anything. Claims survived a re-upload because they upsert on claim_number;
-- denials have no unique key, so every re-ingest inserted a fresh copy. On this
-- deployment one file uploaded twice took 8 denials to 16 - doubling denied
-- dollars, CARC counts and queue volume, and letting two people work the same
-- denial.
--
-- Two defences, because they fail differently:
--   * The API refuses a file whose hash it has already stored (see
--     routes/ingestion.py). Catches the whole-file case with a clear message.
--   * This index catches the rest - overlapping claims arriving in DIFFERENT
--     files, which a hash cannot detect.
--
-- The key is the adjustment as the payer described it. Two rows identical in
-- all of these are the same adjustment counted twice; COALESCE is needed
-- because NULLs never conflict in a unique index, which would defeat it for
-- claim-level adjustments that carry no service line.
--
-- Idempotent.
-- ============================================================

-- Collapse what is already duplicated, keeping the earliest of each set.
--
-- An earlier version simply skipped any duplicate carrying an analysis or a
-- queue item, to avoid discarding work - but on this deployment BOTH copies
-- had been analysed and worked, so it skipped everything and the index below
-- could not be built. Re-pointing that work onto the surviving row loses
-- nothing and leaves one denial where there were two.
CREATE TEMP TABLE dedupe_map AS
WITH ranked AS (
    SELECT id, created_at,
           FIRST_VALUE(id) OVER w AS keeper,
           ROW_NUMBER() OVER w AS copy_number
      FROM denials
    WINDOW w AS (
        PARTITION BY claim_id,
                     COALESCE(service_line_number, -1),
                     COALESCE(cpt_code, ''),
                     cagc,
                     COALESCE(carc_code, ''),
                     charge_amount,
                     adjustment_amount,
                     COALESCE(denial_date, '1900-01-01'::date)
        ORDER BY created_at, id
    )
)
SELECT id AS duplicate_id, keeper FROM ranked WHERE copy_number > 1;

UPDATE ai_analyses a SET denial_id = m.keeper
  FROM dedupe_map m WHERE a.denial_id = m.duplicate_id;

UPDATE appeals_queue q SET denial_id = m.keeper
  FROM dedupe_map m WHERE q.denial_id = m.duplicate_id;

-- Re-pointing can leave a denial with more than one OPEN queue item, which the
-- application assumes cannot happen. Keep the earliest and close the rest,
-- saying why, rather than leaving a state nothing else expects.
UPDATE appeals_queue SET outcome_status = 'cancelled',
       notes = COALESCE(notes || ' | ', '') || 'Closed by deduplication: this denial was a duplicate record.',
       updated_at = NOW()
 WHERE id IN (
    SELECT id FROM (
        SELECT id, ROW_NUMBER() OVER (PARTITION BY denial_id ORDER BY created_at) AS n
          FROM appeals_queue
         WHERE outcome_status IS NULL
            OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')
    ) x WHERE n > 1
 );

DELETE FROM denials WHERE id IN (SELECT duplicate_id FROM dedupe_map);

DROP TABLE dedupe_map;

CREATE UNIQUE INDEX IF NOT EXISTS idx_denials_natural_key ON denials (
    claim_id,
    COALESCE(service_line_number, -1),
    COALESCE(cpt_code, ''),
    cagc,
    COALESCE(carc_code, ''),
    charge_amount,
    adjustment_amount,
    COALESCE(denial_date, '1900-01-01'::date)
);
