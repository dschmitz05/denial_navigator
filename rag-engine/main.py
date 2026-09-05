"""
RAG Engine Service — Embedding generation, vector store, and semantic retrieval
"""

import logging
import os
import json
from typing import Optional

import httpx
import orjson
from fastapi import FastAPI, HTTPException, BackgroundTasks
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# ── Configuration ──
DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator")
LLAMA_BASE_URL = os.environ.get("LLAMA_BASE_URL", "http://localhost:8080")
EMBEDDING_MODEL = os.environ.get("EMBEDDING_MODEL", "nomic-embed-text")
# Embeddings do NOT come from LLAMA_BASE_URL. That server runs the chat model
# and answers /v1/embeddings with:
#   501 "This server does not support embeddings. Start it with `--embeddings`"
# Ollama on the same host serves nomic-embed-text (768 dims), which matches the
# vector(768) column and its ivfflat cosine index.
EMBED_BASE_URL = os.environ.get("EMBED_BASE_URL", "http://10.10.10.98:11434")
CHUNK_CHARS = int(os.environ.get("CHUNK_CHARS", "1500"))
CHUNK_OVERLAP = int(os.environ.get("CHUNK_OVERLAP", "200"))
MIN_SIMILARITY = float(os.environ.get("MIN_SIMILARITY", "0.25"))
API_BASE = os.environ.get("API_BASE", "http://localhost:8000")

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("ragengine")

