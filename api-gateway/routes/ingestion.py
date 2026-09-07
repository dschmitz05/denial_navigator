"""API Gateway — Ingestion routes"""

import hashlib
import json
import logging
from datetime import date, datetime
from typing import Optional
import uuid

from fastapi import APIRouter, HTTPException, UploadFile, File, Query, Depends, Request
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services import EDIParserClient
from api_gateway.services.ratelimit import SlidingWindowLimiter
from api_gateway.services.audit import _client_ip

logger = logging.getLogger("api_gateway.ingestion")

router = APIRouter()
edi_client = EDIParserClient()

ALLOWED_EXTENSIONS = {".835", ".837", ".edi"}
MAX_FILE_SIZE = 25 * 1024 * 1024  # 25 MB
RATE_LIMIT = 30  # requests
RATE_WINDOW = 60  # seconds


_rate_limiter = SlidingWindowLimiter(RATE_LIMIT, RATE_WINDOW, "ingestion")


def _limit_key(request: Request) -> str:
    """Who this upload is charged to.

    It used to be request.client.host, which behind nginx is the proxy's
    address on every single request - so all users shared one 30/minute
    bucket and any one of them could lock out everybody else. Charging the
    authenticated user makes the limit mean what it says.
    """
    who = getattr(request.state, "principal", None)
    if who is not None and getattr(who, "user_id", None):
        return f"user:{who.user_id}"
    if who is not None and getattr(who, "username", None):
        return f"svc:{who.username}"
    return f"ip:{_client_ip(request) or 'unknown'}"


def _check_rate(request: Request) -> None:
    key = _limit_key(request)
    if not _rate_limiter.allow(key):
        raise HTTPException(
            status_code=429,
            detail=f"Rate limit exceeded: {RATE_LIMIT} uploads per {RATE_WINDOW}s",
            headers={"Retry-After": str(_rate_limiter.retry_after(key))},
        )


async def _read_capped(file: UploadFile, limit: int) -> bytes:
    """Read at most `limit` bytes, then refuse.

    `await file.read()` followed by a length check pulls the whole body into
    memory before deciding it is too big, which turns the size limit into a
    memory-exhaustion vector rather than a defence against one.
    """
    chunks, total = [], 0
    while True:
        chunk = await file.read(1 << 20)      # 1 MiB
        if not chunk:
            break
        total += len(chunk)
        if total > limit:
            raise HTTPException(
                status_code=413,
                detail=f"File too large: maximum {limit // (1024 * 1024)} MB",
            )
        chunks.append(chunk)
    return b"".join(chunks)


def _validate_upload(file: UploadFile) -> None:
    if file.filename is None:
        raise HTTPException(status_code=400, detail="No file name provided")
    ext = "." + file.filename.rsplit(".", 1)[-1].lower() if "." in file.filename else ""
    if ext not in ALLOWED_EXTENSIONS:
        raise HTTPException(
            status_code=400,
            detail=(
                f"Invalid file type '{file.filename}'. "
                f"Allowed extensions: {', '.join(sorted(ALLOWED_EXTENSIONS))}"
            ),
        )


def _parse_date(val) -> Optional[date]:
    """Try to parse a date string from EDI data; return None on failure."""
    if val is None or val == "":
        return None
    s = str(val).strip()
    if not s:
        return None
    # Common EDI date formats: YYYYMMDD, MMDDYYYY, YYYY-MM-DD, MM/DD/YYYY
    for fmt in ("%Y%m%d", "%Y-%m-%d", "%m/%d/%Y", "%m%d%Y", "%d-%b-%Y"):
        try:
            return datetime.strptime(s, fmt).date()
        except ValueError:
            continue
    return None


async def _already_ingested(conn, file_hash: str):
    """The previous ingest of this exact file, if there was one.

    The hash was recorded from the beginning and never compared to anything, so
    re-uploading a file silently doubled its denials - and with them the denied
    dollar totals, the CARC counts and the queue.
    """
    return await conn.fetchrow(
        """
        SELECT id, file_name, created_at, claims_count, denials_count
          FROM ingestion_log
         WHERE file_hash = $1 AND status IN ('completed', 'parsed')
         ORDER BY created_at DESC LIMIT 1
        """,
        file_hash,
    )


