"""
LLM Service — Integration with self-hosted llama.cpp (OpenAI-compatible API)
Handles denial analysis reasoning and appeal letter generation
"""

import json
import logging
import os
from typing import Optional

import httpx
import orjson
from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# ── Configuration ──
DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator")
LLAMA_BASE_URL = os.environ.get("LLAMA_BASE_URL", "http://localhost:8080")
LLM_MODEL = os.environ.get("LLM_MODEL", "qwen2.5:7b")
# Default must be the api-gateway, not this service. It was localhost:8000,
# which is llm-service itself, so every /analyses/store POST 404'd and
# no analysis was ever persisted - while the response still said stored.
API_BASE = os.environ.get("API_BASE", "http://api:8000")
LLM_MAX_TOKENS = int(os.environ.get("LLM_MAX_TOKENS", "2048"))
# The llama.cpp server behind LLAMA_BASE_URL runs a REASONING model
# (Qwen3.6-35B-A3B). Left alone it spends the whole token budget on
# chain-of-thought and returns truncated, unparseable JSON. This chat
# template switch is the only lever that works - reasoning_effort is
# accepted and silently ignored by this model.
LLM_DISABLE_THINKING = os.environ.get("LLM_DISABLE_THINKING", "true").lower() not in ("0","false","no")

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("llmservice")

app = FastAPI(
    title="LLM Reasoning Service",
    description="Self-hosted LLM integration for denial analysis and appeal generation",
    version="1.0.0",
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


# ── llama.cpp Client (OpenAI-compatible) ──
class LlamaClient:
    """Client for llama.cpp OpenAI-compatible API"""

    def __init__(self, base_url: str = None, model: str = None):
        self.base_url = base_url or LLAMA_BASE_URL
        self.model = model or LLM_MODEL

    async def chat(self, system: str, user: str, temperature: float = 0.3) -> dict:
        """Send a chat completion request to llama.cpp"""
        async with httpx.AsyncClient(timeout=120) as client:
            resp = await client.post(
                f"{self.base_url}/v1/chat/completions",
                json={
                    "model": self.model,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user},
                    ],
                    "stream": False,
                    "temperature": temperature,
                    "max_tokens": LLM_MAX_TOKENS,
                    **({"chat_template_kwargs": {"enable_thinking": False}}
                       if LLM_DISABLE_THINKING else {}),
                },
            )
            resp.raise_for_status()
            data = resp.json()
            message = data["choices"][0]["message"]
            content = message.get("content") or ""
            if not content.strip():
                # A reasoning model can put everything in reasoning_content and
                # leave content empty. Fall back rather than return nothing.
                content = message.get("reasoning_content") or ""
            return {
                "message": {"content": content},
                "usage": data.get("usage", {}),
                "finish_reason": data["choices"][0].get("finish_reason"),
            }

    async def generate(self, prompt: str, system: str = None, temperature: float = 0.3) -> dict:
        """Send a text generation request to llama.cpp"""
        async with httpx.AsyncClient(timeout=120) as client:
            messages = []
            if system:
                messages.append({"role": "system", "content": system})
            messages.append({"role": "user", "content": prompt})

            resp = await client.post(
                f"{self.base_url}/v1/chat/completions",
                json={
                    "model": self.model,
                    "messages": messages,
                    "stream": False,
                    "temperature": temperature,
                    "max_tokens": LLM_MAX_TOKENS,
                    **({"chat_template_kwargs": {"enable_thinking": False}}
                       if LLM_DISABLE_THINKING else {}),
                },
            )
            resp.raise_for_status()
            data = resp.json()
            return {
                "message": {"content": data["choices"][0]["message"]["content"]},
                "usage": data.get("usage", {}),
            }

    async def list_models(self) -> list[str]:
        """List available models via OpenAI-compatible API"""
        async with httpx.AsyncClient(timeout=10) as client:
            resp = await client.get(f"{self.base_url}/v1/models")
            resp.raise_for_status()
            data = resp.json()
            return [m["id"] for m in data.get("data", [])]

    async def check_model_available(self, model: str = None) -> bool:
        """Check if a specific model is available"""
        model = model or self.model
        available = await self.list_models()
        return model in available or any(model.startswith(m) for m in available)

    async def health(self) -> bool:
        """Check if llama.cpp server is healthy"""
        async with httpx.AsyncClient(timeout=5) as client:
            resp = await client.get(f"{self.base_url}/health")
            return resp.status_code == 200


# ── Global Client ──
llama_client = LlamaClient()


# ── Database Storage ──
async def store_analysis(denial_id: str, claim_id: str, raw_prompt: str,
                         raw_response: str, parsed_result: dict,
                         model_name: str, prompt_tokens: int = 0,
                         completion_tokens: int = 0, total_tokens: int = 0):
    """Store LLM analysis result in the database"""
    payload = {
        "denial_id": denial_id,
        "claim_id": claim_id,
        "model_name": model_name,
        "raw_prompt": raw_prompt,
        "raw_response": raw_response,
        "parsed_result": parsed_result,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    }
    try:
        async with httpx.AsyncClient(timeout=30) as client:
            resp = await client.post(f"{API_BASE}/api/v1/analyses/store", json=payload)
            if resp.status_code != 200:
                logger.error(f"Failed to store analysis: {resp.text}")
                return False
            return True
    except Exception as e:
        logger.error(f"Database store error: {e}")
        return False


