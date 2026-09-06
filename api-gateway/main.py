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
    # Both doc pages are served by hand below, from vendored assets.
    docs_url=None,
    redoc_url=None,
)

# ── Middleware ──
#
# Starlette applies these outermost-last, so the registration order below is
# the REVERSE of the request order. What actually runs is:
#
#   AuditMiddleware        -> logs every request, including ones rejected below
#     CORSMiddleware       -> so even a 401 carries CORS headers and the
#                             browser sees the status instead of a network error
#       AccessControl      -> rejects anyone without credentials
#         route handler
#
# Audit has to sit outside access control: a refused access is precisely the
# event a compliance reviewer needs recorded.
from api_gateway.services.access import AccessControlMiddleware
from api_gateway.services.audit import AuditMiddleware

app.add_middleware(AccessControlMiddleware)

app.add_middleware(
    CORSMiddleware,
    allow_origins=CORS_ORIGINS,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

app.add_middleware(AuditMiddleware)

# ── Import routes ──
from api_gateway.routes import claims, denials, appeals, analyses, knowledge, ingestion, feedback, auth, users, audit, system, retention
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
app.include_router(system.router, prefix="/api/v1", tags=["system"])
app.include_router(retention.router, prefix="/api/v1", tags=["retention"])


# ── API documentation ──
#
# Served entirely from assets vendored into this image. FastAPI's built-in
# pages pull Swagger UI and ReDoc from jsdelivr, and ReDoc additionally pulls
# Montserrat and Roboto from Google Fonts - three outbound requests at page
# load. In an air-gapped deployment every one of them fails, and the failure
# mode is a blank white page with nothing on it explaining why. (That is also
# how redoc@next's 404 presented before this: the tag had been removed.)
#
# Nothing here reaches the network. The assets live in static/docs and are
# served by this app, so the docs work identically on a machine with no route
# off the host - and cannot break because someone else moved a CDN tag.
from pathlib import Path

from fastapi.openapi.docs import get_redoc_html, get_swagger_ui_html
from fastapi.staticfiles import StaticFiles

STATIC_DIR = Path(__file__).parent / "static"
app.mount("/static", StaticFiles(directory=str(STATIC_DIR)), name="static")

DOCS_ASSETS = "/static/docs"


@app.get("/docs", include_in_schema=False)
async def swagger_html():
    return get_swagger_ui_html(
        openapi_url=app.openapi_url,
        title=f"{app.title} — Swagger UI",
        swagger_js_url=f"{DOCS_ASSETS}/swagger-ui-bundle.js",
        swagger_css_url=f"{DOCS_ASSETS}/swagger-ui.css",
        swagger_favicon_url=f"{DOCS_ASSETS}/favicon.svg",
    )


@app.get("/redoc", include_in_schema=False)
async def redoc_html():
    return get_redoc_html(
        openapi_url=app.openapi_url,
        title=f"{app.title} — API Reference",
        redoc_js_url=f"{DOCS_ASSETS}/redoc.standalone.js",
        redoc_favicon_url=f"{DOCS_ASSETS}/favicon.svg",
        # Otherwise the page requests Montserrat and Roboto from Google Fonts
        # and blocks on them; ReDoc falls back to system fonts without it.
        with_google_fonts=False,
    )


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
