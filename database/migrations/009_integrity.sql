-- ============================================================
-- 009 — integrity gaps found in review
--
-- 1. appeals_queue had no constraint preventing two OPEN items on one denial.
--    create_appeal checks then inserts as two separate autocommit statements,
--    so two concurrent submissions could both pass the check and both insert.
--    A partial unique index makes the database enforce what the code intends,
--    which is the only thing a race cannot get around.
--
-- 2. ai_analyses.claim_id carried no foreign key, unlike every sibling table,
--    so an analysis could point at a claim that does not exist.
--
-- 3. knowledge_documents gains payer_name, so the payer filter the analysis
--    path has always SENT can actually be honoured rather than silently
--    dropped. Nullable: a CMS coverage determination belongs to no payer.
--
-- Idempotent.
-- ============================================================

-- Close any existing duplicates before the constraint can reject them.
UPDATE appeals_queue SET outcome_status = 'cancelled',
       notes = COALESCE(notes || ' | ', '') || 'Closed by migration 009: duplicate open item on the same denial.',
       updated_at = NOW()
 WHERE id IN (
    SELECT id FROM (
        SELECT id, ROW_NUMBER() OVER (PARTITION BY denial_id ORDER BY created_at, id) AS n
          FROM appeals_queue
         WHERE outcome_status IS NULL
            OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')
    ) x WHERE n > 1
 );

CREATE UNIQUE INDEX IF NOT EXISTS idx_appeals_queue_one_open_per_denial
    ON appeals_queue (denial_id)
 WHERE outcome_status IS NULL
    OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled');

-- Orphans first, or the constraint cannot be added.
DELETE FROM ai_analyses a
 WHERE NOT EXISTS (SELECT 1 FROM claims c WHERE c.id = a.claim_id);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.table_constraints
         WHERE constraint_name = 'ai_analyses_claim_id_fkey' AND table_name = 'ai_analyses'
    ) THEN
        ALTER TABLE ai_analyses
            ADD CONSTRAINT ai_analyses_claim_id_fkey
            FOREIGN KEY (claim_id) REFERENCES claims(id) ON DELETE CASCADE;
    END IF;
END $$;

ALTER TABLE knowledge_documents ADD COLUMN IF NOT EXISTS payer_name VARCHAR(255);

CREATE INDEX IF NOT EXISTS idx_knowledge_documents_payer_name
    ON knowledge_documents (lower(payer_name)) WHERE payer_name IS NOT NULL;

COMMENT ON COLUMN knowledge_documents.payer_name IS
    'Payer this document governs. NULL means it applies to all (e.g. a CMS LCD).';
