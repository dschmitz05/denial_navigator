"""API Gateway — Auth routes (login, register, me)"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Request, Depends, Header, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.auth import hash_password, verify_password, create_token, decode_token
from api_gateway.services.audit import _client_ip, record as audit_record

logger = logging.getLogger("api_gateway.auth")

router = APIRouter()

# Login was unauthenticated and unlimited: 12 guesses took 2.3 seconds, and
# every one costs ~170ms of bcrypt, so it doubled as a way to burn the server's
# CPU without an account.
#
# Counted out of audit_log rather than in memory. uvicorn runs four workers,
# and an in-process counter gives each of them a separate budget - the stated
# limit of 6 was really 24, and a 10-attempt test did not trip it at all. The
# database is the only state all four workers already share.
#
# Two limits, because they stop different attacks: per-account defeats guessing
# one password list against one user, per-address defeats spraying one password
# across every account.
LOGIN_USER_LIMIT = 6        # failures per account
LOGIN_IP_LIMIT = 20         # failures per source address
LOGIN_WINDOW_MINUTES = 5


async def _recent_failures(conn, username: str, ip: Optional[str]) -> tuple[int, int]:
    """(failures for this account, failures from this address) in the window.

    Only failures since that account last signed in successfully are counted,
    which gives the reset for free: someone who mistypes twice and then gets in
    is not left near the limit for the next five minutes.
    """
    by_user = await conn.fetchval(
        f"""
        SELECT COUNT(*) FROM audit_log
         WHERE action = 'login_failed'
           AND lower(details->>'username') = lower($1)
           AND created_at > NOW() - INTERVAL '{LOGIN_WINDOW_MINUTES} minutes'
           AND created_at > COALESCE((
                 SELECT MAX(created_at) FROM audit_log
                  WHERE action = 'login'
                    AND lower(details->>'username') = lower($1)
               ), 'epoch'::timestamptz)
        """,
        username or "",
    )
    by_ip = 0
    if ip:
        by_ip = await conn.fetchval(
            f"""
            SELECT COUNT(*) FROM audit_log
             WHERE action = 'login_failed'
               AND ip_address = $1::inet
               AND created_at > NOW() - INTERVAL '{LOGIN_WINDOW_MINUTES} minutes'
            """,
            ip,
        )
    return int(by_user or 0), int(by_ip or 0)

# ── Pydantic Models ──

class LoginRequest(BaseModel):
    username: str
    password: str


class LoginResponse(BaseModel):
    access_token: str
    token_type: str = "bearer"
    user: dict


class UserResponse(BaseModel):
    id: str
    username: str
    email: str
    full_name: Optional[str] = None
    role: str
    is_active: bool
    last_login: Optional[str] = None


class RegisterRequest(BaseModel):
    username: str
    email: str
    password: str
    full_name: Optional[str] = None
    role: str = "billing_specialist"


# ── Auth Helpers ──

def get_current_user(request: Request, x_api_key: str = Header(None)):
    """Extract and validate the current user from Authorization header or API key"""
    auth_header = request.headers.get("Authorization", "")
    token = None

    if auth_header.startswith("Bearer "):
        token = auth_header[7:]
    elif x_api_key:
        token = x_api_key

    if not token:
        raise HTTPException(status_code=401, detail="Not authenticated")

    try:
        payload = decode_token(token)
        return payload
    except Exception:
        raise HTTPException(status_code=401, detail="Invalid or expired token")


# ── Routes ──

@router.post("/auth/login", response_model=dict)
async def login(http_request: Request, credentials: LoginRequest):
    """Authenticate a user and return a JWT token.

    Both outcomes are audited with the caller's address. A failed login with
    no IP and no user agent records that someone, somewhere, guessed wrong -
    which is not an access record. Repeated failures from one address are the
    signal worth having.
    """
    ip = _client_ip(http_request)
    user_agent = http_request.headers.get("user-agent")

    async with get_connection() as conn:
        # Checked BEFORE the password is verified, so a blocked attempt costs
        # no bcrypt - which is what closes the CPU-exhaustion side of this.
        fails_user, fails_ip = await _recent_failures(conn, credentials.username, ip)
        over = ("account" if fails_user >= LOGIN_USER_LIMIT
                else "address" if fails_ip >= LOGIN_IP_LIMIT else None)
        if over:
            await audit_record(
                action="login_blocked",
                resource_type="user",
                details={
                    "username": credentials.username,
                    "reason": "rate_limited",
                    "limit": over,
                    "failures_account": fails_user,
                    "failures_address": fails_ip,
                },
                ip_address=ip,
                user_agent=user_agent,
            )
            raise HTTPException(
                status_code=429,
                detail=(
                    "Too many failed sign-in attempts. "
                    f"Try again in {LOGIN_WINDOW_MINUTES} minutes."
                ),
                headers={"Retry-After": str(LOGIN_WINDOW_MINUTES * 60)},
            )

        user = await conn.fetchrow(
            "SELECT * FROM users WHERE username = $1 AND is_active = TRUE",
            credentials.username,
        )

        if not user or not verify_password(credentials.password, user["password_hash"]):
            await audit_record(
                action="login_failed",
                resource_type="user",
                resource_id=str(user["id"]) if user else None,
                details={
                    "username": credentials.username,
                    # Distinguishing the two matters when reading the log: one
                    # is a typo, a run of the other is username enumeration.
                    "reason": "unknown_or_inactive_user" if not user else "bad_password",
                },
                ip_address=ip,
                user_agent=user_agent,
            )
            raise HTTPException(status_code=401, detail="Invalid credentials")

        # Update last login
        await conn.execute(
            "UPDATE users SET last_login = NOW() WHERE id = $1",
            user["id"],
        )

        token = create_token(str(user["id"]), user["username"], user["role"])

    await audit_record(
        action="login",
        resource_type="user",
        resource_id=str(user["id"]),
        user_id=str(user["id"]),
        details={"username": user["username"], "role": user["role"]},
        ip_address=ip,
        user_agent=user_agent,
    )

    return {
        "access_token": token,
        "token_type": "bearer",
        "user": {
            "id": str(user["id"]),
            "username": user["username"],
            "email": user["email"],
            "full_name": user["full_name"],
            "role": user["role"],
            "is_active": user["is_active"],
            "last_login": str(user["last_login"]) if user["last_login"] else None,
        },
    }


@router.get("/auth/me", response_model=dict)
async def get_me(current_user = Depends(get_current_user)):
    """Get current user info"""
    return current_user


@router.post("/auth/register", status_code=201)
async def register(request: RegisterRequest, current_user = Depends(get_current_user)):
    """Register a new user (admin only)"""
    if current_user["role"] != "admin":
        raise HTTPException(status_code=403, detail="Admin access required")

    async with get_connection() as conn:
        # Check if username or email already exists
        existing = await conn.fetchrow(
            "SELECT id FROM users WHERE username = $1 OR email = $2",
            request.username, request.email,
        )
        if existing:
            raise HTTPException(status_code=409, detail="Username or email already exists")

        hashed = hash_password(request.password)
        row = await conn.fetchrow(
            """INSERT INTO users (username, email, password_hash, full_name, role, is_active)
               VALUES ($1, $2, $3, $4, $5, TRUE)
               RETURNING id, username, email, full_name, role, is_active""",
            request.username, request.email, hashed, request.full_name, request.role,
        )

        # Log the action
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, details)
               VALUES ($1, $2, $3, $4)""",
            current_user["sub"],
            "create_user",
            "user",
            json.dumps({"username": request.username, "role": request.role}),
        )

        return dict(row)


