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

        await conn.execute(
            "UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2",
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
    return {"status": "password_changed"}
