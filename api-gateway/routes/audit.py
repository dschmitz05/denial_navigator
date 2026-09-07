"""API Gateway — Audit log routes"""

import json
import logging
from datetime import datetime
from typing import Optional

from fastapi import APIRouter, HTTPException, Depends, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.auth import get_current_user

logger = logging.getLogger("api_gateway.audit")

router = APIRouter()


def _serialize(row) -> dict:
    """Make one audit row JSON-safe.

    Two columns come back as Python objects the response model cannot encode:
    ip_address is INET, which asyncpg hands over as an ipaddress.IPv4Address,
    and details is jsonb, which it hands over as a raw string. The first one
    raised PydanticSerializationError and took the whole endpoint down with a
    500 - invisibly, because the page renders a failed fetch as "no entries".
    """
    entry = dict(row)

    if entry.get("ip_address") is not None:
        entry["ip_address"] = str(entry["ip_address"])

    for key in ("id", "user_id", "resource_id"):
        if entry.get(key) is not None:
            entry[key] = str(entry[key])

    details = entry.get("details")
    if isinstance(details, str):
        try:
            entry["details"] = json.loads(details)
        except (TypeError, ValueError):
            pass

    return entry


def require_admin_or_manager(current_user: dict = Depends(get_current_user)) -> dict:
    """Dependency that requires admin or billing_manager role"""
    if current_user.get("role") not in ("admin", "billing_manager", "rcm_director"):
        raise HTTPException(status_code=403, detail="Manager+ access required")
    return current_user


# Who performed an entry. Three kinds of actor end up in this table and only
# the first has a users row:
#   * a person          -> user_id joins to users.username
#   * a sibling service -> user_id NULL, "service:ediparser" in details
#   * an unauthenticated caller -> user_id NULL, "anonymous" in details
# A user deleted since the entry was written also falls through to details,
# which is what keeps their history attributable.
ACTOR_SQL = "COALESCE(u.username, al.details->>'username', 'system')"


@router.get("/audit/actors", response_model=list[dict])
async def list_audit_actors(current_user = Depends(require_admin_or_manager)):
    """Everyone who appears in the log, with entry counts, for the filter.

    Driven by the data rather than the users table, so the dropdown can only
    ever offer a name that will actually match something - and so services and
    anonymous callers, which have no users row, are selectable too.
    """
    async with get_connection() as conn:
        rows = await conn.fetch(
            f"""
            SELECT {ACTOR_SQL} AS actor,
                   COUNT(*) AS entry_count,
                   MAX(al.created_at) AS last_seen
            FROM audit_log al
            LEFT JOIN users u ON u.id = al.user_id
            GROUP BY {ACTOR_SQL}
            ORDER BY COUNT(*) DESC, actor
            """
        )
        return [
            {
                "actor": r["actor"],
                "entry_count": r["entry_count"],
                "last_seen": r["last_seen"].isoformat() if r["last_seen"] else None,
            }
            for r in rows
        ]


@router.get("/audit", response_model=list[dict])
async def list_audit_log(
    action: Optional[str] = Query(None),
    resource_type: Optional[str] = Query(None),
    user_id: Optional[str] = Query(None),
    username: Optional[str] = Query(None, description="Actor name, including 'service:*' and 'anonymous'"),
    # Typed, so a query string becomes a datetime before it reaches asyncpg -
    # /audit?start_date=... returned 500 on the compliance log's own filter.
    start_date: Optional[datetime] = Query(None),
    end_date: Optional[datetime] = Query(None),
    limit: int = Query(200, ge=1, le=1000),
    offset: int = Query(0, ge=0),
    current_user = Depends(require_admin_or_manager),
):
    """List audit log entries (admin/manager only)"""
    async with get_connection() as conn:
        query = f"""
            SELECT al.id, al.user_id, u.username, al.action,
                   al.resource_type, al.resource_id, al.details,
                   al.ip_address, al.user_agent, al.created_at,
                   {ACTOR_SQL} AS actor
            FROM audit_log al
            LEFT JOIN users u ON u.id = al.user_id
        """
        params = []
        where_count = 0

        if action:
            query += f"{' WHERE' if where_count == 0 else ' AND'} al.action = ${where_count + 1}"
            params.append(action)
            where_count += 1

        if resource_type:
            query += f"{' WHERE' if where_count == 0 else ' AND'} al.resource_type = ${where_count + 1}"
            params.append(resource_type)
            where_count += 1

        if user_id:
            query += f"{' WHERE' if where_count == 0 else ' AND'} al.user_id = ${where_count + 1}"
            params.append(user_id)
            where_count += 1

        if username:
            # Filter on the same expression the dropdown is built from, so a
            # name offered by /audit/actors always returns its rows.
            query += f"{' WHERE' if where_count == 0 else ' AND'} {ACTOR_SQL} = ${where_count + 1}"
            params.append(username)
            where_count += 1

        if start_date:
            query += f"{' WHERE' if where_count == 0 else ' AND'} al.created_at >= ${where_count + 1}"
            params.append(start_date)
            where_count += 1

        if end_date:
            query += f"{' WHERE' if where_count == 0 else ' AND'} al.created_at <= ${where_count + 1}"
            params.append(end_date)
            where_count += 1

        query += f" ORDER BY al.created_at DESC LIMIT ${where_count + 1} OFFSET ${where_count + 2}"
        params.extend([limit, offset])

        rows = await conn.fetch(query, *params)
        return [_serialize(r) for r in rows]


@router.get("/audit/stats")
async def audit_stats(current_user = Depends(require_admin_or_manager)):
    """Get audit log summary statistics"""
    async with get_connection() as conn:
        actions = await conn.fetch(
            "SELECT action, COUNT(*) as cnt FROM audit_log GROUP BY action ORDER BY cnt DESC"
        )

        recent = await conn.fetchval(
            "SELECT COUNT(*) FROM audit_log WHERE created_at >= NOW() - INTERVAL '24 hours'"
        )

        logins = await conn.fetch(
            """SELECT u.username, u.role, COUNT(*) as login_count
               FROM audit_log al
               JOIN users u ON u.id = al.user_id
               WHERE al.action = 'login'
                 AND al.created_at >= NOW() - INTERVAL '7 days'
               GROUP BY u.username, u.role
               ORDER BY login_count DESC
               LIMIT 10"""
        )

        top_actions = [dict(a) for a in actions]
        top_logins = [dict(l) for l in logins]

        return {
            "total_entries": await conn.fetchval("SELECT COUNT(*) FROM audit_log"),
            "recent_24h": recent,
            "actions_breakdown": top_actions,
            "recent_logins": top_logins,
        }
