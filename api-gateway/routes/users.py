"""API Gateway — User management routes (admin only)"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.auth import hash_password, verify_password, decode_token, get_current_user

logger = logging.getLogger("api_gateway.users")

router = APIRouter()


def require_admin(current_user: dict = Depends(get_current_user)) -> dict:
    """Dependency that requires admin role"""
    if current_user.get("role") != "admin":
        raise HTTPException(status_code=403, detail="Admin access required")
    return current_user


# ── Models ──

class UserUpdate(BaseModel):
    email: Optional[str] = None
    full_name: Optional[str] = None
    role: Optional[str] = None
    is_active: Optional[bool] = None


class UserPasswordUpdate(BaseModel):
    password: str


# ── Routes ──

@router.get("/users", response_model=list[dict])
async def list_users(
    role: Optional[str] = Query(None),
    is_active: Optional[bool] = Query(None),
    search: Optional[str] = Query(None),
    limit: int = Query(100, ge=1, le=500),
    offset: int = Query(0, ge=0),
    current_user = Depends(require_admin),
):
    """List all users (admin only)"""
    async with get_connection() as conn:
        query = """
            SELECT id, username, email, full_name, role, is_active, created_at, last_login
            FROM users
        """
        params = []
        where_count = 0

        if role:
            query += f"{' WHERE' if where_count == 0 else ' AND'} role = ${where_count + 1}"
            params.append(role)
            where_count += 1

        if is_active is not None:
            query += f"{' WHERE' if where_count == 0 else ' AND'} is_active = ${where_count + 1}"
            params.append(is_active)
            where_count += 1

        if search:
            query += f"{' WHERE' if where_count == 0 else ' AND'} (username ILIKE ${where_count + 1} OR email ILIKE ${where_count + 1} OR full_name ILIKE ${where_count + 1})"
            params.append(f"%{search}%")
            where_count += 1

        query += f" ORDER BY created_at DESC LIMIT ${where_count + 1} OFFSET ${where_count + 2}"
        params.extend([limit, offset])

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.get("/users/{user_id}", response_model=dict)
async def get_user(
    user_id: str,
    current_user = Depends(require_admin),
):
    """Get a single user by ID (admin only)"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            "SELECT id, username, email, full_name, role, is_active, created_at, last_login FROM users WHERE id = $1",
            user_id,
        )
        if not row:
            raise HTTPException(status_code=404, detail="User not found")
        return dict(row)


@router.patch("/users/{user_id}", response_model=dict)
async def update_user(
    user_id: str,
    update: UserUpdate,
    current_user = Depends(require_admin),
):
    """Update a user (admin only)"""
    async with get_connection() as conn:
        # Prevent self-modification of role
        if current_user["sub"] == user_id and update.role:
            raise HTTPException(status_code=400, detail="Cannot change your own role")

        # Check if email would conflict
        if update.email:
            existing = await conn.fetchrow(
                "SELECT id FROM users WHERE email = $1 AND id != $2",
                update.email, user_id,
            )
            if existing:
                raise HTTPException(status_code=409, detail="Email already in use")

        updates = []
        params = []
        idx = 1

        for field in ["email", "full_name", "role", "is_active"]:
            if getattr(update, field) is not None:
                if field == "role":
                    valid_roles = ["billing_specialist", "billing_manager", "rcm_director", "admin"]
                    if getattr(update, field) not in valid_roles:
                        raise HTTPException(status_code=400, detail=f"Invalid role. Must be one of: {valid_roles}")
                updates.append(f"{field} = ${idx}")
                params.append(getattr(update, field))
                idx += 1

        if not updates:
            raise HTTPException(status_code=400, detail="No updates provided")

        updates.append("updated_at = NOW()")
        params.append(user_id)

        row = await conn.fetchrow(
            f"UPDATE users SET {', '.join(updates)} WHERE id = ${idx} RETURNING id, username, email, full_name, role, is_active, created_at, last_login",
            *params,
        )

        if not row:
            raise HTTPException(status_code=404, detail="User not found")

        # Log the action
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"],
            "update_user",
            "user",
            user_id,
            json.dumps({"changes": {k: v for k, v in update.model_dump().items() if v is not None}}),
        )

        return dict(row)


@router.post("/users/{user_id}/password", status_code=200)
async def reset_user_password(
    user_id: str,
    pwd: UserPasswordUpdate,
    current_user = Depends(require_admin),
):
    """Reset a user's password (admin only)"""
    async with get_connection() as conn:
        # Check user exists
        user = await conn.fetchrow("SELECT id, username FROM users WHERE id = $1", user_id)
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        hashed = hash_password(pwd.password)
        await conn.execute("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2", hashed, user_id)

        # Log the action
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"],
            "reset_password",
            "user",
            user_id,
            json.dumps({"target_username": user["username"]}),
        )

        return {"status": "password_reset"}


@router.delete("/users/{user_id}", status_code=200)
async def delete_user(
    user_id: str,
    current_user = Depends(require_admin),
):
    """Delete or deactivate a user (admin only)"""
    async with get_connection() as conn:
        user = await conn.fetchrow("SELECT id, username, role FROM users WHERE id = $1", user_id)
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        # Prevent self-deletion
        if current_user["sub"] == user_id:
            raise HTTPException(status_code=400, detail="Cannot delete yourself")

        # Prevent deleting the last admin
        admin_count = await conn.fetchval(
            "SELECT COUNT(*) FROM users WHERE role = 'admin' AND id != $1",
            user_id,
        )
        if user["role"] == "admin" and admin_count == 0:
            raise HTTPException(status_code=400, detail="Cannot delete the last admin user")

        # Soft delete by deactivating
        await conn.execute(
            "UPDATE users SET is_active = FALSE, updated_at = NOW() WHERE id = $1",
            user_id,
        )

        # Log the action
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"],
            "deactivate_user",
            "user",
            user_id,
            json.dumps({"username": user["username"]}),
        )

        return {"status": "deactivated"}
