"""
Vector Store — pgvector operations for knowledge document chunks
"""

import os
from typing import Optional

DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator")


class VectorStore:
    """Interface for pgvector operations"""

    def __init__(self, db_url: str = None):
        self.db_url = db_url or DATABASE_URL
        self._connection = None

    async def connect(self):
        """Establish database connection"""
        import asyncpg
        self._connection = await asyncpg.connect(self.db_url)

    async def close(self):
        """Close database connection"""
        if self._connection:
            await self._connection.close()

    async def insert_chunk(
        self,
        document_id: str,
        chunk_index: int,
        content: str,
        embedding: list[float],
        metadata: dict = None,
        token_count: int = 0,
    ):
        """Insert a single knowledge chunk with embedding"""
        query = """
            INSERT INTO knowledge_chunks
                (knowledge_document_id, chunk_index, content, embedding, metadata, token_count)
            VALUES ($1, $2, $3, $4, $5, $6)
        """
        await self._connection.execute(
            query, document_id, chunk_index, content, str(embedding),
            metadata or {}, token_count
        )

    async def insert_chunks(self, chunks: list[dict]):
        """Batch insert knowledge chunks"""
        for chunk in chunks:
            await self.insert_chunk(
                document_id=chunk["document_id"],
                chunk_index=chunk["chunk_index"],
                content=chunk["content"],
                embedding=chunk["embedding"],
                metadata=chunk.get("metadata"),
                token_count=chunk.get("token_count", 0),
            )

    async def search(
        self,
        embedding: list[float],
        top_k: int = 5,
        min_score: float = 0.5,
        filters: dict = None,
    ) -> list[dict]:
        """Search for similar chunks using cosine similarity"""
        query = """
            SELECT
                kc.id,
                kc.knowledge_document_id,
                kc.chunk_index,
                kc.content,
                kc.token_count,
                kc.metadata,
                (kc.embedding <=> $1) AS similarity_score
            FROM knowledge_chunks kc
            WHERE kc.embedding IS NOT NULL
            ORDER BY kc.embedding <=> $1
            LIMIT $2
        """
        rows = await self._connection.fetch(query, str(embedding), top_k)
        return [dict(row) for row in rows]

    async def update_document_status(self, document_id: str, status: str):
        """Update document processing status"""
        await self._connection.execute(
            "UPDATE knowledge_documents SET status = $1 WHERE id = $2",
            status, document_id,
        )

    async def get_document_chunks(self, document_id: str) -> list[dict]:
        """Get all chunks for a document"""
        rows = await self._connection.fetch(
            "SELECT * FROM knowledge_chunks WHERE knowledge_document_id = $1 ORDER BY chunk_index",
            document_id,
        )
        return [dict(row) for row in rows]

    async def count_chunks(self) -> int:
        """Count total chunks in vector store"""
        result = await self._connection.fetchval("SELECT COUNT(*) FROM knowledge_chunks")
        return result or 0
