"""API Gateway — Appeals routes

The appeals_queue table backs TWO operational views, split by resolution_type:

  * Appeals  — work that formally challenges the payer's decision.
  * Worklist — everything else: corrected claims, clinical documentation,
    payer phone calls, write-offs.

Only an appeal belongs on the Appeals tab. A clinical-docs task queued there
inflates the appeal count, gets chased for a payer response that will never
come, and buries the letters that genuinely need sending. The split is defined
here, once, so the two tabs cannot drift apart or double-count an item.
"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query, Request
from pydantic import BaseModel, Field

from api_gateway.services.db import get_connection
from api_gateway.services.claim_status import refresh_claim_status
from api_gateway.services.audit import _client_ip, _identify, record as audit_record
from api_gateway.services.access import SPECIALIST, principal

logger = logging.getLogger("api_gateway.appeals")

router = APIRouter()

# The one resolution type that is an actual appeal.
APPEAL_RESOLUTION_TYPES = ("appeal_letter",)

# Everything else is denial work, not an appeal. Defined as "not an appeal"
# in SQL rather than as a list, so a resolution type added later shows up on
# the Worklist by default instead of silently vanishing from both tabs.
WORKLIST_RESOLUTION_TYPES = (
    "corrected_claim", "clinical_docs", "payer_contact", "bill_patient", "write_off",
)

def _queue_owner(http_request) -> Optional[str]:
    """The user a caller is restricted to, or None for unrestricted access.

    A billing specialist works their own queue. Managers, directors and admins
    see everything, because distributing and reviewing the work is their job.

    "Their own queue" deliberately includes UNASSIGNED items. Nothing assigns
    work automatically today, so every item created before this change has a
    NULL owner - a strict assigned-to-me filter would show every specialist an
    empty queue and make real work invisible. Unassigned items are the shared
    pool everyone can pick from; items owned by someone else are hidden.
    """
    who = getattr(http_request.state, "principal", None) or principal(http_request)
    if who.kind == "user" and who.role == SPECIALIST:
        return who.user_id
    return None


def _assert_may_touch(http_request, row) -> None:
    """Refuse a specialist access to an item that belongs to someone else.

    Filtering the list is not enough on its own: without this, a specialist
    who knew or guessed an id could still open and close a colleague's work.
    """
    if row is None:
        return  # a missing row is a 404 for everyone, handled by the caller
    owner = _queue_owner(http_request)
    if owner and row["assigned_user_id"] is not None and str(row["assigned_user_id"]) != owner:
        raise HTTPException(
            status_code=403,
            detail="This queue item is assigned to another user",
        )


# Outcomes that close a queue item, whichever tab it lives on.
TERMINAL_OUTCOMES = ("approved", "overruled", "resolved", "denied_again", "cancelled")

# Terminal outcomes that mean the work succeeded.
SUCCESS_OUTCOMES = ("approved", "overruled", "resolved")


class AppealCreate(BaseModel):
    denial_id: str
    ai_analysis_id: Optional[str] = None
    resolution_type: str = Field(..., pattern="^(appeal_letter|corrected_claim|clinical_docs|payer_contact|bill_patient|write_off)$")
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
    category: str = Query(
        None,
        description="'appeal' for the Appeals tab, 'worklist' for non-appeal denial work",
    ),
    assigned_user_id: str = Query(None),
    limit: int = Query(50),
    offset: int = Query(0),
    http_request: Request = None,
):
    """List queue items. `category` splits appeals from other denial work."""
    if category not in (None, "appeal", "worklist"):
        raise HTTPException(
            status_code=400,
            detail="category must be 'appeal' or 'worklist'",
        )
    async with get_connection() as conn:
        query = """
            SELECT aq.*, d.cpt_code, d.carc_code, d.charge_amount,
                   c.claim_number, c.patient_name, c.payer_name,
                   aa.needs_appeal,
                   assignee.username AS assigned_username
            FROM appeals_queue aq
            JOIN denials d ON d.id = aq.denial_id
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id
            LEFT JOIN users assignee ON assignee.id = aq.assigned_user_id
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
        if category == "appeal":
            filters.append(f"aq.resolution_type = ANY(${where_count + 1}::text[])")
            params.append(list(APPEAL_RESOLUTION_TYPES))
            where_count += 1
        elif category == "worklist":
            # NOT an appeal, and never NULL-dropped by the comparison.
            filters.append(
                f"(aq.resolution_type IS NULL OR NOT (aq.resolution_type = ANY(${where_count + 1}::text[])))"
            )
            params.append(list(APPEAL_RESOLUTION_TYPES))
            where_count += 1
        if assigned_user_id:
            filters.append(f"aq.assigned_user_id = ${where_count + 1}")
            params.append(assigned_user_id)
            where_count += 1

        # Row-level scope. Applied last and unconditionally, so no combination
        # of query parameters can widen a specialist's view.
        owner = _queue_owner(http_request)
        if owner:
            filters.append(
                f"(aq.assigned_user_id = ${where_count + 1}::uuid OR aq.assigned_user_id IS NULL)"
            )
            params.append(owner)
            where_count += 1

        # Only show open appeals by default (filter out resolved/cancelled)
        open_status_filter = "aq.outcome_status IS NULL OR aq.outcome_status NOT IN ('approved', 'overruled', 'resolved', 'denied_again', 'cancelled')"

        if filters:
            # If filtering by a specific outcome_status, don't add the open status filter
            if outcome_status:
                query += " WHERE " + " AND ".join(filters)
            else:
                query += " WHERE " + " AND ".join(filters) + f" AND ({open_status_filter})"
        else:
            query += f" WHERE {open_status_filter}"

        query += " ORDER BY aq.created_at ASC LIMIT $%d OFFSET $%d" % (where_count + 1, where_count + 2)
        params.extend([limit, offset])

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.post("/appeals", response_model=dict, status_code=201)
async def create_appeal(appeal: AppealCreate, http_request: Request):
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

        # A specialist who queues work owns it, so it lands in their queue
        # rather than the shared pool. Managers and directors distribute work,
        # so what they queue stays unassigned unless they name an assignee.
        assigned_to = appeal.assigned_user_id
        if assigned_to is None and _queue_owner(http_request):
            assigned_to = _queue_owner(http_request)

        row = await conn.fetchrow(
            """
            INSERT INTO appeals_queue
                (denial_id, claim_id, ai_analysis_id, resolution_type,
                 assigned_user_id, notes, outcome_status)
            VALUES ($1, $2, $3, $4, $5::uuid, $6, 'queued')
            RETURNING *
            """,
            appeal.denial_id, denial["claim_id"], analysis_id,
            appeal.resolution_type, assigned_to, appeal.notes,
        )

        # Reflect it on the denial so the two views cannot disagree. A
        # corrected claim or a records request is work in progress, NOT an
        # appeal - calling it 'in_appeal' is what made the denials list read
        # as though everything had been appealed.
        denial_status = (
            "in_appeal"
            if appeal.resolution_type in APPEAL_RESOLUTION_TYPES
            else "in_progress"
        )
        await conn.execute(
            "UPDATE denials SET status = $2, updated_at = NOW() WHERE id = $1",
            appeal.denial_id, denial_status,
        )
        await refresh_claim_status(conn, denial["claim_id"])
        return dict(row)


