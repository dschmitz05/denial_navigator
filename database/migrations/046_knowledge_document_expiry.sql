-- FB-14: knowledge-document expiry alerts and supersession.
--
-- A document already stops governing retrieval once its expiration_date
-- passes (rag-engine's date-of-service filter), but nothing told anyone that
-- had happened, or linked the replacement that should have superseded it.
ALTER TABLE knowledge_documents
    ADD COLUMN IF NOT EXISTS superseded_by UUID REFERENCES knowledge_documents(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_knowledge_documents_superseded_by
    ON knowledge_documents (superseded_by) WHERE superseded_by IS NOT NULL;

-- Used by both the expiry summary and the Knowledge Base filter.
CREATE INDEX IF NOT EXISTS idx_knowledge_documents_expiration_active
    ON knowledge_documents (expiration_date) WHERE status <> 'archived' AND expiration_date IS NOT NULL;
