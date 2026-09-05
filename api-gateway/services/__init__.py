"""API Gateway — HTTP clients for downstream services"""

import httpx
import os

EDIPARSER_SERVICE_URL = os.environ.get("EDIPARSER_SERVICE_URL", "http://localhost:8001")
RAG_ENGINE_URL = os.environ.get("RAG_ENGINE_URL", "http://localhost:8002")
LLM_SERVICE_URL = os.environ.get("LLM_SERVICE_URL", "http://localhost:8003")


class EDIParserClient:
    """Client for EDI Parser service"""

    def __init__(self, base_url: str = None):
        self.base_url = base_url or EDIPARSER_SERVICE_URL

    async def parse_file(self, file_data: bytes) -> dict:
        async with httpx.AsyncClient(timeout=60) as client:
            resp = await client.post(
                f"{self.base_url}/ingest",
                files={"file": ("upload.835", file_data, "text/plain")},
            )
            resp.raise_for_status()
            return resp.json()

    async def ingest_dropzone(self) -> dict:
        async with httpx.AsyncClient(timeout=60) as client:
            resp = await client.post(f"{self.base_url}/ingest-dropzone")
            resp.raise_for_status()
            return resp.json()


class RAGEngineClient:
    """Client for RAG Engine service"""

    def __init__(self, base_url: str = None):
        self.base_url = base_url or RAG_ENGINE_URL

    async def search(self, query: str, top_k: int = 5, filters: dict = None) -> dict:
        async with httpx.AsyncClient(timeout=30) as client:
            resp = await client.post(
                f"{self.base_url}/search",
                json={"query": query, "top_k": top_k, "filters": filters or {}},
            )
            resp.raise_for_status()
            return resp.json()

    async def ingest_document(self, document_id: str, content: str) -> dict:
        # Embedding a long document is slow; give it real time.
        async with httpx.AsyncClient(timeout=600) as client:
            resp = await client.post(
                f"{self.base_url}/ingest-document",
                json={"document_id": document_id, "content": content},
            )
            resp.raise_for_status()
            return resp.json()

    async def build_prompt(self, **kwargs) -> dict:
        async with httpx.AsyncClient(timeout=30) as client:
            resp = await client.post(f"{self.base_url}/prompt/denial-analysis", json=kwargs)
            resp.raise_for_status()
            return resp.json()


class LLMServiceClient:
    """Client for LLM Service"""

    def __init__(self, base_url: str = None):
        self.base_url = base_url or LLM_SERVICE_URL

    async def chat(self, system: str, user: str, temperature: float = 0.3) -> dict:
        async with httpx.AsyncClient(timeout=120) as client:
            resp = await client.post(
                f"{self.base_url}/chat",
                json={"system": system, "user": user, "temperature": temperature},
            )
            resp.raise_for_status()
            return resp.json()

    async def analyze_denial(self, request: dict) -> dict:
        async with httpx.AsyncClient(timeout=180) as client:
            resp = await client.post(f"{self.base_url}/analyze-denial", json=request)
            resp.raise_for_status()
            return resp.json()
