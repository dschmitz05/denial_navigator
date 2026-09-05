"""API Gateway — Denials routes"""

import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection

logger = logging.getLogger("api_gateway.denials")

router = APIRouter()


class DenialUpdate(BaseModel):
    status: Optional[str] = None
    appeal_deadline: Optional[str] = None


@router.get("/denials", response_model=list[dict])
async def list_denials(
    status: str = Query(None),
    carc_code: str = Query(None),
    cagc: str = Query(None),
    claim_id: str = Query(None),
    priority: bool = Query(False),
    limit: int = Query(50, ge=1, le=500),
    offset: int = Query(0),
):
    """List denials with filters"""
    async with get_connection() as conn:
        query = """
            SELECT d.*, c.claim_number, c.patient_name, c.payer_name,
                   c.total_charge, cc.description as carc_description,
                   rc.description as rarc_description,
                   aa.explanation, aa.denial_category, aa.needs_appeal,
                   aq.outcome_status as appeal_status
            FROM denials d
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code
            LEFT JOIN ai_analyses aa ON aa.denial_id = d.id
            LEFT JOIN appeals_queue aq ON aq.denial_id = d.id
        """
        params = []
        where_count = 0

        filters = []
        if status:
            filters.append(f"d.status = ${where_count + 1}")
            params.append(status)
            where_count += 1
        if carc_code:
            filters.append(f"d.carc_code = ${where_count + 1}")
            params.append(carc_code)
            where_count += 1
        if cagc:
            filters.append(f"d.cagc = ${where_count + 1}")
            params.append(cagc)
            where_count += 1
        if claim_id:
            filters.append(f"d.claim_id = ${where_count + 1}")
            params.append(claim_id)
            where_count += 1
        if priority:
            filters.append("d.appeal_deadline IS NOT NULL AND d.appeal_deadline <= CURRENT_DATE + INTERVAL '14 days'")

        if filters:
            query += " WHERE " + " AND ".join(filters)

        query += " ORDER BY d.charge_amount DESC LIMIT $%d OFFSET $%d" % (where_count + 1, where_count + 2)
        params.extend([limit, offset])

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.get("/denials/bulk-carc")
async def denials_by_carc():
    """Get denial counts by CARC code for batch processing"""
    async with get_connection() as conn:
        rows = await conn.fetch("""
            SELECT
                d.carc_code,
                COALESCE(cc.description, 'Unknown') as carc_description,
                d.cagc,
                COUNT(*) as denial_count,
                SUM(d.charge_amount) as total_denied_amount,
                AVG(d.charge_amount) as avg_denial_amount
            FROM denials d
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            WHERE d.status IN ('open', 'analyzed')
            GROUP BY d.carc_code, cc.description, d.cagc
            ORDER BY total_denied_amount DESC
        """)
        return [dict(r) for r in rows]


@router.get("/denials/carc-options", response_model=list[dict])
async def carc_options():
    """CARC codes that actually appear in the data, for filter dropdowns.

    The UI used to hard-code a handful of codes, most of which never occur in
    real remittances. Driving the filter from the data means it can only ever
    offer codes that will actually match something.
    """
    async with get_connection() as conn:
        rows = await conn.fetch("""
            SELECT d.carc_code,
                   COALESCE(cc.description, 'No description on file') AS description,
                   COUNT(*) AS denial_count
            FROM denials d
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            WHERE d.carc_code IS NOT NULL AND d.carc_code <> ''
            GROUP BY d.carc_code, cc.description
            ORDER BY COUNT(*) DESC, d.carc_code
        """)
        return [dict(r) for r in rows]


@router.get("/denials/{denial_id}", response_model=dict)
async def get_denial(denial_id: str):
    """Get a single denial with full context"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            SELECT d.*, c.claim_number, c.patient_name, c.patient_id,
                   c.date_of_birth, c.payer_name, c.payer_id_number,
                   c.icd_10_codes, c.total_charge, c.total_paid,
                   cc.description as carc_description,
                   rc.description as rarc_description,
                   aa.id AS ai_analysis_id,
                   aa.explanation, aa.action_plan, aa.steps, aa.draft_appeal_letter,
                   aa.denial_category, aa.required_action, aa.needs_appeal,
                   aa.confidence_score,
                   aq.id AS appeal_id, aq.outcome_status AS appeal_status
            FROM denials d
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code
            LEFT JOIN ai_analyses aa ON aa.denial_id = d.id
            LEFT JOIN appeals_queue aq ON aq.denial_id = d.id
            WHERE d.id = $1
            """,
            denial_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Denial not found")
        return dict(row)


@router.patch("/denials/{denial_id}", response_model=dict)
async def update_denial(denial_id: str, update: DenialUpdate):
    """Update a denial"""
    async with get_connection() as conn:
        updates = []
        params = []
        idx = 1

        if update.status:
            updates.append(f"status = ${idx}")
            params.append(update.status)
            idx += 1
        if update.appeal_deadline:
            updates.append(f"appeal_deadline = ${idx}")
            params.append(update.appeal_deadline)
            idx += 1

        updates.append("updated_at = NOW()")
        params.append(denial_id)

        row = await conn.fetchrow(
            f"UPDATE denials SET {', '.join(updates)} WHERE id = ${idx} RETURNING *",
            *params,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Denial not found")
        return dict(row)