app = FastAPI(
    title="RAG Engine Service",
    description="Vector search over payer policies and medical guidelines",
    version="1.0.0",
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


# ── Embedding Generation ──
async def generate_embeddings(texts: list[str], model: str = None) -> list[list[float]]:
    """Generate embeddings using llama.cpp OpenAI-compatible API"""
    model = model or EMBEDDING_MODEL
    all_embeddings = []

    async with httpx.AsyncClient(timeout=60) as client:
        for text in texts:
            resp = await client.post(
                f"{EMBED_BASE_URL}/v1/embeddings",
                json={"model": model, "input": text},
            )
            resp.raise_for_status()
            data = resp.json()
            embedding = data.get("data", [])[0].get("embedding") if data.get("data") else None
            if embedding:
                all_embeddings.append(embedding)
            else:
                raise ValueError(f"No embedding returned for text: {text[:50]}...")

    return all_embeddings


# ── Vector Store Operations ──
async def store_embeddings(document_id: str, chunks: list[dict], embeddings: list[list[float]]):
    """Store text chunks with embeddings in the database"""
    payload = {
        "document_id": document_id,
        "chunks": chunks,
        "embeddings": embeddings,
    }
    async with httpx.AsyncClient(timeout=60) as client:
        resp = await client.post(f"{API_BASE}/api/v1/knowledge/embed", json=payload)
        resp.raise_for_status()
        return resp.json()


async def _db():
    """One asyncpg connection. The vector store lives here, not behind the gateway."""
    import asyncpg
    return await asyncpg.connect(DATABASE_URL)


def chunk_text(text: str, size: int = None, overlap: int = None) -> list[str]:
    """Split on paragraph boundaries where possible, with overlap for context."""
    size = size or CHUNK_CHARS
    overlap = overlap or CHUNK_OVERLAP
    text = (text or "").strip()
    if not text:
        return []
    if len(text) <= size:
        return [text]

    chunks, start = [], 0
    while start < len(text):
        end = min(start + size, len(text))
        if end < len(text):
            # Prefer a paragraph break, then a sentence end, then a space.
            for sep in ("\n\n", ". ", "\n", " "):
                cut = text.rfind(sep, start + size // 2, end)
                if cut != -1:
                    end = cut + len(sep)
                    break
        piece = text[start:end].strip()
        if piece:
            chunks.append(piece)
        if end >= len(text):
            break
        start = max(end - overlap, start + 1)
    return chunks


async def search_similar(query: str, top_k: int = 5, filters: dict = None) -> list[dict]:
    """Real semantic search: embed the query, then cosine-rank chunks in pgvector.

    This used to POST back to the api-gateway's /knowledge/search, which was a
    `content ILIKE '%query%'` lookup with `1.0 as similarity_score` hardcoded -
    so the whole RAG loop was circular and the scores were fabricated.
    """
    filters = filters or {}
    embeddings = await generate_embeddings([query])
    if not embeddings:
        logger.warning("No embedding produced for query; returning no results")
        return []
    vector = "[" + ",".join(str(float(x)) for x in embeddings[0]) + "]"

    sql = """
        SELECT kc.id, kc.knowledge_document_id, kc.chunk_index, kc.content,
               kc.token_count, kc.metadata,
               kd.title AS document_title, kd.source_type,
               1 - (kc.embedding <=> $1::vector) AS similarity_score
        FROM knowledge_chunks kc
        JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id
        WHERE kc.embedding IS NOT NULL
          -- Archiving removes a document's chunks, so this is belt and
          -- braces: a superseded policy must never steer an analysis.
          AND kd.status <> 'archived'
    """
    params = [vector]
    if filters.get("source_type"):
        params.append(filters["source_type"])
        sql += f" AND kd.source_type = ${len(params)}"
    params.append(MIN_SIMILARITY)
    sql += f" AND 1 - (kc.embedding <=> $1::vector) >= ${len(params)}"
    params.append(top_k)
    sql += f" ORDER BY kc.embedding <=> $1::vector LIMIT ${len(params)}"

    conn = await _db()
    try:
        rows = await conn.fetch(sql, *params)
    finally:
        await conn.close()

    results = []
    for r in rows:
        d = dict(r)
        d["similarity_score"] = float(d["similarity_score"])
        d["id"] = str(d["id"])
        d["knowledge_document_id"] = str(d["knowledge_document_id"])
        results.append(d)
    logger.info(f"Vector search '{query[:40]}...' -> {len(results)} chunks")
    return results


# ── Prompt Construction ──
# Delegate to prompts module
import sys
sys.path.insert(0, os.path.dirname(__file__))
from prompts.denial_analysis import build_denial_prompt as _build_denial_prompt

async def build_denial_analysis_prompt(**kwargs) -> dict:
    """Build the system and user prompts for denial analysis (delegates to prompts module)"""
    return await _build_denial_prompt(**kwargs)


# ── Pydantic Models ──
class EmbedRequest(BaseModel):
    document_id: str
    chunks: list[dict]
    embeddings: list[list[float]]

class SearchRequest(BaseModel):
    query: str
    top_k: int = 5
    filters: dict = {}

class SearchResponse(BaseModel):
    results: list[dict]
    query: str
    top_k: int

class PromptRequest(BaseModel):
    claim_id: str
    payer_name: str
    cpt_code: str
    icd10_code: str
    cagc: str
    carc_code: str
    carc_definition: str
    rarc_code: str
    rarc_definition: str
    retrieved_policies: list[str]

class PromptResponse(BaseModel):
    system: str
    user: str


# ── Routes ──
@app.get("/health")
async def health():
    return {"status": "healthy", "embedding_model": EMBEDDING_MODEL,
            "llama_url": LLAMA_BASE_URL, "embed_url": EMBED_BASE_URL}

@app.post("/embed", response_model=SearchResponse)
async def store_document_embeddings(request: EmbedRequest):
    """Deprecated. This was a stub that reported success without storing anything.

    Use /ingest-document, which actually chunks, embeds and writes to pgvector.
    """
    raise HTTPException(
        status_code=410,
        detail="/embed was a no-op stub and has been removed; use /ingest-document",
    )

@app.post("/search", response_model=SearchResponse)
async def search_knowledge(request: SearchRequest):
    """Search knowledge base for relevant policy documents"""
    logger.info(f"Searching for: {request.query[:50]}...")
    results = await search_similar(request.query, request.top_k, request.filters)
    return SearchResponse(results=results, query=request.query, top_k=request.top_k)


class IngestDocumentRequest(BaseModel):
    document_id: str
    content: str


@app.post("/ingest-document")
async def ingest_document(request: IngestDocumentRequest):
    """Chunk, embed and store a document's text, then mark it indexed.

    This is the step that never existed: documents were created as metadata
    rows with no text, so the knowledge base held 0 chunks and every analysis
    ran with no policy context.
    """
    chunks = chunk_text(request.content)
    if not chunks:
        raise HTTPException(status_code=400, detail="Document content is empty")

    logger.info(f"Embedding {len(chunks)} chunks for document {request.document_id}")
    vectors = await generate_embeddings(chunks)
    if len(vectors) != len(chunks):
        raise HTTPException(
            status_code=502,
            detail=f"Embedding backend returned {len(vectors)} vectors for {len(chunks)} chunks",
        )

    conn = await _db()
    try:
        async with conn.transaction():
            # Re-ingesting a document replaces its chunks rather than duplicating them.
            await conn.execute(
                "DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1",
                request.document_id,
            )
            for i, (chunk, vec) in enumerate(zip(chunks, vectors)):
                await conn.execute(
                    """
                    INSERT INTO knowledge_chunks
                        (knowledge_document_id, chunk_index, content, embedding, metadata, token_count)
                    VALUES ($1, $2, $3, $4::vector, $5::jsonb, $6)
                    """,
                    request.document_id, i, chunk,
                    "[" + ",".join(str(float(x)) for x in vec) + "]",
                    json.dumps({"chars": len(chunk)}),
                    len(chunk.split()),
                )
            await conn.execute(
                "UPDATE knowledge_documents SET status = 'indexed', updated_at = NOW() WHERE id = $1",
                request.document_id,
            )
    finally:
        await conn.close()

    return {
        "status": "indexed",
        "document_id": request.document_id,
        "chunks": len(chunks),
        "embedding_model": EMBEDDING_MODEL,
        "dimensions": len(vectors[0]),
    }

@app.post("/prompt/denial-analysis")
async def build_prompt(request: PromptRequest):
    """Build a denial analysis prompt"""
    # build_denial_analysis_prompt takes **kwargs only, so these MUST be
    # passed by keyword. Calling it positionally raised
    # "takes 0 positional arguments but 10 were given" on every request,
    # which is why the AI button did nothing.
    prompt = await build_denial_analysis_prompt(
        claim_id=request.claim_id,
        payer_name=request.payer_name,
        cpt_code=request.cpt_code,
        icd10_code=request.icd10_code,
        cagc=request.cagc,
        carc_code=request.carc_code,
        carc_definition=request.carc_definition,
        rarc_code=request.rarc_code,
        rarc_definition=request.rarc_definition,
        retrieved_policies=request.retrieved_policies,
    )
    return PromptResponse(**prompt)

@app.post("/embed/generate")
async def generate_and_store(texts: list[str]):
    """Generate embeddings for a list of texts and return them"""
    logger.info(f"Generating embeddings for {len(texts)} texts")
    embeddings = await generate_embeddings(texts)
    return {"embeddings_count": len(embeddings), "model": EMBEDDING_MODEL}
