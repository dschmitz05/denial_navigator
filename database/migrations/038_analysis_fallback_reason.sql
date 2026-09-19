-- FB-05: why an analysis is degraded, so a silent fallback is visible.
--   llm_error        the model call failed; deterministic rules were used
--   retrieval_error  the policy search failed; the model saw no evidence
--   no_evidence      the policy search found nothing relevant
ALTER TABLE ai_analyses ADD COLUMN IF NOT EXISTS fallback_reason VARCHAR(20)
    CHECK (fallback_reason IN ('llm_error', 'retrieval_error', 'no_evidence'));
CREATE INDEX IF NOT EXISTS idx_ai_analyses_created_fallback
    ON ai_analyses (created_at DESC, fallback_reason);
