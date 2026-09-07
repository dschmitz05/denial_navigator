"""API Gateway — deadline notifications.

Filing deadlines were calculated and then only shown to whoever happened to
open the dashboard. This turns them into something that arrives.

Two kinds:

  deadline_digest      to the person who owns the work: what of theirs is due
                       soon or already past.
  overdue_escalation   to managers: anything overdue that nobody owns, because
                       an unassigned overdue denial has no one to chase it.

Generation is idempotent by (user, kind, day). The digest runs from cron and
the API runs four workers, so "once a day" has to be a database constraint
rather than an assumption about how often this is called.
"""

import json
import logging
from uuid import UUID
import os
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.access import MANAGER_UP, principal
from api_gateway.services.audit import record as audit_record

logger = logging.getLogger("api_gateway.notifications")

router = APIRouter()

# How far ahead a digest looks. Matches the dashboard's priority window, so the
# two cannot tell a user different things about the same denial.
DIGEST_HORIZON_DAYS = int(os.environ.get("DEADLINE_DIGEST_DAYS", "14"))


def _me(http_request: Request) -> str:
    who = getattr(http_request.state, "principal", None) or principal(http_request)
    if who.kind != "user" or not who.user_id:
        raise HTTPException(status_code=401, detail="Not authenticated")
    return who.user_id


@router.get("/notifications", response_model=list[dict])
async def list_notifications(
    http_request: Request,
    unread_only: bool = Query(False),
    limit: int = Query(50, ge=1, le=200),
):
    """Your notifications. Nobody can read anyone else's."""
    user_id = _me(http_request)
    async with get_connection() as conn:
        rows = await conn.fetch(
            f"""
            SELECT id, kind, for_date, title, body, payload, read_at, created_at
              FROM notifications
             WHERE user_id = $1::uuid
               {"AND read_at IS NULL" if unread_only else ""}
             ORDER BY created_at DESC
             LIMIT $2
            """,
            user_id, limit,
        )
    out = []
    for r in rows:
        d = dict(r)
        d["id"] = str(d["id"])
        d["for_date"] = d["for_date"].isoformat() if d["for_date"] else None
        d["created_at"] = d["created_at"].isoformat() if d["created_at"] else None
        d["read_at"] = d["read_at"].isoformat() if d["read_at"] else None
        if isinstance(d.get("payload"), str):
            try:
                d["payload"] = json.loads(d["payload"])
            except ValueError:
                pass
        out.append(d)
    return out


@router.post("/notifications/{notification_id}/read", response_model=dict)
async def mark_read(notification_id: UUID, http_request: Request):
    """Mark one as read. Scoped to the owner, so an id is not enough."""
    user_id = _me(http_request)
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """UPDATE notifications SET read_at = NOW()
                WHERE id = $1::uuid AND user_id = $2::uuid AND read_at IS NULL
            RETURNING id""",
            notification_id, user_id,
        )
    return {"status": "read" if row else "not_found_or_already_read"}


@router.post("/notifications/read-all", response_model=dict)
async def mark_all_read(http_request: Request):
    user_id = _me(http_request)
    async with get_connection() as conn:
        result = await conn.execute(
            "UPDATE notifications SET read_at = NOW() WHERE user_id = $1::uuid AND read_at IS NULL",
            user_id,
        )
    return {"status": "ok", "marked": int(result.split()[-1])}


