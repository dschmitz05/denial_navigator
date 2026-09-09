"""
EDI Parser Service — FastAPI entry point
Monitors dropzone, parses X12 835 (remittance) and 837 (claim submission)
files, stores results in PostgreSQL
"""

import asyncio
import json
import logging
import os
import hashlib
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import httpx
import orjson
from fastapi import FastAPI, File, HTTPException, UploadFile, Request
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, Field

from parser.x12_parser import X12Parser
from parser.schema import Parsed835Response, ParsedClaim, ParsedDenial
from watch.watcher import FileWatcher

# ── Configuration ──
DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator")
DROPZONE_PATH = os.environ.get("DROPZONE_PATH", "/app/dropzone")
OUTPUT_PATH = os.environ.get("OUTPUT_PATH", "/app/output")
API_BASE = os.environ.get("API_BASE", "http://api:8000")
# The gateway refuses unauthenticated requests. This service has no user to
# log in as, so it presents the shared service credential instead.
SERVICE_API_KEY = os.environ.get("SERVICE_API_KEY", "")
SERVICE_HEADERS = {"X-Service-Key": SERVICE_API_KEY, "X-Service-Name": "ediparser"}

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("ediparser")

# ── FastAPI App ──
app = FastAPI(
    title="EDI Parser Service",
    description="X12 835/837 parser — ingests raw EDI files and produces structured claim/denial data",
    version="2.0.0",
)