@router.patch("/appeals/{appeal_id}", response_model=dict)
async def update_appeal(appeal_id: str, appeal: AppealUpdate, http_request: Request):
    """Update an appeal"""
    async with get_connection() as conn:
        # Get current outcome before updating
        old_row = await conn.fetchrow(
            "SELECT outcome_status, assigned_user_id FROM appeals_queue WHERE id = $1",
            appeal_id,
        )
        old_outcome = old_row["outcome_status"] if old_row else None
        _assert_may_touch(http_request, old_row)

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

        if not updates:
            raise HTTPException(status_code=400, detail="No fields to update")

        updates.append("updated_at = NOW()")
        params.append(appeal_id)

        row = await conn.fetchrow(
            f"UPDATE appeals_queue SET {', '.join(updates)} WHERE id = ${idx} RETURNING *",
            *params,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Appeal not found")

        # The middleware records that someone called this endpoint; this row
        # records WHAT CHANGED, attributed to the same person and address.
        actor_id, actor_name = _identify(http_request)
        await audit_record(
            action="appeal_updated",
            resource_type="appeal",
            resource_id=str(row["id"]),
            user_id=actor_id,
            details={
                "username": actor_name or "anonymous",
                "old_outcome": old_outcome,
                "outcome_status": row["outcome_status"],
                "resolution_type": row["resolution_type"],
                "denial_id": str(row["denial_id"]),
                "claim_id": str(row["claim_id"]),
            },
            ip_address=_client_ip(http_request),
            user_agent=http_request.headers.get("user-agent"),
        )

        # Close the denial out in terms of the work that was actually done.
        # A resolved corrected claim was never "appealed", and a write-off is
        # a write-off - reporting them all as appealed overstates the appeal
        # win rate the feedback loop is supposed to measure.
        if old_outcome != row["outcome_status"] and row["outcome_status"] in TERMINAL_OUTCOMES:
            if row["outcome_status"] not in SUCCESS_OUTCOMES:
                # denied_again / cancelled: back into the denials queue.
                denial_status = "analyzed"
            elif row["resolution_type"] == "write_off":
                denial_status = "written_off"
            elif row["resolution_type"] in APPEAL_RESOLUTION_TYPES:
                denial_status = "appealed"
            else:
                denial_status = "resolved"
            await conn.execute(
                "UPDATE denials SET status = $2, updated_at = NOW() WHERE id = $1",
                row["denial_id"], denial_status,
            )
            # Closing the last open denial closes the claim; reopening one
            # (cancelled, denied again) reopens the claim with it.
            await refresh_claim_status(conn, row["claim_id"])

        return dict(row)


@router.get("/appeals/{appeal_id}/letter")
async def get_appeal_letter(appeal_id: str, http_request: Request):
    """Get the draft appeal letter for an appeal"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            SELECT aq.assigned_user_id,
                   d.cpt_code, d.carc_code, d.charge_amount,
                   c.claim_number, c.patient_name, c.patient_id,
                   c.date_of_birth, c.payer_name, c.payer_id_number,
                   c.icd_10_codes, c.service_from, c.service_to,
                   aa.draft_appeal_letter, aa.explanation, aa.needs_appeal
            FROM appeals_queue aq
            JOIN denials d ON d.id = aq.denial_id
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id
            WHERE aq.id = $1
            """,
            appeal_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Appeal not found")
        _assert_may_touch(http_request, row)
        return dict(row)


def _json_field(value):
    """jsonb comes back from asyncpg as a string; hand the UI real structure."""
    if value is None or isinstance(value, (list, dict)):
        return value
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return value


@router.get("/appeals/{appeal_id}", response_model=dict)
async def get_appeal(appeal_id: str, http_request: Request):
    """Full context for one queue item.

    The Appeals tab needs a letter; the Worklist needs the action plan and the
    steps a biller has to carry out. This serves both so neither view has to
    stitch three endpoints together.
    """
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            SELECT aq.*,
                   assignee.username AS assigned_username,
                   d.cpt_code, d.carc_code, d.rarc_code, d.cagc,
                   d.charge_amount, d.adjustment_reason, d.appeal_deadline,
                   d.status AS denial_status,
                   c.claim_number, c.patient_name, c.patient_id, c.date_of_birth,
                   c.payer_name, c.payer_id_number, c.icd_10_codes,
                   c.service_from, c.service_to,
                   cc.description AS carc_description,
                   aa.explanation, aa.denial_category, aa.required_action,
                   aa.root_cause_summary, aa.action_plan, aa.steps,
                   aa.needs_appeal, aa.draft_appeal_letter, aa.confidence_score
            FROM appeals_queue aq
            JOIN denials d ON d.id = aq.denial_id
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id
            LEFT JOIN users assignee ON assignee.id = aq.assigned_user_id
            WHERE aq.id = $1
            """,
            appeal_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="Queue item not found")
        _assert_may_touch(http_request, row)

    item = dict(row)
    item["steps"] = _json_field(item.get("steps"))
    item["action_plan"] = _json_field(item.get("action_plan"))
    item["is_appeal"] = item.get("resolution_type") in APPEAL_RESOLUTION_TYPES
    return item


class AppealAssign(BaseModel):
    # None returns the item to the shared pool, where any specialist can
    # pick it up. That is a real operation, not a missing value.
    assigned_user_id: Optional[str] = None


@router.post("/appeals/{appeal_id}/assign", response_model=dict)
async def assign_appeal(appeal_id: str, body: AppealAssign, http_request: Request):
    """Assign a queue item to a user, or return it to the pool.

    Kept separate from PATCH /appeals/{id} rather than folded into it: routing
    someone else's work is a supervisory act, restricted to managers and above
    (see PATH_PERMISSIONS in services/access.py), while the ordinary status
    fields stay writable by the specialist doing the work. It also gives the
    audit trail a distinct action instead of burying a reassignment among
    status edits.
    """
    async with get_connection() as conn:
        current = await conn.fetchrow(
            """
            SELECT aq.id, aq.assigned_user_id, aq.resolution_type, aq.denial_id,
                   u.username AS current_username
            FROM appeals_queue aq
            LEFT JOIN users u ON u.id = aq.assigned_user_id
            WHERE aq.id = $1
            """,
            appeal_id,
        )
        if not current:
            raise HTTPException(status_code=404, detail="Queue item not found")

        assignee = None
        if body.assigned_user_id:
            assignee = await conn.fetchrow(
                "SELECT id, username, role, is_active FROM users WHERE id = $1",
                body.assigned_user_id,
            )
            if not assignee:
                raise HTTPException(status_code=404, detail="User not found")
            if not assignee["is_active"]:
                # Assigning to a deactivated account is how work disappears:
                # nobody can log in to see it, and it never shows as unowned.
                raise HTTPException(
                    status_code=400,
                    detail=f"{assignee['username']} is deactivated and cannot be assigned work",
                )

        row = await conn.fetchrow(
            """
            UPDATE appeals_queue
               SET assigned_user_id = $2::uuid, updated_at = NOW()
             WHERE id = $1
            RETURNING *
            """,
            appeal_id, str(assignee["id"]) if assignee else None,
        )

        actor_id, actor_name = _identify(http_request)
        await audit_record(
            action="assign_appeal",
            resource_type="appeal",
            resource_id=str(appeal_id),
            user_id=actor_id,
            details={
                "username": actor_name or "anonymous",
                "from": current["current_username"],
                "to": assignee["username"] if assignee else None,
                "resolution_type": current["resolution_type"],
            },
            ip_address=_client_ip(http_request),
            user_agent=http_request.headers.get("user-agent"),
        )

    result = dict(row)
    result["assigned_username"] = assignee["username"] if assignee else None
    return result
