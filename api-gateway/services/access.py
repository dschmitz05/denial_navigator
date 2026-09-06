"""API Gateway — access control.

Recording who touched PHI is only half of HIPAA's requirement; the other half
is refusing the ones who have no business touching it (§164.312(a), Access
Control). Every /api route used to answer anyone who could reach port 8000,
and the audit log faithfully recorded them as `anonymous`.

Two kinds of caller are legitimate:

  * A person, carrying the JWT issued by /auth/login.
  * A sibling service. ediparser POSTs parsed claims to /ingestion/store and
    llm-service POSTs analyses to /analyses/store; neither can log in. They
    present SERVICE_API_KEY instead, and are audited under their own name so a
    machine write is never mistaken for a person's.

Anything else gets 401 before the handler runs.
"""

import logging
import os
import secrets
import uuid
from typing import Optional

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.responses import JSONResponse

from api_gateway.services.auth import decode_token

logger = logging.getLogger("api_gateway.access")

SERVICE_API_KEY = os.environ.get("SERVICE_API_KEY", "")

# Reachable without credentials. Everything here is either infrastructure or
# the door itself - the login endpoint cannot require a login.
# The only things a token issued between the password and the second factor
# may touch. Without this, passing the password would be enough to reach the
# API and 2FA would be decorative.
MFA_ONLY_PATHS = {
    "/api/v1/auth/totp/enroll",
    "/api/v1/auth/totp/confirm",
    "/api/v1/auth/login/totp",
    "/api/v1/auth/me",
}

PUBLIC_EXACT = {
    "/", "/health", "/docs", "/redoc", "/openapi.json", "/favicon.ico",
    "/api/v1/auth/login",
}


class Principal:
    """Who is making this request."""

    __slots__ = ("kind", "user_id", "username", "role", "reason", "issued_at", "scope")

    def __init__(self, kind: str, user_id=None, username=None, role=None,
                 reason=None, issued_at=None, scope=None):
        self.kind = kind            # 'user' | 'service' | 'anonymous'
        self.user_id = user_id      # UUID string, users only
        self.username = username
        self.role = role            # RBAC role, users only
        self.reason = reason        # why authentication failed, if it did
        self.issued_at = issued_at  # JWT iat, epoch seconds
        self.scope = scope          # 'mfa' for a half-authenticated token

    @property
    def authenticated(self) -> bool:
        return self.kind in ("user", "service")


def principal(request) -> Principal:
    """Resolve the caller. Never raises, never rejects - that is the caller's job."""
    service_key = request.headers.get("x-service-key")
    if service_key:
        if SERVICE_API_KEY and secrets.compare_digest(service_key, SERVICE_API_KEY):
            name = request.headers.get("x-service-name") or "unknown"
            return Principal("service", username=f"service:{name}")
        return Principal("anonymous", reason="invalid_service_key")

    header = request.headers.get("authorization") or ""
    token = header[7:] if header.lower().startswith("bearer ") else request.headers.get("x-api-key")
    if not token:
        return Principal("anonymous", reason="no_credentials")

    try:
        payload = decode_token(token)
    except Exception as e:
        # Expired vs malformed matters: the first is a stale browser tab, the
        # second is worth a closer look.
        reason = "expired_token" if "expired" in str(e).lower() else "invalid_token"
        return Principal("anonymous", reason=reason)

    user_id = payload.get("sub")
    try:
        uuid.UUID(str(user_id))
    except (ValueError, TypeError, AttributeError):
        return Principal("anonymous", reason="malformed_subject")

    return Principal(
        "user",
        user_id=str(user_id),
        username=payload.get("username"),
        role=payload.get("role"),
        issued_at=payload.get("iat"),
        scope=payload.get("scope"),
    )


# ── Role-based authorization ──
#
# The four roles come from database/init.sql and the descriptions in
# docs/ARCHITECTURE.md:
#
#   billing_specialist  queue operations, view claims
#   billing_manager     policy management, bulk operations
#   rcm_director        full data access, reporting
#   admin               system configuration
#
# Working denials IS the specialist's job, so they read and write the denial,
# appeal, worklist and analysis queues. What they are kept out of is anything
# that changes the ground everyone else stands on - payer policy that steers
# every future AI analysis, remittance ingestion, the audit trail, and user
# administration.

