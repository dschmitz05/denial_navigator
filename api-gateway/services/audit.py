"""API Gateway — HIPAA access auditing.

HIPAA's audit requirement covers ACCESS to PHI, not just modification: who
looked at a patient's claim, when, from where, and whether it worked. Before
this, the log held logins, user administration and one appeal event - opening
every claim in the system left no trace at all.

Auditing is done as middleware rather than a call in each handler, because a
log with holes is worse than no log: it reads as evidence of absence. A route
added next month is audited without anyone remembering to wire it up.

Routes that record richer, semantic entries of their own (an appeal's old and
new outcome, say) still do. Those describe WHAT CHANGED; the rows written here
describe WHO REACHED THE DATA. A compliance reviewer needs both.
"""

import ipaddress
import json
import logging
import os
import time
import uuid
from typing import Optional

from starlette.middleware.base import BaseHTTPMiddleware

from api_gateway.services.access import principal
from api_gateway.services.db import get_connection

logger = logging.getLogger("api_gateway.audit")

# Infrastructure, not PHI. Auditing these adds noise a reviewer has to wade
# through without recording a single access to patient data.
SKIP_EXACT = {"/", "/health", "/docs", "/redoc", "/openapi.json", "/favicon.ico"}

# POST /auth/login writes its own entry, with the attempted username and the
# success/failure distinction. A second, blinder row here would only dilute it.
SKIP_PREFIXES = (
    "/api/v1/auth/login",
    # The Settings page polls this; it reads no PHI, and logging it would
    # bury real access records under infrastructure chatter.
    "/api/v1/system/health",
)

# path segment -> the audit_log.resource_type it represents
RESOURCE_TYPES = {
    "claims": "claim",
    "denials": "denial",
    "appeals": "appeal",
    "analyses": "analysis",
    "knowledge": "knowledge_doc",
    "ingestion": "file",
    "feedback": "feedback",
    "users": "user",
    "auth": "user",
    "audit": "audit_log",
}

# Specific endpoints whose meaning is not captured by "<verb> <resource>".
# Keyed by (method, path suffix). The vocabulary matches the actions listed
# in database/init.sql.
NAMED_ACTIONS = {
    ("POST", "/analyses/generate"): "generate_analysis",
    ("POST", "/analyses/store"): "store_analysis",
    ("POST", "/ingestion/ingest"): "ingest_file",
    ("POST", "/ingestion/upload"): "ingest_file",
    ("POST", "/ingestion/store"): "ingest_file",
    ("POST", "/appeals"): "submit_appeal",
    ("POST", "/knowledge/documents"): "update_knowledge",
    ("POST", "/knowledge/documents/upload"): "update_knowledge",
    ("POST", "/knowledge/search"): "search_knowledge",
    ("GET", "/audit"): "view_audit_log",
    ("GET", "/audit/stats"): "view_audit_log",
}

VERB_ACTIONS = {
    "GET": "view",
    "POST": "create",
    "PATCH": "edit",
    "PUT": "edit",
    "DELETE": "delete",
}


# Networks whose forwarded headers we believe. Anything else sets those
# headers for itself, so believing them lets a caller choose what the audit
# log records about them - and evade any per-address limit by rotating it.
# Defaults to RFC1918 + loopback, which covers the Docker network nginx sits
# on. Narrow it with TRUSTED_PROXIES if the app is reachable directly.
_TRUSTED_PROXY_NETS = [
    ipaddress.ip_network(n.strip())
    for n in os.environ.get(
        "TRUSTED_PROXIES", "127.0.0.0/8,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16"
    ).split(",")
    if n.strip()
]


def _is_trusted_proxy(addr: Optional[str]) -> bool:
    if not addr:
        return False
    try:
        ip = ipaddress.ip_address(addr)
    except ValueError:
        return False
    return any(ip in net for net in _TRUSTED_PROXY_NETS)


