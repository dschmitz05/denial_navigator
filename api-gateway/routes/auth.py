"""API Gateway — Auth routes (login, register, me)"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Request, Depends, Header, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services.auth import hash_password, verify_password, create_token, decode_token

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
async def login(request: LoginRequest):
    """Authenticate a user and return a JWT token"""
    async with get_connection() as conn:
        user = await conn.fetchrow(
            "SELECT * FROM users WHERE username = $1 AND is_active = TRUE",
            request.username,
        )

        if not user:
            # Log failed attempt
            await conn.execute(
                """INSERT INTO audit_log (action, resource_type, details, ip_address, user_agent)
                   VALUES ($1, $2, $3, $4, $5)""",
                "login_failed",
                "user",
                json.dumps({"username": request.username}),
                None,
                None,
            )
            raise HTTPException(status_code=401, detail="Invalid credentials")

        if not verify_password(request.password, user["password_hash"]):
            await conn.execute(
                """INSERT INTO audit_log (action, resource_type, details, ip_address, user_agent)
                   VALUES ($1, $2, $3, $4, $5)""",
                "login_failed",
                "user",
                json.dumps({"username": request.username}),
                None,
                None,
            )
            raise HTTPException(status_code=401, detail="Invalid credentials")

        # Update last login
        await conn.execute(
            "UPDATE users SET last_login = NOW() WHERE id = $1",
            user["id"],
        )

        token = create_token(str(user["id"]), user["username"], user["role"])

        # Log successful login
        await conn.execute(
            """INSERT INTO audit_log (user_id, action, resource_type, details, ip_address, user_agent)
               VALUES ($1, $2, $3, $4, $5, $6)""",
            user["id"],
            "login",
            "user",
            json.dumps({"username": user["username"], "role": user["role"]}),
            None,
            None,
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
