-- FB-13: record what produced each chunk's vector, so a config change that
-- shifts the vector space (a different model, or a different task prefix)
-- can be detected and repaired instead of silently degrading retrieval.
--
-- NULL means "no recorded provenance" (a chunk from before this migration).
-- The API backfills those to the currently configured model/prefix/dimensions
-- on startup rather than this migration guessing at deploy time what the
-- config was when they were embedded — see rag-engine's startup and
-- docs/ARCHITECTURE.md.
ALTER TABLE knowledge_chunks
    ADD COLUMN IF NOT EXISTS embedding_model TEXT,
    ADD COLUMN IF NOT EXISTS embedding_prefix_scheme TEXT,
    ADD COLUMN IF NOT EXISTS embedding_dimensions INTEGER;

-- Used by both the health mismatch count and the re-index batch query, which
-- both select "embedded chunks whose provenance isn't today's config".
CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_embedding_provenance
    ON knowledge_chunks (embedding_model, embedding_prefix_scheme, embedding_dimensions)
    WHERE embedding IS NOT NULL;
