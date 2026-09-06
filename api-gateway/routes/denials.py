"""API Gateway — Denials routes"""

import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.claim_status import refresh_claim_status

logger = logging.getLogger("api_gateway.denials")

router = APIRouter()

# What the denials list shows when no status filter is chosen: denials that
# still need someone to do something. A denial queued to Appeals or the
# Worklist ('in_appeal' / 'in_progress'), or already closed out, is being
# handled elsewhere. The CARC filter counts MUST use the same definition -
# counting every denial ever ingested made the dropdown promise rows the
# table would not show.
ACTIVE_DENIAL_STATUSES = ("open", "analyzed")

# The AI's required_action, turned into a unit of queue work.
RESOLUTION_FOR_ACTION = {
    "appeal": "appeal_letter",
    "coding_correction": "corrected_claim",
    "clinical_documentation": "clinical_docs",
    "bill_patient": "bill_patient",
    "no_action_required": "write_off",
}


def recommended_resolution(cagc: Optional[str], required_action: Optional[str]) -> Optional[str]:
    """Which queue action to offer for a denial.

    X12 defines PR as Patient Responsibility: the payer has assigned that
    balance to the PATIENT. It is money the practice is still entitled to
    collect, so it can never be a write-off - a deductible recommended as
    "write off" tells the billing team to abandon collectible revenue.

    Older analyses have no bill_patient value to give, because the prompt only
    offered no_action_required for both a patient balance and a contractual
    write-off. This rule corrects those without re-running the model, and acts
    as a guard on future ones.
    """
    if cagc == "PR" and required_action in (None, "", "no_action_required"):
        return "bill_patient"
    return RESOLUTION_FOR_ACTION.get(required_action or "")


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
            SELECT DISTINCT ON (d.id) d.*, c.claim_number, c.patient_name, c.payer_name,
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
               AND (aq.outcome_status IS NULL
                    OR aq.outcome_status NOT IN ('approved', 'overruled', 'resolved', 'denied_again', 'cancelled'))
        """
        params = []
        where_count = 0

        filters = []
        if status:
            filters.append(f"d.status = ${where_count + 1}")
            params.append(status)
            where_count += 1
        else:
            filters.append(f"d.status = ANY(${where_count + 1}::text[])")
            params.append(list(ACTIVE_DENIAL_STATUSES))
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
            filters.append(f"d.claim_id = (SELECT id FROM claims WHERE claim_number = ${where_count + 1})")
            params.append(claim_id)
            where_count += 1
        if priority:
            filters.append("d.appeal_deadline IS NOT NULL AND d.appeal_deadline <= CURRENT_DATE + INTERVAL '14 days'")

        if filters:
            query += " WHERE " + " AND ".join(filters)

        query += " ORDER BY d.id, d.charge_amount DESC LIMIT $%d OFFSET $%d" % (where_count + 1, where_count + 2)
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
async def carc_options(status: str = Query(None)):
    """CARC codes present in the data, with counts, for the filter dropdown.

    `status` must match whatever the denials list is filtered by, so the count
    beside each code is exactly the number of rows selecting it will show.
    With no status the default is the same active set the list uses - denials
    already sent to Appeals or the Worklist are not counted, because picking
    that code would not surface them.
    """
    async with get_connection() as conn:
        if status:
            status_clause = "d.status = $1"
            params = [status]
        else:
            status_clause = "d.status = ANY($1::text[])"
            params = [list(ACTIVE_DENIAL_STATUSES)]

        rows = await conn.fetch(f"""
            SELECT d.carc_code,
                   COALESCE(cc.description, 'No description on file') AS description,
                   COUNT(*) AS denial_count
            FROM denials d
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            WHERE d.carc_code IS NOT NULL AND d.carc_code <> ''
              AND {status_clause}
            GROUP BY d.carc_code, cc.description
            ORDER BY COUNT(*) DESC, d.carc_code
        """, *params)
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
                   aq.id AS appeal_id, aq.outcome_status AS appeal_status,
                   aq.resolution_type AS appeal_resolution_type
            FROM denials d
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code
            LEFT JOIN ai_analyses aa ON aa.denial_id = d.id
            LEFT JOIN appeals_queue aq ON aq.denial_id = d.id
               AND (aq.outcome_status IS NULL
                    OR aq.outcome_status NOT IN ('approved', 'overruled', 'resolved', 'denied_again', 'cancelled'))
            WHERE d.id = $1
            """,
            denial_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Denial not found")
        denial = dict(row)
        denial["recommended_resolution"] = recommended_resolution(
            denial.get("cagc"), denial.get("required_action")
        )
        return denial


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

        # Editing a denial's status by hand has to move the claim as well,
        # or the Claims tab drifts out of step with the denial it summarises.
        if update.status:
            await refresh_claim_status(conn, row["claim_id"])
        return dict(row)
