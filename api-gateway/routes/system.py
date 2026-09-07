"""API Gateway — live system health.

The Settings page used to hardcode "✅ Running" for every service, which is
worse than showing nothing: it reports health it never checked, so the one
time a service is actually down the page still says it is fine.

Everything is probed from the gateway rather than the browser, because the
sibling services are only reachable inside the Docker network - the browser
can see port 8000 and nothing else.
"""

import asyncio
import logging
import os
import time
from typing import Optional

import httpx
from fastapi import APIRouter

from api_gateway.services.db import get_connection

logger = logging.getLogger("api_gateway.system")

router = APIRouter()

EDIPARSER_SERVICE_URL = os.environ.get("EDIPARSER_SERVICE_URL", "http://ediparser:8000")
RAG_ENGINE_URL = os.environ.get("RAG_ENGINE_URL", "http://rag-engine:8000")
LLM_SERVICE_URL = os.environ.get("LLM_SERVICE_URL", "http://llm-service:8000")
LLAMA_BASE_URL = os.environ.get("LLAMA_BASE_URL", "http://10.10.10.98:8080")
EMBED_BASE_URL = os.environ.get("EMBED_BASE_URL", "http://10.10.10.98:11434")

# Short: this is a status panel, not a readiness gate. A service that cannot
# answer in three seconds is degraded from the user's point of view anyway.
PROBE_TIMEOUT = 3.0


async def _timed(name: str, essential: bool, probe) -> dict:
    """Run one probe, never raising, always reporting how long it took."""
    started = time.monotonic()
    try:
        detail = await asyncio.wait_for(probe(), timeout=PROBE_TIMEOUT)
        status = "ok"
    except asyncio.TimeoutError:
        detail, status = f"No response within {PROBE_TIMEOUT:g}s", "down"
    except Exception as e:
        # The class name alone is useless to a reader ("ConnectError"); the
        # message says which host refused.
        detail, status = f"{type(e).__name__}: {e}" if str(e) else type(e).__name__, "down"
    return {
        "name": name,
        "status": status,
        "essential": essential,
        "detail": detail,
        "latency_ms": int((time.monotonic() - started) * 1000),
    }


async def _http_probe(url: str, expect_json_key: Optional[str] = None) -> str:
    async with httpx.AsyncClient(timeout=PROBE_TIMEOUT) as client:
        resp = await client.get(url)
        resp.raise_for_status()
        if expect_json_key:
            data = resp.json()
            value = data.get(expect_json_key)
            if value is not None:
                return f"{expect_json_key}: {value}"
        return f"HTTP {resp.status_code}"


async def _postgres_probe() -> str:
    async with get_connection() as conn:
        await conn.fetchval("SELECT 1")
        claims = await conn.fetchval("SELECT COUNT(*) FROM claims")
        chunks = await conn.fetchval("SELECT COUNT(*) FROM knowledge_chunks")
    return f"{claims} claims, {chunks} indexed policy chunks"


async def _llama_probe() -> str:
    """Ask what is loaded, not just whether the port answers.

    A llama.cpp server with no model loaded still answers /health, and every
    analysis against it fails - so the model list is the honest signal.
    """
    async with httpx.AsyncClient(timeout=PROBE_TIMEOUT) as client:
        resp = await client.get(f"{LLAMA_BASE_URL}/v1/models")
        resp.raise_for_status()
        models = [m.get("id") for m in resp.json().get("data", []) if m.get("id")]
    if not models:
        raise RuntimeError("server is up but no model is loaded")
    return ", ".join(models[:3]) + (f" (+{len(models) - 3} more)" if len(models) > 3 else "")


async def _embeddings_probe() -> str:
    """Ask the embedding backend what it serves, whichever backend it is.

    This used to call Ollama's /api/tags, which llama.cpp does not serve - so
    pointing EMBED_BASE_URL at a llama-server made the card report the backend
    as down while embeddings worked perfectly. Both speak the OpenAI-compatible
    /v1/models, so that is tried first and Ollama's own endpoint second.
    """
    async with httpx.AsyncClient(timeout=PROBE_TIMEOUT) as client:
        models = []
        try:
            resp = await client.get(f"{EMBED_BASE_URL}/v1/models")
            resp.raise_for_status()
            models = [m.get("id") for m in resp.json().get("data", []) if m.get("id")]
        except Exception:
            resp = await client.get(f"{EMBED_BASE_URL}/api/tags")
            resp.raise_for_status()
            models = [m.get("name") for m in resp.json().get("models", []) if m.get("name")]

    if not models:
        raise RuntimeError("reachable but no embedding model is available")
    return ", ".join(models[:3]) + (f" (+{len(models) - 3} more)" if len(models) > 3 else "")


@router.get("/system/health")
async def system_health():
    """Probe every dependency and report what is actually true right now."""
    probes = [
        _timed("PostgreSQL + pgvector", True, _postgres_probe),
        _timed("EDI Parser", True, lambda: _http_probe(f"{EDIPARSER_SERVICE_URL}/health", "status")),
        _timed("RAG Engine", True, lambda: _http_probe(f"{RAG_ENGINE_URL}/health", "embedding_model")),
        _timed("LLM Service", True, lambda: _http_probe(f"{LLM_SERVICE_URL}/health", "model")),
        _timed("llama.cpp (reasoning)", False, _llama_probe),
        _timed("Embeddings", False, _embeddings_probe),
    ]
    results = await asyncio.gather(*probes)

    # The gateway is answering this request, so its own status is not a guess.
    results.insert(0, {
        "name": "API Gateway", "status": "ok", "essential": True,
        "detail": "Serving this request", "latency_ms": 0,
    })

    down_essential = [r["name"] for r in results if r["status"] != "ok" and r["essential"]]
    down_optional = [r["name"] for r in results if r["status"] != "ok" and not r["essential"]]

    if down_essential:
        overall = "down"
    elif down_optional:
        # The app still runs without the model servers - you simply cannot
        # generate an analysis - so this is degraded, not down.
        overall = "degraded"
    else:
        overall = "ok"

    return {
        "overall": overall,
        "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "services": results,
        "urls": {
            "llama": LLAMA_BASE_URL,
            "embeddings": EMBED_BASE_URL,
        },
    }