# This service is reached by the API gateway over the Docker network, never
# by a browser, so it needs no CORS at all. `allow_origins=["*"]` together
# with `allow_credentials=True` told any site on the internet it could make
# credentialed requests here; set CORS_ORIGINS explicitly if that ever changes.
_cors_origins = [o for o in os.environ.get("CORS_ORIGINS", "").split(",") if o.strip()]
if _cors_origins:
    app.add_middleware(
        CORSMiddleware,
        allow_origins=_cors_origins,
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

# ── Pydantic Models ──
class HealthResponse(BaseModel):
    status: str
    version: str
    dropzone_path: str
    parsed_files_count: int

class IngestResponse(BaseModel):
    file_name: str
    transaction_type: Optional[str] = None   # "835" or "837"
    file_size: int
    file_hash: str
    claims_parsed: int
    denials_parsed: int
    status: str
    message: str
    claims: list[dict] = Field(default_factory=list)
    denials: list[dict] = Field(default_factory=list)

class FileInfo(BaseModel):
    file_name: str
    file_size: int
    file_hash: str
    status: str
    claims_count: int
    denials_count: int
    processed_at: Optional[datetime] = None

# ── State ──
parser = X12Parser()
watcher: Optional[FileWatcher] = None

ALLOWED_EXTENSIONS = {".835", ".837", ".edi"}
MAX_FILE_SIZE = 25 * 1024 * 1024
RATE_LIMIT = 60
RATE_WINDOW = 60


class _RateLimiter:
    def __init__(self, limit: int, window: int):
        self.limit = limit
        self.window = window
        self._buckets: dict[str, list[float]] = defaultdict(list)
    def check(self, key: str) -> None:
        now = time.monotonic()
        bucket = self._buckets[key]
        bucket[:] = [t for t in bucket if now - t < self.window]
        if len(bucket) >= self.limit:
            raise HTTPException(status_code=429, detail=f"Rate limit exceeded: {self.limit} requests per {self.window}s")
        bucket.append(now)

_rate_limiter = _RateLimiter(RATE_LIMIT, RATE_WINDOW)

# ── Database Helper (lightweight asyncpg-style via http to API) ──
async def store_parsed_data(parsed: Parsed835Response, file_hash: str, file_name: str, file_size: int = 0) -> tuple[int, dict]:
    """Store parsed results via the API gateway; returns (http_status, body)"""
    payload = {
        "file_name": file_name,
        "file_hash": file_hash,
        "file_size": file_size,
        # model_dump() is required: httpx json= uses json.dumps, which cannot
        # serialise pydantic models. Passing them raised on every single call
        # and the exception was only logged, so nothing ever reached the DB.
        "transaction_type": parsed.metadata.transaction_set_identifier,
        "claims": [c.model_dump() for c in parsed.claims],
        "denials": [d.model_dump() for d in parsed.denials],
    }
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(
            f"{API_BASE}/api/v1/ingestion/store", json=payload, headers=SERVICE_HEADERS
        )
        if resp.status_code != 200:
            logger.error(f"Failed to store parsed data: HTTP {resp.status_code} {resp.text}")
        # The status code travels with the body: a 409 duplicate has a body
        # that looks like an error but means "already stored", and only the
        # caller knows which of those two to report.
        return resp.status_code, resp.json()

# ── EDI File Processing ──
DROPZONE_PATTERNS = ("*.835", "*.837", "*.edi", "*.txt")


def _dropzone_files(dropzone: Path) -> list[Path]:
    """Every EDI file in the dropzone, de-duplicated and in a stable order."""
    seen: dict[str, Path] = {}
    for pattern in DROPZONE_PATTERNS:
        for f in dropzone.glob(pattern):
            seen[f.name] = f
    return [seen[name] for name in sorted(seen)]

async def process_file(file_path: Path) -> dict:
    """Process a single 835 or 837 file"""
    file_name = file_path.name
    logger.info(f"Processing file: {file_name}")

    # Read file content
    content = file_path.read_text(encoding="utf-8", errors="replace")

    # Calculate hash
    file_hash = hashlib.sha256(content.encode("utf-8", errors="replace")).hexdigest()

    # Parse X12
    try:
        parsed = parser.parse(content)
    except Exception as e:
        logger.error(f"Parse error in {file_name}: {e}")
        return {"file_name": file_name, "status": "error", "error": str(e)}

    # Store via API
    try:
        http_status, result = await store_parsed_data(parsed, file_hash, file_name, len(content.encode("utf-8", errors="replace")))
    except Exception as e:
        logger.error(f"Store error for {file_name}: {e}")
        http_status, result = None, {"error": str(e)}

    # Save output
    output_file = Path(OUTPUT_PATH) / f"{file_name}.json"
    output_file.write_text(orjson.dumps(parsed.model_dump(), option=orjson.OPT_INDENT_2).decode("utf-8"))

    # The store result used to be computed and discarded, so a file whose
    # store call failed still reported "completed" - the one status a
    # watcher-driven pipeline must get right. Success is exactly the
    # gateway's 200 with {"status": "stored"}. A 409 duplicate is NOT a
    # success of this run: the data is in the DB, but under a different
    # file name (the detail says which), and the operator should know that.
    store_status = (result or {}).get("status")
    stored_ok = http_status == 200 and store_status == "stored"
    status = "completed" if stored_ok else "store_failed"

    logger.info(
        f"Processed {file_name}: {len(parsed.claims)} claims, "
        f"{len(parsed.denials)} denials, store={status}"
    )
    return {
        "file_name": file_name,
        "file_hash": file_hash,
        "claims_count": len(parsed.claims),
        "denials_count": len(parsed.denials),
        "status": status,
        "store_result": store_status,
        # A transport failure puts the exception in "error"; an HTTP failure
        # carries the gateway's explanation in "detail" (a 409 duplicate says
        # which file name the data was already stored under).
        "error": (result or {}).get("error") or (result or {}).get("detail"),
        "processed_at": datetime.now(timezone.utc).isoformat(),
    }

# ── Routes ──
@app.get("/health", response_model=HealthResponse)
async def health():
    return HealthResponse(
        status="healthy",
        version="1.0.0",
        dropzone_path=DROPZONE_PATH,
        parsed_files_count=len(list(Path(OUTPUT_PATH).glob("*.json"))) if Path(OUTPUT_PATH).is_dir() else 0,
    )

@app.post("/ingest", response_model=IngestResponse)
async def ingest_file(request: Request, file: UploadFile = File(...)):
    """Upload and parse an 835 or 837 file directly"""
    # Only the gateway calls this service, so every request shares one address.
    # The gateway already limits per user upstream; this is a backstop against
    # a runaway loop, not a per-user control.
    _rate_limiter.check("gateway")
    if file.filename is None:
        raise HTTPException(status_code=400, detail="No file name provided")
    ext = "." + file.filename.rsplit(".", 1)[-1].lower() if "." in file.filename else ""
    if ext not in ALLOWED_EXTENSIONS:
        raise HTTPException(status_code=400, detail=f"Invalid file type. Allowed: {', '.join(sorted(ALLOWED_EXTENSIONS))}")
    # Read with a cap rather than reading everything and measuring afterwards.
    chunks, total = [], 0
    while True:
        chunk = await file.read(1 << 20)
        if not chunk:
            break
        total += len(chunk)
        if total > MAX_FILE_SIZE:
            raise HTTPException(status_code=413, detail=f"File too large: max {MAX_FILE_SIZE} bytes")
        chunks.append(chunk)
    content = b"".join(chunks)
    if not content.startswith(b"ISA"):
        raise HTTPException(status_code=400, detail="File does not appear to be a valid X12 file (missing ISA segment)")
    file_name_base = file.filename or "uploaded_file.835"
    file_hash = hashlib.sha256(content).hexdigest()
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    stamped_name = f"ingest_{stamp}_{file_name_base}"
    try:
        parsed = parser.parse(content.decode("utf-8", errors="replace"))
    except ValueError as e:
        logger.warning(f"Parse rejected for {file_name_base}: {e}")
        raise HTTPException(status_code=400, detail=f"Parse failed: {e}")
    except Exception as e:
        logger.exception(f"Unexpected parse error for {file_name_base}")
        raise HTTPException(status_code=400, detail=f"Parse failed: {e}")

    # Save output for inspection
    output_file = Path(OUTPUT_PATH) / f"{stamped_name}.json"
    output_file.write_text(orjson.dumps(parsed.model_dump(), option=orjson.OPT_INDENT_2).decode("utf-8"))

    logger.info(f"Processed {file_name_base}: {len(parsed.claims)} claims, {len(parsed.denials)} denials")
    return IngestResponse(
        file_name=file_name_base,
        transaction_type=parsed.metadata.transaction_set_identifier,
        file_size=len(content),
        file_hash=file_hash,
        claims_parsed=len(parsed.claims),
        denials_parsed=len(parsed.denials),
        status="completed",
        message=(
            f"Parsed {len(parsed.claims)} claims with {len(parsed.denials)} denials"
            + (f" ({len(parsed.warnings)} warnings)" if parsed.warnings else "")
        ),
        claims=[c.model_dump() for c in parsed.claims],
        denials=[d.model_dump() for d in parsed.denials],
    )

@app.post("/ingest-dropzone")
async def ingest_dropzone():
    """Manually trigger processing of all files in the dropzone"""
    dropzone = Path(DROPZONE_PATH)
    files = _dropzone_files(dropzone)
    results = []
    for f in files:
        result = await process_file(f)
        results.append(result)
    return {"processed": len(results), "results": results}

@app.get("/files", response_model=list[FileInfo])
async def list_files():
    """List all processed files"""
    dropzone = Path(DROPZONE_PATH)
    files = []
    for f in _dropzone_files(dropzone):
        files.append(FileInfo(
            file_name=f.name,
            file_size=f.stat().st_size,
            file_hash="",
            # Parsed output lives in OUTPUT_PATH, not the dropzone - checking
            # the wrong directory meant every file reported "pending" forever.
            status="processed" if (Path(OUTPUT_PATH) / f"{f.name}.json").exists() else "pending",
            claims_count=0,
            denials_count=0,
        ))
    return files

@app.get("/output/{file_name}")
async def get_output(file_name: str):
    """Get parsed output for a file"""
    output_file = Path(OUTPUT_PATH) / f"{file_name}.json"
    if not output_file.exists():
        raise HTTPException(status_code=404, detail="Output not found")
    return json.loads(output_file.read_text())

# ── Retention ──
# Both directories hold PHI: the dropzone holds the raw 835s (patient names,
# member IDs, dates of birth), and the output holds the parsed form of the
# same thing. Neither was ever cleaned, so a deployment accumulated an
# ever-growing pile of unencrypted PHI on disk beside a database that has
# retention rules. The database is the record of the ingest; these files are
# a working copy and a debugging aid.
#
# Pruned together, or a dropzone file whose output has been removed would be
# reparsed on the next restart. 0 disables the sweep.
PARSED_RETENTION_DAYS = int(os.environ.get("PARSED_RETENTION_DAYS", "30"))


def prune_old_files() -> int:
    """Remove dropzone and output files past the retention window."""
    if PARSED_RETENTION_DAYS <= 0:
        return 0
    cutoff = time.time() - PARSED_RETENTION_DAYS * 86400
    removed = 0
    for directory in (DROPZONE_PATH, OUTPUT_PATH):
        d = Path(directory)
        if not d.is_dir():
            continue
        for f in d.iterdir():
            try:
                if f.is_file() and f.stat().st_mtime < cutoff:
                    f.unlink()
                    removed += 1
            except OSError as e:
                logger.warning(f"Could not prune {f.name}: {e}")
    if removed:
        logger.info(
            f"Pruned {removed} file(s) older than {PARSED_RETENTION_DAYS} days"
        )
    return removed


async def _retention_loop():
    """Sweep daily. The parser is long-lived, so startup alone is not enough."""
    while True:
        try:
            prune_old_files()
        except Exception as e:
            logger.error(f"Retention sweep failed: {e}")
        await asyncio.sleep(86400)


# ── Startup ──
@app.on_event("startup")
async def startup():
    dropzone = Path(DROPZONE_PATH)
    dropzone.mkdir(parents=True, exist_ok=True)
    Path(OUTPUT_PATH).mkdir(parents=True, exist_ok=True)

    # Before seeding, so the watcher does not treat a just-pruned output as
    # proof that its dropzone file still needs parsing.
    prune_old_files()

    global watcher
    watcher = FileWatcher(DROPZONE_PATH, process_file)
    watcher.seed_from_outputs(OUTPUT_PATH)
    watcher.start()
    asyncio.create_task(_retention_loop())
    logger.info(
        f"File watcher started on {DROPZONE_PATH}; "
        f"retention {PARSED_RETENTION_DAYS} days"
    )

@app.on_event("shutdown")
async def shutdown():
    global watcher
    if watcher:
        watcher.stop()