class PasswordChange(BaseModel):
    current_password: str
    new_password: str


MIN_PASSWORD_LENGTH = 8


@router.post("/auth/change-password", response_model=dict)
async def change_password(
    body: PasswordChange,
    http_request: Request,
    current_user = Depends(get_current_user),
):
    """Change your OWN password.

    Distinct from the admin reset at /users/{id}/password, which sets someone
    else's password without knowing it. This one proves the caller currently
    holds the password before replacing it, so a walk-up on an unlocked
    session cannot silently take the account over.
    """
    if len(body.new_password) < MIN_PASSWORD_LENGTH:
        raise HTTPException(
            status_code=400,
            detail=f"New password must be at least {MIN_PASSWORD_LENGTH} characters",
        )
    if body.new_password == body.current_password:
        raise HTTPException(
            status_code=400,
            detail="New password must be different from the current one",
        )

    ip = _client_ip(http_request)
    user_agent = http_request.headers.get("user-agent")

    async with get_connection() as conn:
        user = await conn.fetchrow(
            "SELECT id, username, password_hash FROM users WHERE id = $1 AND is_active = TRUE",
            current_user["sub"],
        )
        if not user:
            raise HTTPException(status_code=404, detail="User not found")

        if not verify_password(body.current_password, user["password_hash"]):
            # A failed attempt to change a password is a security event in its
            # own right, and is logged as one.
            await audit_record(
                action="password_change_failed",
                resource_type="user",
                resource_id=str(user["id"]),
                user_id=str(user["id"]),
                details={"username": user["username"], "reason": "wrong_current_password"},
                ip_address=ip,
                user_agent=user_agent,
            )
            raise HTTPException(status_code=401, detail="Current password is incorrect")

        # Moving sessions_valid_from is what evicts every other session. If
        # the old password was known to someone else, changing it has to end
        # the access it bought them, not just stop it being reusable.
        await conn.execute(
            """UPDATE users
                  SET password_hash = $1, sessions_valid_from = date_trunc('second', NOW()), updated_at = NOW()
                WHERE id = $2""",
            hash_password(body.new_password), user["id"],
        )

    await audit_record(
        action="password_changed",
        resource_type="user",
        resource_id=str(user["id"]),
        user_id=str(user["id"]),
        details={"username": user["username"], "self_service": True},
        ip_address=ip,
        user_agent=user_agent,
    )

    # Every session opened before this moment is now refused - including the
    # one making this request. Hand back a fresh token so changing your own
    # password does not log you out, while still evicting everyone else.
    return {
        "status": "password_changed",
        "access_token": create_token(str(user["id"]), user["username"], current_user["role"]),
        "token_type": "bearer",
    }