def _duplicate_response(previous) -> HTTPException:
    when = previous["created_at"].strftime("%d %b %Y at %H:%M")
    return HTTPException(
        status_code=409,
        detail=(
            f"This exact file was already ingested as '{previous['file_name']}' on "
            f"{when} ({previous['claims_count']} claims, {previous['denials_count']} "
            "denials). Re-ingesting it would duplicate those denials. "
            "Send force=true if you intend to load it again anyway."
        ),
    )


def _clean_claim(claim, index, seen_numbers):
    """Normalize claim data from EDIParser for safe DB insertion."""
    raw_id = claim.get("claim_id", "") or ""
    patient_id = claim.get("patient_id", "") or ""
    dob = _parse_date(claim.get("date_of_birth"))
    patient_name = claim.get("patient_name")

    # Generate a unique claim number
    # Use patient_id + DOB as the base, but ensure uniqueness
    base = f"{patient_id}-{dob or 'NODOB'}"
    claim_number = base if raw_id == "" else raw_id

    # If this claim_number already seen, append a UUID suffix
    if claim_number in seen_numbers:
        claim_number = f"{raw_id or base}-{uuid.uuid4().hex[:8]}"
    seen_numbers.add(claim_number)

    return {
        "claim_id": claim_number,
        "patient_id": patient_id,
        "patient_name": patient_name,
        "date_of_birth": dob,
        "provider_npi": claim.get("provider_npi"),
        "provider_name": claim.get("provider_name"),
        "payer_name": claim.get("payer_name"),
        "payer_id_number": claim.get("payer_id_number"),
        "total_charged": claim.get("total_charged", 0),
        "total_paid": claim.get("total_paid", 0),
        "total_adjustment": claim.get("total_adjustment", 0),
        "claim_type": claim.get("claim_type", "professional"),
        # These are DATE columns; asyncpg needs date objects, not strings.
        # The old parser never populated them, which hid this until now.
        "service_from": _parse_date(claim.get("service_from")),
        "service_to": _parse_date(claim.get("service_to")),
        "diagnosis_codes": claim.get("diagnosis_codes", []) or [],
    }


def _clean_denial(denial):
    """Normalize denial data from EDIParser for safe DB insertion.

    NOTE: the denials table spells the column `modifier_1` (a typo baked into
    database/init.sql) and stores the free-text reason in `adjustment_reason`.
    The dict keys here stay correctly spelled; the INSERT statements below do
    the translation. Before this was fixed every denial insert failed on an
    undefined column, which is why the table held 0 rows against 91 ingestion
    runs.
    """
    return {
        "claim_id": denial.get("claim_id", ""),
        "service_line_number": denial.get("service_line_number"),
        "cpt_code": denial.get("cpt_code"),
        "hcpcs_code": denial.get("hcpcs_code"),
        "modifier_1": denial.get("modifier_1"),
        "modifier_2": denial.get("modifier_2"),
        "charge_amount": denial.get("charge_amount", 0),
        "payment_amount": denial.get("payment_amount", 0),
        "adjustment_amount": denial.get("adjustment_amount", 0),
        "cagc": denial.get("cagc"),
        "carc_code": denial.get("carc_code"),
        "rarc_code": denial.get("rarc_code"),
        "denial_reason": denial.get("denial_reason") or denial.get("denial_reason_code"),
        "denial_date": _parse_date(denial.get("denial_date")),
    }


# ── Claims upsert ──
#
# A claim can arrive twice: as an 837 submission and later as an 835
# remittance. The old `DO UPDATE SET updated_at, raw_835_data` meant the
# SECOND file contributed nothing but a timestamp — so an 835 could never
# record payment against a claim that had already been submitted, which is
# the whole point of this application. These two variants merge by source.

_CLAIM_INSERT = """
    INSERT INTO claims
        (claim_number, patient_id, patient_name, date_of_birth, provider_npi, provider_name,
         payer_name, payer_id_number, total_charge, total_paid, total_adjustment,
         status, claim_type, service_from, service_to, icd_10_codes, raw_835_data, parsed_at)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
            'parsed', $12, $13, $14, $15, $16, NOW())
"""

