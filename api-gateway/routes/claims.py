"""API Gateway — Claims routes"""

import logging
from uuid import UUID
from typing import Optional

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.claim_status import TERMINAL_DENIAL_STATUSES
from api_gateway.routes.appeals import APPEAL_RESOLUTION_TYPES, TERMINAL_OUTCOMES

logger = logging.getLogger("api_gateway.claims")

router = APIRouter()


class ClaimCreate(BaseModel):
    claim_number: str
    patient_id: str
    payer_name: str
    total_charge: float
    icd_10_codes: list[str] = []


class ClaimUpdate(BaseModel):
    status: Optional[str] = None
    total_paid: Optional[float] = None
    total_adjustment: Optional[float] = None


@router.get("/claims", response_model=list[dict])
async def list_claims(
    status: str = Query(None, description="Filter by claim status"),
    q: str = Query(None, description="Claim number, patient name or patient id"),
    limit: int = Query(50, ge=1, le=500),
    offset: int = Query(0, ge=0),
):
    """List claims with optional status filter.

    Returns open_denial_count alongside denial_count so the UI can show WHY a
    claim is not resolved yet. A claim only reaches 'resolved' once every one
    of its denials is closed, so a claim with resolved queue items against it
    can still, correctly, read 'partially_paid' - that difference is confusing
    unless the remaining work is visible on the row.
    """
    async with get_connection() as conn:
        # $1 is always the terminal-status array, so the optional filters
        # below can number themselves from the end of the params list.
        params = [list(TERMINAL_DENIAL_STATUSES)]
        query = """
            SELECT c.*,
                   COUNT(d.id) AS denial_count,
                   COUNT(d.id) FILTER (WHERE d.status <> ALL($1::text[])) AS open_denial_count
            FROM claims c
            LEFT JOIN denials d ON d.claim_id = c.id
        """

        where = []
        if status:
            params.append(status)
            where.append(f"c.status = ${len(params)}")
        if q and q.strip():
            # Looking one patient up beats paging the whole census - and the
            # audit log records a targeted lookup rather than a bulk browse.
            params.append(f"%{q.strip()}%")
            where.append(
                f"(c.claim_number ILIKE ${len(params)}"
                f" OR c.patient_name ILIKE ${len(params)}"
                f" OR c.patient_id ILIKE ${len(params)})"
            )
        if where:
            query += " WHERE " + " AND ".join(where)

        query += " GROUP BY c.id ORDER BY c.created_at DESC"
        params.append(limit)
        query += f" LIMIT ${len(params)}"
        params.append(offset)
        query += f" OFFSET ${len(params)}"

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.get("/claims/{claim_id}", response_model=dict)
async def get_claim(claim_id: UUID):
    """Get a single claim with its denials"""
    async with get_connection() as conn:
        row = await conn.fetchrow("SELECT * FROM claims WHERE id = $1", claim_id)
        if not row:
            raise HTTPException(status_code=404, detail="Claim not found")

        denials = await conn.fetch(
            "SELECT * FROM denials WHERE claim_id = $1 ORDER BY service_line_number", claim_id
        )
        analyses = await conn.fetch(
            "SELECT * FROM ai_analyses WHERE claim_id = $1 ORDER BY created_at DESC", claim_id
        )

        result = dict(row)
        result["denials"] = [dict(d) for d in denials]
        result["analyses"] = [dict(a) for a in analyses]
        return result


@router.post("/claims", response_model=dict, status_code=201)
async def create_claim(claim: ClaimCreate):
    """Create a new claim"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO claims (claim_number, patient_id, payer_name, total_charge, icd_10_codes, status)
            VALUES ($1, $2, $3, $4, $5, 'ingested')
            RETURNING *
            """,
            claim.claim_number, claim.patient_id, claim.payer_name,
            claim.total_charge, claim.icd_10_codes,
        )
        return dict(row)