# ── Pydantic Models ──
class ChatRequest(BaseModel):
    system: str
    user: str
    temperature: float = 0.3

class ChatResponse(BaseModel):
    model: str
    response: str
    tokens_used: int

class DenialAnalysisRequest(BaseModel):
    denial_id: str
    claim_id: str
    prompt: dict  # {system, user}
    temperature: float = 0.3

class DenialAnalysisResponse(BaseModel):
    model: str
    raw_response: str
    parsed_json: Optional[dict] = None
    tokens_used: int
    stored: bool

class HealthResponse(BaseModel):
    status: str
    model: str
    model_available: bool


# ── Routes ──
@app.get("/health", response_model=HealthResponse)
async def health():
    model_available = await llama_client.check_model_available()
    return HealthResponse(
        status="healthy",
        model=LLM_MODEL,
        model_available=model_available,
    )

@app.post("/chat", response_model=ChatResponse)
async def chat(request: ChatRequest):
    """General chat endpoint"""
    try:
        result = await llama_client.chat(request.system, request.user, request.temperature)
        response_text = result.get("message", {}).get("content", "")

        # Estimate token counts
        prompt_tokens = len(request.system.split()) + len(request.user.split())
        completion_tokens = len(response_text.split())

        return ChatResponse(
            model=LLM_MODEL,
            response=response_text,
            tokens_used=prompt_tokens + completion_tokens,
        )
    except Exception as e:
        logger.error(f"Chat error: {e}")
        raise HTTPException(status_code=500, detail=str(e))

@app.post("/analyze-denial", response_model=DenialAnalysisResponse)
async def analyze_denial(request: DenialAnalysisRequest):
    """Analyze a denial and produce structured output"""
    try:
        # Extract prompt parts
        system_prompt = request.prompt.get("system", "")
        user_prompt = request.prompt.get("user", "")

        # Call LLM
        result = await llama_client.chat(system_prompt, user_prompt, request.temperature)
        raw_response = result.get("message", {}).get("content", "")

        # Try to parse JSON from response
        parsed_json = None
        try:
            # Clean up potential markdown formatting
            cleaned = raw_response.strip()
            if cleaned.startswith("```json"):
                cleaned = cleaned[7:]
            if cleaned.endswith("```"):
                cleaned = cleaned[:-3]
            cleaned = cleaned.strip()
            parsed_json = json.loads(cleaned)
        except (json.JSONDecodeError, ValueError):
            logger.warning(f"Could not parse LLM JSON response, storing raw")
            parsed_json = {"raw": raw_response, "parse_error": True}

        # Normalise `steps`. The prompt asks for objects with step/action keys,
        # but models routinely return a plain list of strings. Consumers should
        # not have to handle both shapes.
        if isinstance(parsed_json, dict) and isinstance(parsed_json.get("steps"), list):
            normalised = []
            for i, item in enumerate(parsed_json["steps"], start=1):
                if isinstance(item, dict):
                    normalised.append({
                        "step": item.get("step", i),
                        "action": item.get("action") or item.get("text") or "",
                    })
                else:
                    text = str(item).strip()
                    # Strip a leading "1." / "1)" the model already numbered.
                    for sep in (". ", ") ", "- "):
                        head, _, tail = text.partition(sep)
                        if head.isdigit() and tail:
                            text = tail.strip()
                            break
                    normalised.append({"step": i, "action": text})
            parsed_json["steps"] = normalised

        # Store in database. The result is reported honestly below; this used
        # to return stored=True even when the POST failed.
        stored_ok = await store_analysis(
            denial_id=request.denial_id,
            claim_id=request.claim_id,
            raw_prompt=f"{system_prompt}\n\n{user_prompt}",
            raw_response=raw_response,
            parsed_result=parsed_json or {},
            model_name=LLM_MODEL,
        )

        prompt_tokens = len(system_prompt.split()) + len(user_prompt.split())
        completion_tokens = len(raw_response.split())

        return DenialAnalysisResponse(
            model=LLM_MODEL,
            raw_response=raw_response,
            parsed_json=parsed_json,
            tokens_used=prompt_tokens + completion_tokens,
            stored=stored_ok,
        )
    except Exception as e:
        logger.error(f"Analysis error: {e}")
        raise HTTPException(status_code=500, detail=str(e))

@app.get("/models")
async def list_models():
    """List available models"""
    try:
        models = await llama_client.list_models()
        return {"models": models, "current_model": LLM_MODEL}
    except Exception as e:
        return {"models": [], "error": str(e), "current_model": LLM_MODEL}