# The 835 is the adjudication: authoritative for money, and it supersedes.
_ON_CONFLICT_835 = """
    ON CONFLICT (claim_number) DO UPDATE SET
        patient_name     = COALESCE(EXCLUDED.patient_name, claims.patient_name),
        date_of_birth    = COALESCE(EXCLUDED.date_of_birth, claims.date_of_birth),
        provider_npi     = COALESCE(EXCLUDED.provider_npi, claims.provider_npi),
        provider_name    = COALESCE(EXCLUDED.provider_name, claims.provider_name),
        payer_name       = COALESCE(EXCLUDED.payer_name, claims.payer_name),
        payer_id_number  = COALESCE(EXCLUDED.payer_id_number, claims.payer_id_number),
        -- An 835 is authoritative for money, but "authoritative" is not the
        -- same as "overwrite with zero". A remittance that reports no charge
        -- for a claim the 837 already priced should leave that figure alone;
        -- otherwise re-ingesting wipes a charge amount the submission supplied.
        total_charge     = CASE WHEN EXCLUDED.total_charge > 0
                                THEN EXCLUDED.total_charge ELSE claims.total_charge END,
        total_paid       = EXCLUDED.total_paid,
        total_adjustment = EXCLUDED.total_adjustment,
        service_from     = COALESCE(EXCLUDED.service_from, claims.service_from),
        service_to       = COALESCE(EXCLUDED.service_to, claims.service_to),
        icd_10_codes     = CASE
                             WHEN EXCLUDED.icd_10_codes IS NOT NULL
                              AND cardinality(EXCLUDED.icd_10_codes) > 0
                             THEN EXCLUDED.icd_10_codes ELSE claims.icd_10_codes END,
        status           = 'parsed',
        raw_835_data     = EXCLUDED.raw_835_data,
        parsed_at        = NOW(),
        updated_at       = NOW()
"""

# The 837 is the submission: it fills gaps and owns diagnosis codes and
# claim_type, but must never overwrite money an 835 has already adjudicated.
_ON_CONFLICT_837 = """
    ON CONFLICT (claim_number) DO UPDATE SET
        patient_name     = COALESCE(claims.patient_name, EXCLUDED.patient_name),
        date_of_birth    = COALESCE(claims.date_of_birth, EXCLUDED.date_of_birth),
        provider_npi     = COALESCE(claims.provider_npi, EXCLUDED.provider_npi),
        provider_name    = COALESCE(claims.provider_name, EXCLUDED.provider_name),
        payer_name       = COALESCE(claims.payer_name, EXCLUDED.payer_name),
        payer_id_number  = COALESCE(claims.payer_id_number, EXCLUDED.payer_id_number),
        total_charge     = CASE WHEN claims.total_charge = 0
                                THEN EXCLUDED.total_charge ELSE claims.total_charge END,
        claim_type       = EXCLUDED.claim_type,
        service_from     = COALESCE(claims.service_from, EXCLUDED.service_from),
        service_to       = COALESCE(claims.service_to, EXCLUDED.service_to),
        icd_10_codes     = CASE
                             WHEN EXCLUDED.icd_10_codes IS NOT NULL
                              AND cardinality(EXCLUDED.icd_10_codes) > 0
                             THEN EXCLUDED.icd_10_codes ELSE claims.icd_10_codes END,
        updated_at       = NOW()
"""


def _claim_upsert_sql(transaction_type: Optional[str]) -> str:
    """Pick the merge strategy for the document that produced this claim."""
    return _CLAIM_INSERT + (_ON_CONFLICT_837 if transaction_type == "837" else _ON_CONFLICT_835)


class StoreIngestion(BaseModel):
    file_name: str
    file_hash: str
    file_size: int
    claims: list[dict]
    denials: list[dict]
    transaction_type: Optional[str] = None   # "835" or "837"


@router.post("/ingestion/upload", response_model=dict)
async def upload_file(request: Request, file: UploadFile = File(...)):
    """Upload an 835 file for parsing"""
    _check_rate(request)
    _validate_upload(file)
    content = await _read_capped(file, MAX_FILE_SIZE)
    if not content.startswith(b"ISA"):
        raise HTTPException(status_code=400, detail="File does not appear to be a valid X12 file (missing ISA segment)")
    file_hash = hashlib.sha256(content).hexdigest()

    try:
        result = await edi_client.parse_file(content)
    except Exception as e:
        raise HTTPException(status_code=400, detail=f"Parse failed: {e}")

    return {
        "file_name": file.filename or "unknown",
        "file_hash": file_hash,
        "claims_parsed": result.get("claims_parsed", 0),
        "denials_parsed": result.get("denials_parsed", 0),
        "status": result.get("status", "error"),
    }


