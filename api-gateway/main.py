"""
API Gateway — Main entry point for Denial Navigator
Routes requests to microservices, manages database connections
"""

import logging
import os
from typing import Optional

from fastapi import FastAPI, HTTPException, Depends
from fastapi.middleware.cors import CORSMiddleware

# ── Configuration ──
DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator")
EDIPARSER_SERVICE_URL = os.environ.get("EDIPARSER_SERVICE_URL", "http://localhost:8001")
RAG_ENGINE_URL = os.environ.get("RAG_ENGINE_URL", "http://localhost:8002")
LLM_SERVICE_URL = os.environ.get("LLM_SERVICE_URL", "http://localhost:8003")
CORS_ORIGINS = os.environ.get("CORS_ORIGINS", "http://localhost:3081,http://localhost:5173").split(",")

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("apigateway")

# ── FastAPI App ──
app = FastAPI(
    title="Denial Navigator API",
    description="Healthcare denial management system — EDI parsing, RAG reasoning, appeals",
    version="1.0.0",
    docs_url="/docs",
    redoc_url="/redoc",
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=CORS_ORIGINS,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# ── Import routes ──
from api_gateway.routes import claims, denials, appeals, analyses, knowledge, ingestion, feedback, auth, users, audit
from api_gateway.services import db

# Register routers
app.include_router(claims.router, prefix="/api/v1", tags=["claims"])
app.include_router(denials.router, prefix="/api/v1", tags=["denials"])
app.include_router(appeals.router, prefix="/api/v1", tags=["appeals"])
app.include_router(analyses.router, prefix="/api/v1", tags=["analyses"])
app.include_router(knowledge.router, prefix="/api/v1", tags=["knowledge"])
app.include_router(ingestion.router, prefix="/api/v1", tags=["ingestion"])
app.include_router(feedback.router, prefix="/api/v1", tags=["feedback"])
app.include_router(auth.router, prefix="/api/v1", tags=["auth"])
app.include_router(users.router, prefix="/api/v1", tags=["users"])
app.include_router(audit.router, prefix="/api/v1", tags=["audit"])


# ── Health ──
@app.get("/health")
async def health():
    return {
        "status": "healthy",
        "version": "1.0.0",
        "services": {
            "ediparser": EDIPARSER_SERVICE_URL,
            "rag_engine": RAG_ENGINE_URL,
            "llm_service": LLM_SERVICE_URL,
            "database": DATABASE_URL.split("@")[0] + "@postgres",
        },
    }


@app.get("/")
async def root():
    return {
        "name": "Denial Navigator API",
        "version": "1.0.0",
        "docs": "/docs",
        "health": "/health",
    }
