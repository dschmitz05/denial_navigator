"""API Gateway — audit log retention.

The audit log grows without limit and nothing pruned it. HIPAA does not name a
number, but it does expect a stated policy: most covered entities keep six
years, matching the documentation retention requirement in §164.316(b)(2).

Pruning an audit trail is itself a sensitive act, so it is deliberate rather
than automatic: admin-only, it refuses to delete anything inside the retention
window, and it records what it removed - including the date range - as an audit
entry of its own, which is the one row that survives to say the prune happened.
"""

import json
import logging
import os
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field

from api_gateway.services.db import get_connection
from api_gateway.services.auth import get_current_user
from api_gateway.services.audit import _client_ip, _identify, record as audit_record

logger = logging.getLogger("api_gateway.retention")

router = APIRouter()

# Six years, the usual figure for HIPAA documentation retention. Override for a
# state or contract that requires longer; the floor below stops it being set to
# something that would discard evidence.
DEFAULT_RETENTION_DAYS = int(os.environ.get("AUDIT_RETENTION_DAYS", "2190"))
MINIMUM_RETENTION_DAYS = 365


def require_admin(current_user: dict = Depends(get_current_user)) -> dict:
    if current_user.get("role") != "admin":
        raise HTTPException(status_code=403, detail="Admin access required")
    return current_user


class PruneRequest(BaseModel):
    older_than_days: int = Field(DEFAULT_RETENTION_DAYS, ge=MINIMUM_RETENTION_DAYS)
    confirm: bool = False


@router.get("/retention/audit", response_model=dict)
async def audit_retention_status(current_user = Depends(require_admin)):
    """How much log there is, how old it is, and what a prune would remove."""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            f"""
            SELECT COUNT(*) AS total,
                   MIN(created_at) AS oldest,
                   MAX(created_at) AS newest,
                   COUNT(*) FILTER (
                       WHERE created_at < NOW() - INTERVAL '{DEFAULT_RETENTION_DAYS} days'
                   ) AS beyond_retention,
                   pg_size_pretty(pg_total_relation_size('audit_log')) AS on_disk
              FROM audit_log
            """
        )
    return {
        "retention_days": DEFAULT_RETENTION_DAYS,
        "retention_years": round(DEFAULT_RETENTION_DAYS / 365, 1),
        "minimum_allowed_days": MINIMUM_RETENTION_DAYS,
        "total_entries": row["total"],
        "oldest_entry": row["oldest"].isoformat() if row["oldest"] else None,
        "newest_entry": row["newest"].isoformat() if row["newest"] else None,
        "entries_beyond_retention": row["beyond_retention"],
        "on_disk": row["on_disk"],
        "note": (
            "Nothing is deleted automatically. Prune deliberately, and keep an "
            "exported copy if your policy requires the history beyond this window."
        ),
    }


@router.post("/retention/audit/prune", response_model=dict)
async def prune_audit_log(
    body: PruneRequest,
    http_request: Request,
    current_user = Depends(require_admin),
):
    """Delete audit entries older than the retention window.

    Requires confirm=true. Refuses a window shorter than a year, so a mistyped
    number cannot quietly destroy the trail.
    """
    if not body.confirm:
        raise HTTPException(
            status_code=400,
            detail="Send confirm=true. This permanently deletes audit history.",
        )

    async with get_connection() as conn:
        preview = await conn.fetchrow(
            f"""SELECT COUNT(*) AS n, MIN(created_at) AS from_date, MAX(created_at) AS to_date
                  FROM audit_log
                 WHERE created_at < NOW() - INTERVAL '{body.older_than_days} days'"""
        )
        if not preview["n"]:
            return {"status": "nothing_to_prune", "older_than_days": body.older_than_days,
                    "deleted": 0}

        deleted = await conn.execute(
            f"DELETE FROM audit_log WHERE created_at < NOW() - INTERVAL '{body.older_than_days} days'"
        )

    actor_id, actor_name = _identify(http_request)
    # Written after the delete so it cannot itself be removed by the same
    # statement. This entry is the only remaining record that the prune ran.
    await audit_record(
        action="audit_pruned",
        resource_type="audit_log",
        user_id=actor_id,
        details={
            "username": actor_name or "anonymous",
            "older_than_days": body.older_than_days,
            "deleted": preview["n"],
            "covered_from": preview["from_date"].isoformat() if preview["from_date"] else None,
            "covered_to": preview["to_date"].isoformat() if preview["to_date"] else None,
        },
        ip_address=_client_ip(http_request),
        user_agent=http_request.headers.get("user-agent"),
    )
    logger.warning(f"audit log pruned by {actor_name}: {preview['n']} entries removed")

    return {
        "status": "pruned",
        "older_than_days": body.older_than_days,
        "deleted": preview["n"],
        "covered_from": preview["from_date"].isoformat() if preview["from_date"] else None,
        "covered_to": preview["to_date"].isoformat() if preview["to_date"] else None,
    }