SPECIALIST = "billing_specialist"
MANAGER = "billing_manager"
DIRECTOR = "rcm_director"
ADMIN = "admin"

ALL_ROLES = frozenset({SPECIALIST, MANAGER, DIRECTOR, ADMIN})
MANAGER_UP = frozenset({MANAGER, DIRECTOR, ADMIN})
ADMIN_ONLY = frozenset({ADMIN})
NOBODY = frozenset()

# resource -> {"read": roles, "write": roles}
PERMISSIONS = {
    # Day-to-day denial work. Everyone who logs in does this.
    "claims":    {"read": ALL_ROLES, "write": MANAGER_UP},
    "denials":   {"read": ALL_ROLES, "write": ALL_ROLES},
    "appeals":   {"read": ALL_ROLES, "write": ALL_ROLES},
    "analyses":  {"read": ALL_ROLES, "write": ALL_ROLES},
    "feedback":  {"read": ALL_ROLES, "write": ALL_ROLES},

    # Policy documents steer every future AI analysis; ingestion creates the
    # claims everyone else works. Both are manager responsibilities.
    "knowledge": {"read": ALL_ROLES, "write": MANAGER_UP},
    "ingestion": {"read": ALL_ROLES, "write": MANAGER_UP},

    # The audit trail is evidence: readable by oversight, writable by no one
    # through the API - entries are only ever produced as a side effect.
    "audit":     {"read": MANAGER_UP, "write": NOBODY},

    "users":     {"read": ADMIN_ONLY, "write": ADMIN_ONLY},

    # /auth/me. /auth/register carries its own admin check in the handler.
    "auth":      {"read": ALL_ROLES, "write": ALL_ROLES},

    # Service health. Readable by anyone signed in - knowing the LLM is down
    # is how a biller understands why the AI button is failing. Nothing to
    # write: status is observed, never set.
    "system":    {"read": ALL_ROLES, "write": NOBODY},
}

# Exceptions that are narrower or wider than their resource's rule. Checked
# before PERMISSIONS, with UUID path segments normalised to "{id}".
#
#   users/assignable  a manager must see who they can hand work to, but that
#                     is not a reason to open user administration - this
#                     returns names and roles only.
#   appeals/{id}/assign  assigning work is a supervisory act, so it is barred
#                     to specialists even though they may otherwise write to
#                     the appeals queue.
PATH_PERMISSIONS = {
    ("GET", "/api/v1/users/assignable"): MANAGER_UP,
    ("POST", "/api/v1/appeals/{id}/assign"): MANAGER_UP,
    # Deciding whether an account needs 2FA, and resetting a lost device, are
    # administrator actions - never the user's own.
    ("POST", "/api/v1/users/{id}/totp"): ADMIN_ONLY,
    ("POST", "/api/v1/users/{id}/totp/reset"): ADMIN_ONLY,
}

WRITE_METHODS = {"POST", "PUT", "PATCH", "DELETE"}

# POST endpoints that only read. Judging by HTTP verb alone would lock a
# specialist out of searching the knowledge base and reading ingest history.
READ_ONLY_POSTS = {
    "/api/v1/knowledge/search",
    "/api/v1/ingestion/log",
}


def _normalise(path: str) -> str:
    """Replace UUID segments with {id} so a path rule matches any record."""
    out = []
    for segment in path.split("/"):
        try:
            uuid.UUID(segment)
            out.append("{id}")
        except ValueError:
            out.append(segment)
    return "/".join(out)


def _resource_of(path: str) -> str:
    trimmed = path[len("/api/v1"):] if path.startswith("/api/v1") else path
    segments = [s for s in trimmed.split("/") if s]
    return segments[0] if segments else ""


def authorize(who: "Principal", method: str, path: str) -> tuple[bool, str]:
    """(allowed, reason). Resource-and-action level, evaluated per request."""
    # Sibling services are not people and hold no role. They are already
    # restricted by holding the shared key, which never leaves the network.
    if who.kind == "service":
        return True, ""

    # A half-authenticated token carries no role - the session it will become
    # has not been issued yet. It is already confined to MFA_ONLY_PATHS by the
    # middleware, so there is nothing further for the role rules to decide.
    if who.scope == "mfa":
        return True, ""

    specific = PATH_PERMISSIONS.get((method, _normalise(path)))
    if specific is not None:
        if who.role not in specific:
            return False, f"role '{who.role}' may not use {_normalise(path)}"
        return True, ""

    resource = _resource_of(path)
    rules = PERMISSIONS.get(resource)
    if rules is None:
        # Unknown resource: refuse rather than guess. A route added without a
        # permission entry should fail loudly in testing, not quietly allow.
        return False, f"no permission rule for '{resource}'"

    action = "write" if (method in WRITE_METHODS and path not in READ_ONLY_POSTS) else "read"
    allowed = rules[action]

    if who.role not in allowed:
        return False, (
            f"role '{who.role}' may not {action} {resource}"
        )
    return True, ""


