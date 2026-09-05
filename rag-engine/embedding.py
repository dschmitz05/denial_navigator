"""
Embedding Generation — Interface with llama.cpp (OpenAI-compatible API)
"""

import os
import httpx
from typing import Optional

LLAMA_BASE_URL = os.environ.get("LLAMA_BASE_URL", "http://localhost:8080")
EMBEDDING_MODEL = os.environ.get("EMBEDDING_MODEL", "nomic-embed-text")


class EmbeddingGenerator:
    """Generate text embeddings using llama.cpp OpenAI-compatible API"""

    def __init__(self, model: str = None, base_url: str = None):
        self.model = model or EMBEDDING_MODEL
        self.base_url = base_url or LLAMA_BASE_URL

    async def generate(self, text: str) -> list[float]:
        """Generate embedding for a single text"""
        async with httpx.AsyncClient(timeout=60) as client:
            resp = await client.post(
                f"{self.base_url}/v1/embeddings",
                json={"model": self.model, "input": text},
            )
            resp.raise_for_status()
            data = resp.json()
            embeddings = data.get("data", [])
            if not embeddings:
                raise ValueError(f"No embedding returned for: {text[:50]}")
            return embeddings[0]["embedding"]

    async def generate_batch(self, texts: list[str]) -> list[list[float]]:
        """Generate embeddings for multiple texts"""
        all_embeddings = []
        for text in texts:
            embedding = await self.generate(text)
            all_embeddings.append(embedding)
        return all_embeddings

    def get_model(self) -> str:
        return self.model
