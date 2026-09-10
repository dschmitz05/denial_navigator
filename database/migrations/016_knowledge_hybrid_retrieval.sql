-- Hybrid retrieval's lexical index and document-date filter support.
CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_content_fts
    ON knowledge_chunks USING GIN (to_tsvector('english', content));

CREATE INDEX IF NOT EXISTS idx_knowledge_documents_effective_dates
    ON knowledge_documents (effective_date, expiration_date)
    WHERE status <> 'archived';

CREATE INDEX IF NOT EXISTS idx_knowledge_documents_jurisdiction
    ON knowledge_documents (lower(COALESCE(metadata->>'jurisdiction', '')))
    WHERE metadata ? 'jurisdiction';
