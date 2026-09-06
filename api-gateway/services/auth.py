"""API Gateway — Authentication service (JWT + bcrypt)"""

import os
import jwt
from fastapi import Header, HTTPException
import bcrypt
from datetime import datetime, timedelta, timezone
from typing import Optional

JWT_SECRET = os.environ.get("JWT_SECRET", "change-me-in-production-use-a-real-secret")
JWT_ALGORITHM = "HS256"
TOKEN_EXPIRY_MINUTES = 480  # 8 hours


def hash_password(password: str) -> str:
    return bcrypt.hashpw(password.encode("utf-8"), bcrypt.gensalt()).decode("utf-8")


def verify_password(password: str, hashed: str) -> bool:
    return bcrypt.checkpw(password.encode("utf-8"), hashed.encode("utf-8"))


def create_token(user_id: str, username: str, role: str) -> str:
    now = datetime.now(timezone.utc)
    payload = {
        "sub": user_id,
        "username": username,
        "role": role,
        # Issued-at is what makes revocation possible: a password change moves
        # users.sessions_valid_from forward, and every token minted before it
        # stops being accepted.
        "iat": now,
        "exp": now + timedelta(minutes=TOKEN_EXPIRY_MINUTES),
    }
    return jwt.encode(payload, JWT_SECRET, algorithm=JWT_ALGORITHM)


def decode_token(token: str) -> dict:
    return jwt.decode(token, JWT_SECRET, algorithms=[JWT_ALGORITHM])


def get_current_user(authorization: str = Header(None)) -> dict:
    """Dependency: extract and decode JWT token from Authorization header"""
    if not authorization or not authorization.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="Not authenticated")
    token = authorization.split(" ", 1)[1]
    try:
        payload = decode_token(token)
        return payload
    except jwt.ExpiredSignatureError:
        raise HTTPException(status_code=401, detail="Token expired")
    except jwt.InvalidTokenError:
        raise HTTPException(status_code=401, detail="Invalid token")
