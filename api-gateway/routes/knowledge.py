"""API Gateway — Knowledge Base routes"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query, UploadFile, File
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services import RAGEngineClient

logger = logging.getLogger("api_gateway.knowledge")

router = APIRouter()
rag_client = RAGEngineClient()

# A PDF is identified by its magic bytes, not by filename or the browser's
# content-type guess - both of which are trivially wrong.
PDF_MAGIC = b"%PDF"


def _extract_pdf_text(raw: bytes) -> tuple[str, int]:
    """Extract text from a PDF. Returns (text, page_count).

    Raises HTTPException with an actionable message rather than letting an
    empty extraction be indexed as a silently useless document.
    """
    import io as _io

    try:
        from pypdf import PdfReader
    except ImportError:  # pragma: no cover - dependency is pinned
        raise HTTPException(
            status_code=500,
            detail="PDF support requires the pypdf package to be installed",
        )

    try:
        reader = PdfReader(_io.BytesIO(raw))
    except Exception as e:
        raise HTTPException(status_code=400, detail=f"Could not read PDF: {e}")

    if getattr(reader, "is_encrypted", False):
        # An empty-password decrypt covers the common "protected but not
        # password-locked" case; anything else is a real block.
        try:
            if reader.decrypt("") == 0:
                raise HTTPException(
                    status_code=400,
                    detail="This PDF is password protected. Remove the password and re-upload.",
                )
        except HTTPException:
            raise
        except Exception:
            raise HTTPException(
                status_code=400,
                detail="This PDF is encrypted and cannot be read.",
            )

    pages = []
    for page in reader.pages:
        try:
            pages.append(page.extract_text() or "")
        except Exception as e:
            logger.warning(f"Page extraction failed: {e}")
            pages.append("")

    text = "\n\n".join(p.strip() for p in pages if p and p.strip()).strip()

    if len(text) < 50:
        # Almost certainly a scanned document: pypdf extracts glyphs, not
        # pixels. Indexing this would create a document with no retrievable
        # content, which is worse than refusing it.
        raise HTTPException(
            status_code=422,
            detail=(
                f"No extractable text found in this PDF ({len(reader.pages)} pages). "
                "It is most likely a scan or image-only PDF, which needs OCR. "
                "Upload a text-based PDF, or paste the text directly."
            ),
        )

    return text, len(reader.pages)


def _decode_upload(raw: bytes, filename: str) -> tuple[str, dict]:
    """Turn an uploaded file into indexable text plus metadata."""
    if raw.startswith(PDF_MAGIC):
        text, page_count = _extract_pdf_text(raw)
        return text, {"format": "pdf", "pages": page_count, "chars": len(text)}

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        raise HTTPException(
            status_code=400,
            detail=f"{filename} is neither a PDF nor UTF-8 text. "
                   "Supported: .pdf, .txt, .md",
        )
    if not text.strip():
        raise HTTPException(status_code=400, detail="File is empty")
    return text, {"format": "text", "chars": len(text)}


class DocumentCreate(BaseModel):
    title: str
    source_type: str
    payer_id: Optional[str] = None
    effective_date: Optional[str] = None
    # The whole point of a knowledge base. Without this a document was created
    # as a metadata row with no text and stayed 'pending' forever.
    content: Optional[str] = None


class DocumentContent(BaseModel):
    content: str


class EmbedRequest(BaseModel):
    document_id: str
    chunks: list[dict]
    embeddings: list[list[float]]


class SearchRequest(BaseModel):
    query: str
    top_k: int = 5
    filters: dict = {}


@router.get("/knowledge/documents", response_model=list[dict])
async def list_documents(
    source_type: str = Query(None),
    status: str = Query(None),
    limit: int = Query(50),
):
    """List knowledge documents"""
    async with get_connection() as conn:
        # chunk_count makes "indexed" verifiable at a glance: a document with
        # 0 chunks contributes nothing to retrieval no matter what status says.
        query = """
            SELECT kd.*,
                   (SELECT COUNT(*) FROM knowledge_chunks kc
                     WHERE kc.knowledge_document_id = kd.id) AS chunk_count
            FROM knowledge_documents kd WHERE 1=1
        """
        params = []
        where_count = 0

        if source_type:
            query += f" AND kd.source_type = ${where_count + 1}"
            params.append(source_type)
            where_count += 1
        if status:
            query += f" AND kd.status = ${where_count + 1}"
            params.append(status)
            where_count += 1

        query += f" ORDER BY kd.created_at DESC LIMIT ${where_count + 1}"
        params.append(limit)

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.post("/knowledge/documents", response_model=dict, status_code=201)
async def create_document(doc: DocumentCreate):
    """Create a knowledge document record"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO knowledge_documents
                (title, source_type, payer_id, effective_date, status)
            VALUES ($1, $2, $3, $4, 'pending')
            RETURNING *
            """,
            doc.title, doc.source_type, doc.payer_id, doc.effective_date,
        )
        created = dict(row)

    if doc.content and doc.content.strip():
        try:
            result = await rag_client.ingest_document(str(created["id"]), doc.content)
            created["chunks_indexed"] = result.get("chunks", 0)
            created["status"] = "indexed"
        except Exception as e:
            logger.error(f"Indexing failed for {created['id']}: {e}")
            async with get_connection() as conn:
                await conn.execute(
                    "UPDATE knowledge_documents SET status = 'error' WHERE id = $1",
                    created["id"],
                )
            created["status"] = "error"
            created["index_error"] = str(e)

    return created


@router.post("/knowledge/documents/{document_id}/content", response_model=dict)
async def add_document_content(document_id: str, body: DocumentContent):
    """Attach text to an existing document and index it into pgvector."""
    async with get_connection() as conn:
        exists = await conn.fetchval(
            "SELECT 1 FROM knowledge_documents WHERE id = $1", document_id
        )
    if not exists:
        raise HTTPException(status_code=404, detail="Document not found")

    try:
        result = await rag_client.ingest_document(document_id, body.content)
    except Exception as e:
        async with get_connection() as conn:
            await conn.execute(
                "UPDATE knowledge_documents SET status = 'error' WHERE id = $1", document_id
            )
        raise HTTPException(status_code=502, detail=f"Indexing failed: {e}")
    return result


@router.post("/knowledge/documents/upload", response_model=dict, status_code=201)
async def upload_document(
    file: UploadFile = File(...),
    title: str = Query(None),
    source_type: str = Query("payer_policy"),
):
    """Upload a policy document (PDF, plain text or markdown) and index it."""
    raw = await file.read()
    if not raw:
        raise HTTPException(status_code=400, detail="File is empty")

    content, meta = _decode_upload(raw, file.filename or "upload")

    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO knowledge_documents
                (title, source_type, status, mime_type, file_size_bytes, metadata)
            VALUES ($1, $2, 'pending', $3, $4, $5::jsonb)
            RETURNING *
            """,
            title or file.filename, source_type,
            "application/pdf" if meta["format"] == "pdf" else (file.content_type or "text/plain"),
            len(raw), json.dumps(meta),
        )
        created = dict(row)

    try:
        result = await rag_client.ingest_document(str(created["id"]), content)
    except Exception as e:
        async with get_connection() as conn:
            await conn.execute(
                "UPDATE knowledge_documents SET status = 'error' WHERE id = $1", created["id"]
            )
        raise HTTPException(status_code=502, detail=f"Indexing failed: {e}")

    created["chunks_indexed"] = result.get("chunks", 0)
    created["status"] = "indexed"
    created["extracted"] = meta
    return created