@router.post("/ingestion/ingest", response_model=dict, status_code=201)
async def ingest_file(
    request: Request,
    file: UploadFile = File(...),
    force: bool = Query(False, description="Ingest even if this exact file was loaded before"),
):
    """Upload and parse an 835/837 file, storing results in one step"""
    _check_rate(request)
    _validate_upload(file)
    content = await _read_capped(file, MAX_FILE_SIZE)
    if not content.startswith(b"ISA"):
        raise HTTPException(status_code=400, detail="File does not appear to be a valid X12 file (missing ISA segment)")
    file_hash = hashlib.sha256(content).hexdigest()
    file_size = len(content)

    if not force:
        async with get_connection() as conn:
            previous = await _already_ingested(conn, file_hash)
        if previous:
            raise _duplicate_response(previous)

    # Parse via EDI service
    try:
        result = await edi_client.parse_file(content)
    except Exception as e:
        async with get_connection() as conn:
            await conn.execute(
                """INSERT INTO ingestion_log (file_name, file_size_bytes, file_hash, status, errors)
                   VALUES ($1, $2, $3, 'error', $4)""",
                file.filename or "unknown", file_size, file_hash, json.dumps({"error": str(e)}),
            )
        raise HTTPException(status_code=400, detail=f"Parse failed: {e}")

    claims = result.get("claims", []) or []
    denials = result.get("denials", []) or []
    transaction_type = result.get("transaction_type")

    if not claims and not denials:
        async with get_connection() as conn:
            await conn.execute(
                """INSERT INTO ingestion_log
                   (file_name, file_size_bytes, file_hash, status, claims_count, denials_count)
                VALUES ($1, $2, $3, 'completed', 0, 0)""",
                file.filename or "unknown", file_size, file_hash,
            )
        return {
            "status": "stored",
            "file_name": file.filename or "unknown",
            "claims_stored": 0,
            "denials_stored": 0,
        }

    async with get_connection() as conn:
        # Everything below is one unit of work. Without this a failure partway
        # through left the file logged as 'completed' with only some of its
        # claims written and denials referencing claims that were never stored -
        # and since the file hash was already recorded, the retry was then
        # refused as a duplicate. The transaction makes a failed ingest a no-op
        # the user can simply upload again.
        async with conn.transaction():
            # Create ingestion log first, get its id
            ingestion_row = await conn.fetchrow(
                """
                INSERT INTO ingestion_log
                    (file_name, file_size_bytes, file_hash, status, claims_count, denials_count, raw_response)
                VALUES ($1, $2, $3, 'completed', $4, $5, $6)
                RETURNING id
                """,
                file.filename or "unknown", file_size, file_hash,
                len(claims), len(denials), json.dumps(result, default=str),
            )

            # Store claims and denials (cleaned)
            seen_numbers = set()
            cleaned_claims = [_clean_claim(c, i, seen_numbers) for i, c in enumerate(claims)]
            cleaned_denials = [_clean_denial(d) for d in denials]

            for claim_data in cleaned_claims:
                await conn.execute(
                    _claim_upsert_sql(transaction_type),
                    claim_data["claim_id"],
                    claim_data["patient_id"],
                    claim_data["patient_name"],
                    claim_data["date_of_birth"],
                    claim_data["provider_npi"],
                    claim_data["provider_name"],
                    claim_data["payer_name"],
                    claim_data["payer_id_number"],
                    claim_data["total_charged"],
                    claim_data["total_paid"],
                    claim_data["total_adjustment"],
                    claim_data["claim_type"],
                    claim_data["service_from"],
                    claim_data["service_to"],
                    claim_data["diagnosis_codes"],
                    json.dumps(claim_data, default=str),
                )

            denials_written = 0
            for denial_data in cleaned_denials:
                # ON CONFLICT DO NOTHING means "offered" and "stored" are different
                # numbers. Reporting the first as the second told the user five
                # denials had been stored when none had.
                inserted = await conn.fetchval(
                    """
                    INSERT INTO denials
                        (claim_id, service_line_number, cpt_code, hcpcs_code, modifier_1, modifier_2,
                         charge_amount, payment_amount, adjustment_amount, cagc, carc_code, rarc_code,
                         adjustment_reason, denial_date, status, appeal_deadline)
                    VALUES ((SELECT id FROM claims WHERE claim_number = $1), $2, $3, $4, $5, $6,
                            $7, $8, $9, $10, $11, $12, $13, $14, 'open',
                            -- The filing clock starts at the remittance date and runs
                            -- for the payer's window; see migration 006. Computed in
                            -- SQL so ingestion, backfill and recompute cannot disagree.
                            appeal_deadline_for(
                                (SELECT payer_name FROM claims WHERE claim_number = $1),
                                COALESCE($14, CURRENT_DATE)))
                    -- The same claim can legitimately arrive in two different files
                    -- (a corrected 837 after an 835, say). The hash check cannot see
                    -- that; this does. Skipping the row is right - the adjustment is
                    -- already recorded.
                    ON CONFLICT DO NOTHING
                    -- RETURNING is empty on a skipped row, which answers "was it
                    -- inserted?" directly. Parsing the "INSERT 0 1" status tag for
                    -- a trailing " 1" answered it by inference, and would miscount
                    -- silently if that tag ever changed shape.
                    RETURNING id
                    """,
                    denial_data["claim_id"],
                    denial_data["service_line_number"],
                    denial_data["cpt_code"],
                    denial_data["hcpcs_code"],
                    denial_data["modifier_1"],
                    denial_data["modifier_2"],
                    denial_data["charge_amount"],
                    denial_data["payment_amount"],
                    denial_data["adjustment_amount"],
                    denial_data["cagc"],
                    denial_data["carc_code"],
                    denial_data["rarc_code"],
                    denial_data["denial_reason"],
                    denial_data["denial_date"],
                )
                denials_written += 1 if inserted else 0

            # Reflect adjudication on the claim itself. Ingestion hardcodes
            # status 'parsed' and nothing ever marked a claim denied, so the
            # dashboard's "Denied Claims" card read 0 no matter what was loaded.
            if cleaned_denials:
                await conn.execute(
                    """
                    UPDATE claims c
                    SET status = CASE WHEN c.total_paid > 0 THEN 'partially_paid' ELSE 'denied' END,
                        updated_at = NOW()
                    WHERE c.claim_number = ANY($1::text[])
                      AND EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id)
                    """,
                    [d["claim_id"] for d in cleaned_denials],
                )

            return {
                "status": "stored",
                "ingestion_id": str(ingestion_row["id"]),
                "file_name": file.filename or "unknown",
                "claims_stored": len(cleaned_claims),
                "denials_stored": denials_written,
                "denials_skipped_as_duplicates": len(cleaned_denials) - denials_written,
            }


