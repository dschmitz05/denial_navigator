"""API Gateway — User management routes (admin only)"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request
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


# Outcomes that mean a queue item is finished. Assignment on a CLOSED item is
# the record of who did the work, so it is left alone; only live work moves.
OPEN_QUEUE_CLAUSE = """
    (outcome_status IS NULL
     OR outcome_status NOT IN ('approved', 'overruled', 'resolved',
                               'denied_again', 'cancelled'))
"""


async def _release_queue_items(conn, user_id: str) -> int:
    """Return a deactivated user's live queue items to the shared pool.

    Blocking assignment TO a deactivated user is only half the problem. The
    other half is assigning work to someone active and deactivating them
    afterwards: the item stays pinned to an account nobody can log into, and
    because specialists only see their own items plus unassigned ones, it
    becomes invisible to every specialist while still counting as open work.

    Unassigning is better than refusing to deactivate the user - offboarding
    should not be blocked by a queue - and better than deleting the items,
    which are real outstanding money.
    """
    released = await conn.fetch(
        f"""
        UPDATE appeals_queue
           SET assigned_user_id = NULL, updated_at = NOW()
         WHERE assigned_user_id = $1
           AND {OPEN_QUEUE_CLAUSE}
        RETURNING id
        """,
        user_id,
    )
    return len(released)


def require_manager_up(current_user: dict = Depends(get_current_user)) -> dict:
    """Dependency that requires manager, director or admin."""
    if current_user.get("role") not in ("billing_manager", "rcm_director", "admin"):
        raise HTTPException(status_code=403, detail="Manager+ access required")
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
            SELECT id, username, email, full_name, role, is_active, created_at, last_login,
                   totp_required,
                   (totp_confirmed_at IS NOT NULL) AS totp_enrolled
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


@router.get("/users/assignable", response_model=list[dict])
async def list_assignable_users(current_user = Depends(require_manager_up)):
    """Users a manager can hand queue work to.

    Deliberately not /users: assigning work needs names and roles, not email
    addresses, activity or the ability to edit anyone. Managers get this;
    user administration stays with admins.

    Declared above /users/{user_id} on purpose - FastAPI matches routes in
    order, and "assignable" would otherwise be parsed as a user id.
    """
    async with get_connection() as conn:
        rows = await conn.fetch(
            """
            SELECT id, username, full_name, role
            FROM users
            WHERE is_active = TRUE
            ORDER BY
                -- Specialists first: they are who work is usually assigned to.
                CASE role WHEN 'billing_specialist' THEN 0
                          WHEN 'billing_manager' THEN 1
                          WHEN 'rcm_director' THEN 2
                          ELSE 3 END,
                COALESCE(full_name, username)
            """
        )
        return [{**dict(r), "id": str(r["id"])} for r in rows]


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

        # Deactivating through this route is the same event as DELETE, so it
        # must release the same work. Handling it in only one of the two paths
        # is how a fix quietly stops applying.
        released = 0
        if update.is_active is False and row:
            released = await _release_queue_items(conn, user_id)

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
            json.dumps({
                "changes": {k: v for k, v in update.model_dump().items() if v is not None},
                **({"queue_items_released": released} if released else {}),
            }),
        )

        result = dict(row)
        if update.is_active is False:
            result["queue_items_released"] = released
        return result


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
        # Same reasoning as the self-service change: an admin resetting a
        # password is usually responding to a compromise, so the sessions
        # opened with the old one must end too.
        await conn.execute(
            """UPDATE users
                  SET password_hash = $1, sessions_valid_from = date_trunc('second', NOW()), updated_at = NOW()
                WHERE id = $2""",
            hashed, user_id,
        )

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
    purge: bool = Query(False, description="Permanently erase the account instead of deactivating it"),
    confirm_username: Optional[str] = Query(
        None, description="Required with purge=true; must equal the target's username"
    ),
    current_user = Depends(require_admin),
):
    """Deactivate a user, or with `purge=true`, erase the account entirely.

    Deactivation is the normal path: sign-in is blocked, the row survives, and
    everything that user ever did stays attributable to a name.

    A purge is different in kind, so it is guarded differently. Nothing in this
    schema has a foreign key to users, so the database will not stop you and
    nothing cascades - the references have to be cleared here, deliberately,
    and the attribution they carried is gone. `confirm_username` must match the
    account exactly, which makes an accidental purge - a wrong id in a script,
    a mis-click - very hard to perform.
    """
    async with get_connection() as conn:
        user = await conn.fetchrow("SELECT id, username, role FROM users WHERE id = $1", user_id)
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        # Prevent self-deletion
        if current_user["sub"] == user_id:
            raise HTTPException(status_code=400, detail="Cannot delete yourself")

        # Prevent deleting the last admin
        admin_count = await conn.fetchval(
            "SELECT COUNT(*) FROM users WHERE role = 'admin' AND id != $1 AND is_active = TRUE",
            user_id,
        )
        if user["role"] == "admin" and admin_count == 0:
            raise HTTPException(status_code=400, detail="Cannot delete the last admin user")

        if purge:
            if confirm_username != user["username"]:
                raise HTTPException(
                    status_code=400,
                    detail=(
                        "Permanent deletion requires confirm_username to match "
                        f"the account exactly (expected '{user['username']}')"
                    ),
                )

            # Their live work goes back to the pool; closed items lose the
            # record of who did them, because that record was the user row.
            released = await _release_queue_items(conn, user_id)
            closed_items = await conn.fetchval(
                f"""SELECT COUNT(*) FROM appeals_queue
                     WHERE assigned_user_id = $1 AND NOT {OPEN_QUEUE_CLAUSE}""",
                user_id,
            )
            audit_entries = await conn.fetchval(
                "SELECT COUNT(*) FROM audit_log WHERE user_id = $1", user_id
            )

            # Written BEFORE the row disappears, so the log records who was
            # erased, by whom, and what it cost. This entry is the only thing
            # left afterwards that names them.
            await conn.execute(
                """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
                   VALUES ($1, $2, $3, $4, $5)""",
                current_user["sub"], "purge_user", "user", user_id,
                json.dumps({
                    "username": user["username"],
                    "role": user["role"],
                    "queue_items_released": released,
                    "closed_items_unattributed": closed_items,
                    "audit_entries_orphaned": audit_entries,
                }),
            )

            async with conn.transaction():
                # No foreign keys exist, so these would otherwise be left
                # pointing at an id that resolves to nobody.
                await conn.execute(
                    "UPDATE appeals_queue SET assigned_user_id = NULL WHERE assigned_user_id = $1",
                    user_id,
                )
                await conn.execute(
                    "UPDATE feedback_loop SET user_id = NULL WHERE user_id = $1", user_id
                )
                await conn.execute("DELETE FROM users WHERE id = $1", user_id)

            logger.warning(
                f"PURGED user {user['username']} ({user_id}) by {current_user.get('username')}"
            )
            return {
                "status": "purged",
                "username": user["username"],
                "queue_items_released": released,
                "closed_items_unattributed": closed_items,
                "audit_entries_orphaned": audit_entries,
            }

        # Soft delete by deactivating
        await conn.execute(
            "UPDATE users SET is_active = FALSE, updated_at = NOW() WHERE id = $1",
            user_id,
        )

        # Their live work goes back in the pool, or it is orphaned.
        released = await _release_queue_items(conn, user_id)

        # Log the action
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"],
            "deactivate_user",
            "user",
            user_id,
            json.dumps({"username": user["username"], "queue_items_released": released}),
        )

        return {"status": "deactivated", "queue_items_released": released}


# ── Two-factor administration ──
#
# An administrator decides WHETHER an account uses 2FA and can reset it when a
# device is lost. They never see or set the secret: enrolment happens between
# the user and their authenticator, so an admin cannot generate codes for
# someone else's account.

class TotpPolicy(BaseModel):
    required: bool


@router.post("/users/{user_id}/totp", response_model=dict)
async def set_totp_policy(
    user_id: str,
    body: TotpPolicy,
    http_request: Request,
    current_user = Depends(require_admin),
):
    """Require or stop requiring two-factor authentication for an account."""
    async with get_connection() as conn:
        user = await conn.fetchrow(
            "SELECT id, username, totp_required, totp_confirmed_at FROM users WHERE id = $1",
            user_id,
        )
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        if body.required:
            # Turning it on does not clear an existing enrolment: an admin
            # toggling the policy should not silently invalidate a working
            # authenticator the user still holds.
            await conn.execute(
                "UPDATE users SET totp_required = TRUE, updated_at = NOW() WHERE id = $1",
                user_id,
            )
            outcome = "enrolled" if user["totp_confirmed_at"] else "enrollment_pending"
        else:
            # Turning it off discards the secret. Leaving it behind would mean
            # re-enabling 2FA later silently re-activates a device that may be
            # long gone, and keeps a password-equivalent secret for no reason.
            await conn.execute(
                """UPDATE users
                      SET totp_required = FALSE, totp_secret = NULL,
                          totp_confirmed_at = NULL, totp_last_used_step = NULL,
                          updated_at = NOW()
                    WHERE id = $1""",
                user_id,
            )
            outcome = "disabled"

        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"],
            "totp_required" if body.required else "totp_disabled",
            "user", user_id,
            json.dumps({"username": user["username"], "outcome": outcome}),
        )

    return {
        "status": outcome,
        "username": user["username"],
        "totp_required": body.required,
    }


@router.post("/users/{user_id}/totp/reset", response_model=dict)
async def reset_totp(
    user_id: str,
    http_request: Request,
    current_user = Depends(require_admin),
):
    """Clear an account's authenticator so it can be set up on a new device.

    This is the answer to a lost or replaced phone. It keeps the requirement in
    place, so the account cannot sign in until it has enrolled again - resetting
    is not a way to switch 2FA off by the back door.

    Every session is also ended: if the device is lost rather than replaced,
    leaving existing sessions running would be the obvious gap.
    """
    async with get_connection() as conn:
        user = await conn.fetchrow(
            "SELECT id, username, totp_required FROM users WHERE id = $1", user_id
        )
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        await conn.execute(
            """UPDATE users
                  SET totp_secret = NULL, totp_confirmed_at = NULL,
                      totp_last_used_step = NULL,
                      sessions_valid_from = date_trunc('second', NOW()),
                      updated_at = NOW()
                WHERE id = $1""",
            user_id,
        )
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, resource_id, details)
               VALUES ($1, $2, $3, $4, $5)""",
            current_user["sub"], "totp_reset", "user", user_id,
            json.dumps({"username": user["username"], "still_required": user["totp_required"]}),
        )

    return {
        "status": "reset",
        "username": user["username"],
        "totp_required": user["totp_required"],
        "note": "The user will be asked to set up an authenticator at their next sign-in.",
    }