@router.patch("/claims/{claim_id}", response_model=dict)
async def update_claim(claim_id: UUID, claim: ClaimUpdate):
    """Update a claim"""
    async with get_connection() as conn:
        updates = []
        params = []
        param_idx = 1

        if claim.status:
            updates.append(f"status = ${param_idx}")
            params.append(claim.status)
            param_idx += 1
        if claim.total_paid is not None:
            updates.append(f"total_paid = ${param_idx}")
            params.append(claim.total_paid)
            param_idx += 1
        if claim.total_adjustment is not None:
            updates.append(f"total_adjustment = ${param_idx}")
            params.append(claim.total_adjustment)
            param_idx += 1

        if not updates:
            raise HTTPException(status_code=400, detail="No updates provided")

        updates.append("updated_at = NOW()")
        params.append(claim_id)

        row = await conn.fetchrow(
            f"UPDATE claims SET {', '.join(updates)} WHERE id = ${param_idx} RETURNING *",
            *params,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Claim not found")
        return dict(row)


@router.get("/claims/dashboard/stats")
async def dashboard_stats():
    """Get dashboard statistics"""
    async with get_connection() as conn:
        total_claims = await conn.fetchval("SELECT COUNT(*) FROM claims")

        # A claim is denied if it actually carries denial lines. This used to
        # count `claims.status = 'denied'`, but ingestion writes 'parsed' and
        # nothing ever set 'denied', so the card read 0 forever.
        denied_claims = await conn.fetchval(
            "SELECT COUNT(DISTINCT claim_id) FROM denials"
        )

        total_denials = await conn.fetchval(
            "SELECT COUNT(*) FROM denials WHERE status = 'open'"
        )

        # "Pending" is anything not yet in a terminal state - including rows
        # with a NULL outcome_status. Counting only 'queued' missed every
        # appeal a biller had started working.
        #
        # Counted per TAB, because the card links to one. A single count over
        # the whole queue would send you to Appeals showing a number that
        # included worklist items. The terminal list is shared with
        # routes/appeals.py so the card and the page can never disagree - it
        # used to omit 'denied_again' here, which the Appeals list excludes.
        pending_appeals = await conn.fetchval(
            f"""
            SELECT COUNT(*) FROM appeals_queue
            WHERE (outcome_status IS NULL OR outcome_status <> ALL($1::text[]))
              AND resolution_type = ANY($2::text[])
            """,
            list(TERMINAL_OUTCOMES), list(APPEAL_RESOLUTION_TYPES),
        )

        pending_worklist = await conn.fetchval(
            f"""
            SELECT COUNT(*) FROM appeals_queue
            WHERE (outcome_status IS NULL OR outcome_status <> ALL($1::text[]))
              AND (resolution_type IS NULL
                   OR NOT (resolution_type = ANY($2::text[])))
            """,
            list(TERMINAL_OUTCOMES), list(APPEAL_RESOLUTION_TYPES),
        )

        # Financial impact is the money actually adjusted off on denial lines.
        # The old query summed claims.total_adjustment filtered on the same
        # never-set status, so it was always 0.00.
        total_denied = await conn.fetchval(
            "SELECT COALESCE(SUM(adjustment_amount), 0) FROM denials"
        ) or 0

        open_denied = await conn.fetchval(
            "SELECT COALESCE(SUM(adjustment_amount), 0) FROM denials WHERE status = 'open'"
        ) or 0

        financial = await conn.fetchrow(
            """
            SELECT
                COALESCE(SUM(total_charge), 0)     as total_charges,
                COALESCE(SUM(total_paid), 0)       as total_paid,
                COALESCE(SUM(total_adjustment), 0) as total_adjustments
            FROM claims
            WHERE id IN (SELECT DISTINCT claim_id FROM denials)
            """
        )

        return {
            "total_claims": total_claims,
            "denied_claims": denied_claims,
            "pending_denials": total_denials,
            "pending_appeals": pending_appeals,
            "pending_worklist": pending_worklist,
            "total_denied": float(total_denied),
            "open_denied": float(open_denied),
            "total_charges": float(financial["total_charges"] or 0),
            "total_paid": float(financial["total_paid"] or 0),
            "total_adjustments": float(financial["total_adjustments"] or 0),
        }