@router.post("/ingestion/log", response_model=list[dict])
async def list_ingestion_log(
    limit: int = Query(100, ge=1, le=1000),
):
    """List ingestion history (POST variant)"""
    async with get_connection() as conn:
        rows = await conn.fetch(
            "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at FROM ingestion_log ORDER BY created_at DESC LIMIT $1", limit
        )
        return [dict(r) for r in rows]


@router.get("/ingestion/history", response_model=list[dict])
async def get_ingestion_history(
    limit: int = Query(50, ge=1, le=500),
):
    """List ingestion history (GET - for frontend)"""
    async with get_connection() as conn:
        rows = await conn.fetch(
            "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at FROM ingestion_log ORDER BY created_at DESC LIMIT $1", limit
        )
        return [dict(r) for r in rows]


@router.post("/ingestion/store", response_model=dict)
async def store_parsed_data(payload: StoreIngestion):
    """Store parsed claims and denials from EDI parser.

    The watcher re-reads the dropzone after a restart, so this path needs the
    same guard as the upload endpoint or a container restart re-ingests
    everything sitting there.
    """
    async with get_connection() as conn:
        previous = await _already_ingested(conn, payload.file_hash)
    if previous:
        logger.info(
            f"Skipping {payload.file_name}: identical to ingest {previous['id']} "
            f"({previous['created_at']:%Y-%m-%d %H:%M})"
        )
        return {
            "status": "skipped_duplicate",
            "file_name": payload.file_name,
            "previous_ingestion_id": str(previous["id"]),
            "claims_stored": 0,
            "denials_stored": 0,
        }

    seen_numbers = set()
    cleaned_claims = [_clean_claim(c, i, seen_numbers) for i, c in enumerate(payload.claims)]
    cleaned_denials = [_clean_denial(d) for d in payload.denials]
    transaction_type = payload.transaction_type

    async with get_connection() as conn:
        # Store ingestion log
        ingestion_row = await conn.fetchrow(
            """
            INSERT INTO ingestion_log
                (file_name, file_size_bytes, file_hash, status, claims_count, denials_count)
            VALUES ($1, $2, $3, 'completed', $4, $5)
            RETURNING id
            """,
            payload.file_name, payload.file_size, payload.file_hash,
            len(cleaned_claims), len(cleaned_denials),
        )

        # Store cleaned claims
        for claim_data in cleaned_claims:
            await conn.execute(
                _claim_upsert_sql(transaction_type),
                claim_data["claim_id"],
                claim_data["patient_id"],
                claim_data["patient_name"],
                claim_data["date_of_birth"],
                claim_data["provider_npi"],
                claim_data["provider_name"],
                claim_data["payer_name"],
                claim_data["payer_id_number"],
                claim_data["total_charged"],
                claim_data["total_paid"],
                claim_data["total_adjustment"],
                claim_data["claim_type"],
                claim_data["service_from"],
                claim_data["service_to"],
                claim_data["diagnosis_codes"],
                json.dumps(claim_data, default=str),
            )

        # Store cleaned denials
        denials_written = 0
        for denial_data in cleaned_denials:
            # ON CONFLICT DO NOTHING means "offered" and "stored" are different
            # numbers. Reporting the first as the second told the user five
            # denials had been stored when none had.
            result = await conn.execute(
                """
                INSERT INTO denials
                    (claim_id, service_line_number, cpt_code, hcpcs_code, modifier_1, modifier_2,
                     charge_amount, payment_amount, adjustment_amount, cagc, carc_code, rarc_code,
                     adjustment_reason, denial_date, status, appeal_deadline)
                VALUES ((SELECT id FROM claims WHERE claim_number = $1), $2, $3, $4, $5, $6,
                        $7, $8, $9, $10, $11, $12, $13, $14, 'open',
                        -- The filing clock starts at the remittance date and runs
                        -- for the payer's window; see migration 006. Computed in
                        -- SQL so ingestion, backfill and recompute cannot disagree.
                        appeal_deadline_for(
                            (SELECT payer_name FROM claims WHERE claim_number = $1),
                            COALESCE($14, CURRENT_DATE)))
                -- The same claim can legitimately arrive in two different files
                -- (a corrected 837 after an 835, say). The hash check cannot see
                -- that; this does. Skipping the row is right - the adjustment is
                -- already recorded.
                ON CONFLICT DO NOTHING
                """,
                denial_data["claim_id"],
                denial_data["service_line_number"],
                denial_data["cpt_code"],
                denial_data["hcpcs_code"],
                denial_data["modifier_1"],
                denial_data["modifier_2"],
                denial_data["charge_amount"],
                denial_data["payment_amount"],
                denial_data["adjustment_amount"],
                denial_data["cagc"],
                denial_data["carc_code"],
                denial_data["rarc_code"],
                denial_data["denial_reason"],
                denial_data["denial_date"],
            )
            denials_written += 1 if result.endswith(" 1") else 0

        # Reflect adjudication on the claim itself. Ingestion hardcodes
        # status 'parsed' and nothing ever marked a claim denied, so the
        # dashboard's "Denied Claims" card read 0 no matter what was loaded.
        if cleaned_denials:
            await conn.execute(
                """
                UPDATE claims c
                SET status = CASE WHEN c.total_paid > 0 THEN 'partially_paid' ELSE 'denied' END,
                    updated_at = NOW()
                WHERE c.claim_number = ANY($1::text[])
                  AND EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id)
                """,
                [d["claim_id"] for d in cleaned_denials],
            )

        return {
            "status": "stored",
            "ingestion_id": str(ingestion_row["id"]),
            "file_name": payload.file_name,
            "claims_stored": len(cleaned_claims),
            "denials_stored": denials_written,
            "denials_skipped_as_duplicates": len(cleaned_denials) - denials_written,
        }