def _client_ip(request) -> Optional[str]:
    """The end user's IP, not nginx's - and not one the caller invented.

    The frontend proxies through nginx, so request.client.host is the proxy on
    every request, which is useless for tracing an access back to a person.
    The forwarded headers carry the real address, but anyone can set them: a
    forged X-Forwarded-For put 203.0.113.99 into the compliance log during an
    audit of this system. So they are honoured only when the request actually
    arrived from a proxy we trust, and validated before reaching an INET
    column either way.
    """
    peer = request.client.host if request.client else None

    if _is_trusted_proxy(peer):
        # X-Real-IP first: nginx sets it to $remote_addr, its own view of the
        # peer, which the caller cannot influence.
        real_ip = request.headers.get("x-real-ip")
        if real_ip:
            try:
                return str(ipaddress.ip_address(real_ip.strip()))
            except ValueError:
                pass

        # X-Forwarded-For is built with $proxy_add_x_forwarded_for, which
        # APPENDS our peer to whatever the client sent. The left-most entry is
        # therefore still attacker-controlled - reading it that way recorded a
        # forged 203.0.113.99 even after this function started checking the
        # peer. The last entry is the one our own proxy added.
        forwarded = request.headers.get("x-forwarded-for")
        if forwarded:
            candidate = forwarded.split(",")[-1].strip()
            try:
                return str(ipaddress.ip_address(candidate))
            except ValueError:
                pass

    if peer:
        try:
            return str(ipaddress.ip_address(peer))
        except ValueError:
            return None
    return None


def _identify(request) -> tuple[Optional[str], Optional[str]]:
    """(user_id, username) for the caller.

    Identity resolution lives in services/access.py so that what gets AUDITED
    and what gets ENFORCED can never disagree. Access control resolves it once
    per request and leaves it on request.state; this falls back to resolving it
    directly for the public routes that run before that happens.

    A service caller has no user_id - it is not a person - but is named, so a
    machine write is never filed as an anonymous one.
    """
    who = getattr(request.state, "principal", None) or principal(request)
    return who.user_id, who.username


def _classify(method: str, path: str) -> tuple[str, str, Optional[str]]:
    """(action, resource_type, resource_id) for one request."""
    trimmed = path[len("/api/v1"):] if path.startswith("/api/v1") else path
    segments = [s for s in trimmed.split("/") if s]

    resource_type = RESOURCE_TYPES.get(segments[0], segments[0]) if segments else "system"

    # A trailing UUID is the record that was touched.
    resource_id = None
    for segment in reversed(segments):
        try:
            resource_id = str(uuid.UUID(segment))
            break
        except ValueError:
            continue

    named = NAMED_ACTIONS.get((method, trimmed.rstrip("/")))
    if named:
        return named, resource_type, resource_id

    verb = VERB_ACTIONS.get(method, method.lower())
    # "view_claim" for one record, "list_claim" for a collection - a reviewer
    # cares whether someone opened one chart or pulled the whole census.
    if verb == "view" and resource_id is None:
        verb = "list"
    return f"{verb}_{resource_type}", resource_type, resource_id


async def record(
    action: str,
    resource_type: str,
    resource_id: Optional[str] = None,
    user_id: Optional[str] = None,
    details: Optional[dict] = None,
    ip_address: Optional[str] = None,
    user_agent: Optional[str] = None,
) -> None:
    """Write one audit row. Never raises into the caller."""
    try:
        async with get_connection() as conn:
            await conn.execute(
                """
                INSERT INTO audit_log
                    (user_id, action, resource_type, resource_id, details, ip_address, user_agent)
                VALUES ($1, $2, $3, $4, $5::jsonb, $6::inet, $7)
                """,
                user_id, action, resource_type, resource_id,
                json.dumps(details or {}, default=str),
                ip_address, user_agent,
            )
    except Exception as e:
        # An audit failure must be loud, but it must not take down the request
        # that was already served - the access happened either way, and hiding
        # it behind a 500 would lose the operational signal too.
        logger.error(f"AUDIT WRITE FAILED action={action} resource={resource_type}: {e}")


class AuditMiddleware(BaseHTTPMiddleware):
    """Record every API request against the resource it touched."""

    async def dispatch(self, request, call_next):
        path = request.url.path
        method = request.method

        skip = (
            method == "OPTIONS"
            or path in SKIP_EXACT
            or path.startswith(SKIP_PREFIXES)
            or not path.startswith("/api/")
        )
        if skip:
            return await call_next(request)

        started = time.monotonic()
        response = await call_next(request)
        duration_ms = int((time.monotonic() - started) * 1000)

        action, resource_type, resource_id = _classify(method, path)
        user_id, username = _identify(request)

        details = {
            "method": method,
            "path": path,
            "status_code": response.status_code,
            "duration_ms": duration_ms,
            "outcome": "success" if response.status_code < 400 else "failure",
        }
        if request.url.query:
            details["query"] = request.url.query
        if username:
            details["username"] = username
        elif user_id is None:
            # Worth surfacing: an access nobody can be held accountable for.
            details["username"] = "anonymous"

        await record(
            action=action,
            resource_type=resource_type,
            resource_id=resource_id,
            user_id=user_id,
            details=details,
            ip_address=_client_ip(request),
            user_agent=request.headers.get("user-agent"),
        )
        return response
