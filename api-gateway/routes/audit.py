"""API Gateway — Audit log routes"""

import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Depends, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.auth import get_current_user

logger = logging.getLogger("api_gateway.audit")

router = APIRouter()


def require_admin_or_manager(current_user: dict = Depends(get_current_user)) -> dict:
    """Dependency that requires admin or billing_manager role"""
    if current_user.get("role") not in ("admin", "billing_manager", "rcm_director"):
        raise HTTPException(status_code=403, detail="Manager+ access required")
    return current_user


@router.get("/audit", response_model=list[dict])
async def list_audit_log(
    action: Optional[str] = Query(None),
    resource_type: Optional[str] = Query(None),
    user_id: Optional[str] = Query(None),
    start_date: Optional[str] = Query(None),
    end_date: Optional[str] = Query(None),
    limit: int = Query(200, ge=1, le=1000),
    offset: int = Query(0, ge=0),
    current_user = Depends(require_admin_or_manager),
):
    """List audit log entries (admin/manager only)"""
    async with get_connection() as conn:
        query = """
            SELECT al.id, al.user_id, u.username, al.action,
                   al.resource_type, al.resource_id, al.details,
                   al.ip_address, al.user_agent, al.created_at
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
        return [dict(r) for r in rows]


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
