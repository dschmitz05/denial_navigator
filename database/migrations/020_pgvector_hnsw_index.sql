-- Formalize the pgvector prerequisite and vector index for installations
-- upgraded from a pre-SQLx schema. Runtime use is controlled by
-- VECTOR_SEARCH_ENABLED; when disabled, the RAG service uses lexical search
-- and persists chunks without embeddings.
CREATE EXTENSION IF NOT EXISTS vector;

CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_embedding
    ON knowledge_chunks USING hnsw (embedding vector_cosine_ops);