def is_public(path: str, method: str) -> bool:
    if method == "OPTIONS":       # CORS preflight carries no credentials
        return True
    if path in PUBLIC_EXACT:
        return True
    # Only the API is protected; anything else here is static/infrastructure.
    return not path.startswith("/api/")


async def account_is_current(who: "Principal") -> tuple[bool, str]:
    """Is the account behind this token still entitled to use it?

    A signature and an expiry only prove the token was minted by us and has not
    aged out. They say nothing about whether the account still exists, is still
    enabled, or has had its password changed since - so without this check a
    dismissed employee kept working access for the rest of the token's life,
    and resetting a stolen password did not evict whoever stole it.

    One indexed primary-key lookup per request. Deliberately not cached: a
    cache TTL is exactly the window in which a revoked session still works,
    and the point of this function is that there is no such window.
    """
    from api_gateway.services.db import get_connection

    try:
        async with get_connection() as conn:
            row = await conn.fetchrow(
                "SELECT is_active, sessions_valid_from FROM users WHERE id = $1::uuid",
                who.user_id,
            )
    except Exception as e:
        # Fail closed. If the account cannot be confirmed, the request is not
        # authorised - an outage must not become an authentication bypass.
        logger.error(f"account check failed for {who.username}: {e}")
        return False, "account_check_unavailable"

    if row is None:
        return False, "account_deleted"
    if not row["is_active"]:
        return False, "account_deactivated"

    valid_from = row["sessions_valid_from"]
    if valid_from is not None and who.issued_at is not None:
        # Compared exactly. An earlier version allowed a one-second grace to
        # avoid rejecting a token minted in the same second as the bump - but
        # that grace IS a bypass window, and it was wide enough that a password
        # reset failed to evict the old session at all. The self-service change
        # returns a fresh token instead, so nothing legitimate needs the slack.
        if float(who.issued_at) < valid_from.timestamp():
            return False, "credentials_changed"

    return True, ""


class AccessControlMiddleware(BaseHTTPMiddleware):
    """Reject unauthenticated API requests before the handler sees them."""

    async def dispatch(self, request, call_next):
        path = request.url.path
        if is_public(path, request.method):
            return await call_next(request)

        who = principal(request)

        # A token that has only cleared the password step is confined to the
        # endpoints that complete the second factor.
        if who.kind == "user" and who.scope == "mfa" and path not in MFA_ONLY_PATHS:
            logger.warning(f"401 {request.method} {path} reason=mfa_incomplete user={who.username}")
            return JSONResponse(
                status_code=401,
                content={"detail": "Two-factor authentication has not been completed"},
                headers={"WWW-Authenticate": "Bearer"},
            )

        # A valid signature is not enough; the account behind it must still be
        # entitled to it. Services hold no account, so they skip this.
        if who.kind == "user":
            current, why = await account_is_current(who)
            if not current:
                who = Principal("anonymous", username=who.username, reason=why)

        if not who.authenticated:
            # The audit middleware sits outside this one, so the rejection is
            # still recorded - a refused access is exactly what a reviewer
            # needs to see.
            logger.warning(
                f"401 {request.method} {path} reason={who.reason} "
                f"ip={request.headers.get('x-real-ip') or (request.client.host if request.client else '?')}"
            )
            return JSONResponse(
                status_code=401,
                content={"detail": "Not authenticated"},
                headers={"WWW-Authenticate": "Bearer"},
            )

        # Handlers and the audit middleware reuse this instead of re-parsing
        # the token on every request.
        request.state.principal = who

        allowed, reason = authorize(who, request.method, path)
        if not allowed:
            logger.warning(
                f"403 {request.method} {path} user={who.username} reason={reason}"
            )
            return JSONResponse(
                status_code=403,
                content={"detail": f"Access denied: {reason}"},
            )

        return await call_next(request)