@router.post("/notifications/generate-digests", response_model=dict)
async def generate_digests(http_request: Request):
    """Build today's deadline digests.

    Called from cron (see scripts/send_deadline_digests.sh). Safe to call more
    than once: the unique index means a second run updates nothing rather than
    sending everyone a duplicate.
    """
    who = getattr(http_request.state, "principal", None) or principal(http_request)
    # A service credential or an admin. Not something a specialist triggers,
    # since it writes to everyone's notifications.
    if not (who.kind == "service" or who.role == "admin"):
        raise HTTPException(status_code=403, detail="Admin or service access required")

    created, escalated = 0, 0
    async with get_connection() as conn:
        # ── per-owner digests ──
        owners = await conn.fetch(
            f"""
            SELECT aq.assigned_user_id AS user_id,
                   COUNT(*) FILTER (WHERE d.appeal_deadline < CURRENT_DATE) AS overdue,
                   COUNT(*) FILTER (WHERE d.appeal_deadline >= CURRENT_DATE) AS upcoming,
                   MIN(d.appeal_deadline) AS soonest,
                   SUM(d.charge_amount) AS amount,
                   json_agg(json_build_object(
                       'claim_number', c.claim_number,
                       'deadline', d.appeal_deadline,
                       'amount', d.charge_amount
                   ) ORDER BY d.appeal_deadline) AS items
              FROM appeals_queue aq
              JOIN denials d ON d.id = aq.denial_id
              JOIN claims c ON c.id = d.claim_id
             WHERE aq.assigned_user_id IS NOT NULL
               AND (aq.outcome_status IS NULL
                    OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled'))
               AND d.appeal_deadline IS NOT NULL
               AND d.appeal_deadline <= CURRENT_DATE + INTERVAL '{DIGEST_HORIZON_DAYS} days'
             GROUP BY aq.assigned_user_id
            """
        )
        for row in owners:
            overdue, upcoming = row["overdue"], row["upcoming"]
            title = (
                f"{overdue} overdue and {upcoming} due within {DIGEST_HORIZON_DAYS} days"
                if overdue else
                f"{upcoming} filing deadline(s) within {DIGEST_HORIZON_DAYS} days"
            )
            body = (
                f"Soonest is {row['soonest']}. "
                f"{float(row['amount'] or 0):,.2f} in denied charges is affected."
            )
            result = await conn.execute(
                """
                INSERT INTO notifications (user_id, kind, title, body, payload)
                VALUES ($1, 'deadline_digest', $2, $3, $4::jsonb)
                ON CONFLICT (user_id, kind, for_date) DO NOTHING
                """,
                row["user_id"], title, body,
                json.dumps({"overdue": overdue, "upcoming": upcoming,
                            "items": json.loads(row["items"]) if isinstance(row["items"], str) else row["items"]},
                           default=str),
            )
            created += 1 if result.endswith(" 1") else 0

        # ── unowned overdue work goes to managers ──
        orphan = await conn.fetchrow(
            """
            SELECT COUNT(*) AS n, SUM(d.charge_amount) AS amount, MIN(d.appeal_deadline) AS oldest
              FROM denials d
              LEFT JOIN appeals_queue aq ON aq.denial_id = d.id
                   AND (aq.outcome_status IS NULL
                        OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled'))
             WHERE d.appeal_deadline IS NOT NULL
               AND d.appeal_deadline < CURRENT_DATE
               AND d.status IN ('open', 'analyzed')
               AND (aq.id IS NULL OR aq.assigned_user_id IS NULL)
            """
        )
        if orphan and orphan["n"]:
            managers = await conn.fetch(
                "SELECT id FROM users WHERE is_active AND role = ANY($1::text[])",
                list(MANAGER_UP),
            )
            for m in managers:
                result = await conn.execute(
                    """
                    INSERT INTO notifications (user_id, kind, title, body, payload)
                    VALUES ($1, 'overdue_escalation', $2, $3, $4::jsonb)
                    ON CONFLICT (user_id, kind, for_date) DO NOTHING
                    """,
                    m["id"],
                    f"{orphan['n']} overdue denial(s) with nobody assigned",
                    (f"Oldest deadline {orphan['oldest']}. "
                     f"{float(orphan['amount'] or 0):,.2f} in denied charges is unowned."),
                    json.dumps({"count": orphan["n"], "oldest": str(orphan["oldest"])}),
                )
                escalated += 1 if result.endswith(" 1") else 0

    await audit_record(
        action="deadline_digests_generated",
        resource_type="system",
        details={"digests": created, "escalations": escalated,
                 "horizon_days": DIGEST_HORIZON_DAYS},
    )
    return {"status": "ok", "digests_created": created, "escalations_created": escalated,
            "horizon_days": DIGEST_HORIZON_DAYS}
