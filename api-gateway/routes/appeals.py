"""API Gateway — Appeals routes"""

import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel, Field

from api_gateway.services.db import get_connection

logger = logging.getLogger("api_gateway.appeals")

router = APIRouter()


class AppealCreate(BaseModel):
    denial_id: str
    ai_analysis_id: Optional[str] = None
    resolution_type: str = Field(..., pattern="^(appeal_letter|corrected_claim|clinical_docs|payer_contact|write_off)$")
    assigned_user_id: Optional[str] = None
    notes: Optional[str] = None


class AppealUpdate(BaseModel):
    outcome_status: Optional[str] = None
    notes: Optional[str] = None
    submitted_at: Optional[str] = None
    payer_response: Optional[str] = None
    payer_response_text: Optional[str] = None
    final_outcome: Optional[str] = None


@router.get("/appeals", response_model=list[dict])
async def list_appeals(
    outcome_status: str = Query(None),
    resolution_type: str = Query(None),
    assigned_user_id: str = Query(None),
    limit: int = Query(50),
    offset: int = Query(0),
):
    """List appeals queue items"""
    async with get_connection() as conn:
        query = """
            SELECT aq.*, d.cpt_code, d.carc_code, d.charge_amount,
                   c.claim_number, c.patient_name, c.payer_name
            FROM appeals_queue aq
            JOIN denials d ON d.id = aq.denial_id
            JOIN claims c ON c.id = d.claim_id
        """
        params = []
        where_count = 0

        filters = []
        if outcome_status:
            filters.append(f"aq.outcome_status = ${where_count + 1}")
            params.append(outcome_status)
            where_count += 1
        if resolution_type:
            filters.append(f"aq.resolution_type = ${where_count + 1}")
            params.append(resolution_type)
            where_count += 1
        if assigned_user_id:
            filters.append(f"aq.assigned_user_id = ${where_count + 1}")
            params.append(assigned_user_id)
            where_count += 1

        if filters:
            query += " WHERE " + " AND ".join(filters)

        query += " ORDER BY aq.created_at ASC LIMIT $%d OFFSET $%d" % (where_count + 1, where_count + 2)
        params.extend([limit, offset])

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.post("/appeals", response_model=dict, status_code=201)
async def create_appeal(appeal: AppealCreate):
    """Create an appeal queue item"""
    async with get_connection() as conn:
        denial = await conn.fetchrow("SELECT claim_id FROM denials WHERE id = $1", appeal.denial_id)
        if not denial:
            raise HTTPException(status_code=404, detail="Denial not found")

        # One open item per denial. Without this, clicking "Queue appeal"
        # twice silently creates two queue entries for the same work.
        existing = await conn.fetchrow(
            """
            SELECT id FROM appeals_queue
            WHERE denial_id = $1
              AND (outcome_status IS NULL
                   OR outcome_status NOT IN ('approved', 'overruled', 'resolved', 'cancelled'))
            """,
            appeal.denial_id,
        )
        if existing:
            raise HTTPException(
                status_code=409,
                detail=f"An open appeal already exists for this denial ({existing['id']})",
            )

        # Link the analysis that justified this appeal, so the feedback loop
        # can later record whether its recommendation actually worked.
        analysis_id = appeal.ai_analysis_id
        if analysis_id is None:
            analysis_id = await conn.fetchval(
                "SELECT id FROM ai_analyses WHERE denial_id = $1 ORDER BY created_at DESC LIMIT 1",
                appeal.denial_id,
            )

        row = await conn.fetchrow(
            """
            INSERT INTO appeals_queue
                (denial_id, claim_id, ai_analysis_id, resolution_type,
                 assigned_user_id, notes, outcome_status)
            VALUES ($1, $2, $3, $4, $5, $6, 'queued')
            RETURNING *
            """,
            appeal.denial_id, denial["claim_id"], analysis_id,
            appeal.resolution_type, appeal.assigned_user_id, appeal.notes,
        )

        # Reflect it on the denial so the two views cannot disagree.
        await conn.execute(
            "UPDATE denials SET status = 'in_appeal', updated_at = NOW() WHERE id = $1",
            appeal.denial_id,
        )
        return dict(row)


@router.patch("/appeals/{appeal_id}", response_model=dict)
async def update_appeal(appeal_id: str, appeal: AppealUpdate):
    """Update an appeal"""
    async with get_connection() as conn:
        updates = []
        params = []
        idx = 1

        if appeal.outcome_status:
            updates.append(f"outcome_status = ${idx}")
            params.append(appeal.outcome_status)
            idx += 1
        if appeal.notes:
            updates.append(f"notes = ${idx}")
            params.append(appeal.notes)
            idx += 1
        if appeal.submitted_at:
            updates.append(f"submitted_at = ${idx}")
            params.append(appeal.submitted_at)
            idx += 1
        if appeal.payer_response:
            updates.append(f"payer_response = ${idx}")
            params.append(appeal.payer_response)
            idx += 1
        if appeal.payer_response_text:
            updates.append(f"payer_response_text = ${idx}")
            params.append(appeal.payer_response_text)
            idx += 1
        if appeal.final_outcome:
            updates.append(f"final_outcome = ${idx}")
            params.append(appeal.final_outcome)
            idx += 1

        updates.append("updated_at = NOW()")
        params.append(appeal_id)

        row = await conn.fetchrow(
            f"UPDATE appeals_queue SET {', '.join(updates)} WHERE id = ${idx} RETURNING *",
            *params,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Appeal not found")
        return dict(row)


@router.get("/appeals/{appeal_id}/letter")
async def get_appeal_letter(appeal_id: str):
    """Get the draft appeal letter for an appeal"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            SELECT d.cpt_code, d.carc_code, d.charge_amount,
                   c.claim_number, c.patient_name, c.patient_id,
                   c.date_of_birth, c.payer_name, c.payer_id_number,
                   c.icd_10_codes, c.service_from, c.service_to,
                   aa.draft_appeal_letter, aa.explanation
            FROM appeals_queue aq
            JOIN denials d ON d.id = aq.denial_id
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN ai_analyses aa ON aa.denial_id = aq.denial_id
            WHERE aq.id = $1
            """,
            appeal_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Appeal not found")
        return dict(row)