@router.delete("/knowledge/documents/{document_id}", response_model=dict)
async def delete_document(
    document_id: str,
    purge: bool = Query(False, description="Permanently delete the record instead of archiving it"),
):
    """Retire a knowledge document so it stops feeding denial analysis.

    Default is ARCHIVE: the document row is kept for audit, but its chunks are
    deleted so it can no longer be retrieved. That distinction matters here -
    a superseded payer policy that is still searchable will keep steering the
    LLM with out-of-date rules, which is worse than having no policy at all.

    `?purge=true` removes the record entirely (chunks cascade).
    """
    async with get_connection() as conn:
        doc = await conn.fetchrow(
            "SELECT id, title, status FROM knowledge_documents WHERE id = $1", document_id
        )
        if not doc:
            raise HTTPException(status_code=404, detail="Document not found")

        chunk_count = await conn.fetchval(
            "SELECT COUNT(*) FROM knowledge_chunks WHERE knowledge_document_id = $1",
            document_id,
        )

        if purge:
            await conn.execute("DELETE FROM knowledge_documents WHERE id = $1", document_id)
            action = "purged"
        else:
            async with conn.transaction():
                await conn.execute(
                    "DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1", document_id
                )
                await conn.execute(
                    """UPDATE knowledge_documents
                          SET status = 'archived', updated_at = NOW()
                        WHERE id = $1""",
                    document_id,
                )
            action = "archived"

    logger.info(f"{action} knowledge document {document_id} ({chunk_count} chunks removed)")
    return {
        "status": action,
        "document_id": document_id,
        "title": doc["title"],
        "chunks_removed": chunk_count,
    }


@router.post("/knowledge/search", response_model=dict)
async def search_knowledge(request: SearchRequest):
    """Semantic search over indexed policy chunks.

    This was `content ILIKE '%query%'` with `1.0 as similarity_score`
    hardcoded - never touching pgvector, and reporting a fabricated 100% match
    on every row. It now delegates to the rag-engine, which embeds the query
    and ranks by real cosine similarity.
    """
    try:
        result = await rag_client.search(
            request.query, top_k=request.top_k, filters=request.filters
        )
    except Exception as e:
        logger.error(f"Vector search failed: {e}")
        raise HTTPException(status_code=502, detail=f"Search failed: {e}")

    return {
        "results": result.get("results", []),
        "query": request.query,
        "top_k": request.top_k,
    }


@router.get("/knowledge/documents/{document_id}", response_model=dict)
async def get_document(document_id: str):
    """Get a knowledge document and its full text content (all chunks)."""
    async with get_connection() as conn:
        doc = await conn.fetchrow(
            """
            SELECT kd.*,
                   (SELECT COUNT(*) FROM knowledge_chunks kc
                     WHERE kc.knowledge_document_id = kd.id) AS chunk_count
            FROM knowledge_documents kd WHERE kd.id = $1
            """,
            document_id,
        )
        if not doc:
            raise HTTPException(status_code=404, detail="Document not found")

        chunks = await conn.fetch(
            """
            SELECT chunk_index, content, token_count
            FROM knowledge_chunks
            WHERE knowledge_document_id = $1
            ORDER BY chunk_index
            """,
            document_id,
        )

    return {
        "id": str(doc["id"]),
        "title": doc["title"],
        "source_type": doc["source_type"],
        "status": doc["status"],
        "chunk_count": doc["chunk_count"],
        "created_at": doc["created_at"].isoformat() if doc["created_at"] else None,
        "content": "\n\n".join(r["content"] for r in chunks) if chunks else "(no content — indexing may still be in progress)",
        "chunks": [dict(r) for r in chunks],
    }
