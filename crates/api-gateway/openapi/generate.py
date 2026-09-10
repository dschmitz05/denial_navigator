#!/usr/bin/env python3
"""Generate crates/api-gateway/openapi/openapi.json.

The Rust gateway has no framework-level schema reflection (unlike FastAPI), so
the API description is maintained here by hand and rendered to a static
document that `/docs`, `/redoc` and `/openapi.json` serve. Keep this in step
with crates/api-gateway/src/routes/*.rs.

    python3 crates/api-gateway/openapi/generate.py
"""
import json
import pathlib

OUT = pathlib.Path(__file__).with_name("openapi.json")

BEARER = [{"bearerAuth": []}]
SERVICE = [{"serviceKey": []}]

# Reusable parameter fragments.
P_LIMIT = {"name": "limit", "in": "query", "schema": {"type": "integer"},
           "description": "Max rows to return."}
P_OFFSET = {"name": "offset", "in": "query", "schema": {"type": "integer"}}
P_Q = {"name": "q", "in": "query", "schema": {"type": "string"},
       "description": "Free-text search."}


def path_param(name, desc="Resource id.", fmt="uuid"):
    schema = {"type": "string"}
    if fmt:
        schema["format"] = fmt
    return {"name": name, "in": "path", "required": True, "schema": schema,
            "description": desc}


def op(summary, tag, *, security=BEARER, params=None, body=None,
       responses=None, description=None):
    o = {"summary": summary, "tags": [tag], "security": security}
    if description:
        o["description"] = description
    if params:
        o["parameters"] = params
    if body is not None:
        o["requestBody"] = body
    o["responses"] = responses or {
        "200": {"description": "OK", "content": {"application/json": {
            "schema": {"type": "object", "additionalProperties": True}}}},
        "401": {"$ref": "#/components/responses/Unauthorized"},
        "403": {"$ref": "#/components/responses/Forbidden"},
    }
    return o


def jbody(props, required=None, *, desc=None):
    schema = {"type": "object", "properties": props}
    if required:
        schema["required"] = required
    if not required and not props:
        schema["additionalProperties"] = True
    b = {"required": True, "content": {"application/json": {"schema": schema}}}
    if desc:
        b["description"] = desc
    return b


def multipart(props, required=None):
    schema = {"type": "object", "properties": props}
    if required:
        schema["required"] = required
    return {"required": True,
            "content": {"multipart/form-data": {"schema": schema}}}


S = lambda **kw: {"type": "string", **kw}
I = lambda **kw: {"type": "integer", **kw}
N = lambda **kw: {"type": "number", **kw}
B = lambda **kw: {"type": "boolean", **kw}
ARR = lambda items: {"type": "array", "items": items}
OBJ = {"type": "object", "additionalProperties": True}

CREATED = {"201": {"description": "Created", "content": {"application/json": {
    "schema": OBJ}}},
    "401": {"$ref": "#/components/responses/Unauthorized"},
    "403": {"$ref": "#/components/responses/Forbidden"}}

paths = {}


def add(path, **methods):
    paths[path] = methods


# ── auth ──────────────────────────────────────────────────────────────────
add("/api/v1/auth/login",
    post=op("Password login", "auth", security=[],
            body=jbody({"username": S(), "password": S()},
                       ["username", "password"]),
            responses={"200": {"description": "A full-scope token, or an "
                               "mfa-scope token when TOTP is required.",
                               "content": {"application/json": {"schema": OBJ}}},
                       "401": {"$ref": "#/components/responses/Unauthorized"},
                       "429": {"description": "Login throttled (per-account or "
                               "per-IP failure budget exceeded)."}}))
add("/api/v1/auth/login/totp",
    post=op("Complete login with a TOTP code", "auth", security=BEARER,
            description="Exchanges an mfa-scope token for a full one.",
            body=jbody({"code": S(description="6-digit TOTP")}, ["code"])))
add("/api/v1/auth/me",
    get=op("The account behind the current token", "auth"))
add("/api/v1/auth/register",
    post=op("Create a user (admin only)", "auth",
            body=jbody({"username": S(), "email": S(), "password": S(),
                        "full_name": S(), "role": S(enum=[
                            "billing_specialist", "billing_manager",
                            "rcm_director", "admin"])},
                       ["username", "email", "password", "role"]),
            responses=CREATED))
add("/api/v1/auth/change-password",
    post=op("Change your own password", "auth",
            body=jbody({"current_password": S(), "new_password": S()},
                       ["current_password", "new_password"])))
add("/api/v1/auth/totp/enroll",
    post=op("Begin TOTP enrolment", "auth",
            description="Returns an otpauth URI and QR SVG. Requires a "
                        "full-scope or mfa-scope token."))
add("/api/v1/auth/totp/confirm",
    post=op("Confirm TOTP enrolment with a code", "auth",
            body=jbody({"code": S()}, ["code"])))

# ── claims ────────────────────────────────────────────────────────────────
add("/api/v1/claims",
    get=op("List claims", "claims",
           params=[{"name": "status", "in": "query", "schema": S()},
                   P_Q, P_LIMIT, P_OFFSET]),
    post=op("Create a claim (manager+)", "claims",
            body=jbody({"claim_number": S(), "patient_id": S(),
                        "payer_name": S(), "total_charge": N(),
                        "icd_10_codes": ARR(S())},
                       ["claim_number", "patient_id", "payer_name",
                        "total_charge"]),
            responses=CREATED))
add("/api/v1/claims/dashboard/stats",
    get=op("Dashboard totals", "claims"))
add("/api/v1/claims/{claim_id}",
    get=op("One claim with its denials and analyses", "claims",
           params=[path_param("claim_id")]),
    patch=op("Update a claim (manager+)", "claims",
             params=[path_param("claim_id")],
             body=jbody({"status": S(), "total_paid": N(),
                         "total_adjustment": N()})))

# ── denials ───────────────────────────────────────────────────────────────
add("/api/v1/denials",
    get=op("List denials (open + analyzed by default)", "denials",
           params=[
               {"name": "status", "in": "query", "schema": S()},
               {"name": "carc_code", "in": "query", "schema": S()},
               {"name": "cagc", "in": "query", "schema": S()},
               {"name": "claim_id", "in": "query", "schema": S(),
                "description": "Claim NUMBER, not id."},
               P_Q,
               {"name": "priority", "in": "query", "schema": B(),
                "description": "Only denials with a deadline in 14 days."},
               P_LIMIT, P_OFFSET]))
add("/api/v1/denials/bulk-carc",
    get=op("Denial counts and amounts grouped by CARC", "denials"))
add("/api/v1/denials/carc-options",
    get=op("Distinct CARC codes present in denials, for filters", "denials",
           params=[{"name": "status", "in": "query", "schema": S()}]))
add("/api/v1/denials/appeal-windows",
    get=op("Per-payer appeal filing windows", "denials"),
    put=op("Set a payer's appeal window (manager+)", "denials",
           body=jbody({"payer_name": S(), "appeal_window_days": I(),
                       "notes": S()}, ["payer_name", "appeal_window_days"])))
add("/api/v1/denials/{denial_id}",
    get=op("One denial with codes, analysis and recommended resolution",
           "denials", params=[path_param("denial_id")]),
    patch=op("Update a denial", "denials",
             params=[path_param("denial_id")],
             body=jbody({"status": S(),
                         "appeal_deadline": S(format="date")})))

# ── analyses ──────────────────────────────────────────────────────────────
add("/api/v1/analyses",
    get=op("List AI analyses", "analyses",
           params=[{"name": "denial_id", "in": "query",
                    "schema": S(format="uuid")},
                   {"name": "claim_id", "in": "query",
                    "schema": S(format="uuid")}, P_LIMIT]))
add("/api/v1/analyses/store",
    post=op("Store an analysis result (usually called by the LLM service)",
            "analyses", security=BEARER + SERVICE,
            body=jbody({"denial_id": S(format="uuid"),
                        "claim_id": S(format="uuid"), "model_name": S(),
                        "raw_prompt": S(), "raw_response": S(),
                        "parsed_result": OBJ, "prompt_tokens": I(),
                        "completion_tokens": I(), "total_tokens": I()},
                       ["denial_id", "claim_id", "model_name", "raw_prompt",
                        "raw_response", "parsed_result"])))
add("/api/v1/analyses/generate",
    post=op("Run the full pipeline for a denial (RAG + LLM)", "analyses",
            description="Rate limited per caller (ANALYSES_RATE_LIMIT/min).",
            body=jbody({"denial_id": S(format="uuid"),
                        "temperature": N(default=0.3)}, ["denial_id"]),
            responses={"200": {"description": "OK", "content": {
                "application/json": {"schema": OBJ}}},
                "404": {"$ref": "#/components/responses/NotFound"},
                "429": {"description": "Rate limited."},
                "401": {"$ref": "#/components/responses/Unauthorized"}}))

# ── appeals / worklist ───────────────────────────────────────────────────
add("/api/v1/appeals",
    get=op("The worklist: appeals and non-appeal work", "appeals",
           params=[{"name": "status", "in": "query", "schema": S()},
                   {"name": "resolution_type", "in": "query", "schema": S()},
                   {"name": "assigned", "in": "query", "schema": S(),
                    "description": "'me', 'unassigned', or a user id."},
                   P_LIMIT, P_OFFSET]),
    post=op("Queue a denial for a resolution", "appeals",
            body=jbody({"denial_id": S(format="uuid"),
                        "resolution_type": S(), "notes": S()},
                       ["denial_id", "resolution_type"]),
            responses={**CREATED,
                       "409": {"$ref": "#/components/responses/Conflict"}}))
add("/api/v1/appeals/bulk",
    post=op("Queue many denials in one call", "appeals",
            body=jbody({"denial_ids": ARR(S(format="uuid")),
                        "resolution_type": S()},
                       ["denial_ids", "resolution_type"])))
add("/api/v1/appeals/{appeal_id}",
    get=op("One worklist item", "appeals",
           params=[path_param("appeal_id")]),
    patch=op("Update outcome / notes / assignment", "appeals",
             params=[path_param("appeal_id")],
             body=jbody({"outcome_status": S(), "notes": S(),
                         "payer_response": S(format="date"),
                         "final_outcome": S()})))
add("/api/v1/appeals/{appeal_id}/letter",
    get=op("The drafted appeal letter for this item", "appeals",
           params=[path_param("appeal_id")]))
add("/api/v1/appeals/{appeal_id}/assign",
    post=op("Assign the item to a user (manager+)", "appeals",
            params=[path_param("appeal_id")],
            body=jbody({"assigned_user_id": S(format="uuid", nullable=True)})))

# ── ingestion ────────────────────────────────────────────────────────────
add("/api/v1/ingestion/upload",
    post=op("Upload an 835/837 file, parse and store it", "ingestion",
            description="Rate limited per caller (INGESTION_RATE_LIMIT/min); "
                        "max 25 MB.",
            body=multipart({"file": S(format="binary")}, ["file"])))
add("/api/v1/ingestion/ingest",
    post=op("Trigger a dropzone sweep on the parser", "ingestion"))
add("/api/v1/ingestion/store",
    post=op("Store an already-parsed result (called by the parser)",
            "ingestion", security=BEARER + SERVICE,
            body=jbody({"file_name": S(), "file_hash": S(), "file_size": I(),
                        "claims": ARR(OBJ), "denials": ARR(OBJ)},
                       ["file_name", "file_hash", "claims", "denials"]),
            responses={"200": {"description": "stored"},
                       "409": {"$ref": "#/components/responses/Conflict"}}))
add("/api/v1/ingestion/log",
    post=op("List ingestion-log entries (read; POST for the filter body)",
            "ingestion", body=jbody({}, desc="Optional filter object.")))
add("/api/v1/ingestion/history",
    get=op("Ingestion history", "ingestion", params=[P_LIMIT]))

# ── knowledge base ───────────────────────────────────────────────────────
add("/api/v1/knowledge/documents",
    get=op("List policy documents (with chunk counts)", "knowledge",
           params=[{"name": "source_type", "in": "query", "schema": S()},
                   {"name": "status", "in": "query", "schema": S()}, P_LIMIT]),
    post=op("Create a document, indexing it if content is supplied "
            "(manager+)", "knowledge",
            body=jbody({"title": S(), "source_type": S(), "payer_id": S(),
                        "payer_name": S(), "effective_date": S(format="date"),
                        "content": S()}, ["title", "source_type"]),
            responses=CREATED))
add("/api/v1/knowledge/documents/upload",
    post=op("Upload a PDF / text policy document and index it (manager+)",
            "knowledge",
            description="PDF text is extracted server-side; scans are "
                        "rejected. Rate limited (KNOWLEDGE_RATE_LIMIT/min); "
                        "max 25 MB.",
            params=[{"name": "title", "in": "query", "schema": S()},
                    {"name": "source_type", "in": "query", "schema": S()},
                    {"name": "payer_name", "in": "query", "schema": S()}],
            body=multipart({"file": S(format="binary")}, ["file"]),
            responses=CREATED))
add("/api/v1/knowledge/documents/{document_id}",
    get=op("A document and its full indexed text", "knowledge",
           params=[path_param("document_id")]),
    delete=op("Archive (default) or purge a document (manager+)", "knowledge",
              params=[path_param("document_id"),
                      {"name": "purge", "in": "query", "schema": B(),
                       "description": "Delete the row instead of archiving."}]))
add("/api/v1/knowledge/documents/{document_id}/content",
    post=op("Attach text to a document and index it (manager+)", "knowledge",
            params=[path_param("document_id")],
            body=jbody({"content": S()}, ["content"])))
add("/api/v1/knowledge/search",
    post=op("Semantic search over indexed policy chunks", "knowledge",
            description="Read-only. Rate limited (KNOWLEDGE_RATE_LIMIT/min).",
            body=jbody({"query": S(), "top_k": I(default=5),
                        "filters": OBJ}, ["query"])))

# ── feedback ─────────────────────────────────────────────────────────────
add("/api/v1/feedback",
    get=op("List feedback records", "feedback",
           params=[{"name": "ai_analysis_id", "in": "query",
                    "schema": S(format="uuid")},
                   {"name": "accepted", "in": "query", "schema": B()},
                   P_LIMIT]),
    post=op("Record feedback on an analysis", "feedback",
            body=jbody({"ai_analysis_id": S(format="uuid"),
                        "user_id": S(format="uuid"),
                        "rating": I(minimum=1, maximum=5),
                        "accepted": B(), "user_edits": OBJ,
                        "action_taken": S(),
                        "was_paid_on_resubmit": B(),
                        "resubmit_result": S(), "feedback_text": S()},
                       ["ai_analysis_id"]),
            responses=CREATED))
add("/api/v1/feedback/analytics",
    get=op("Aggregate recommendation performance", "feedback",
           description="Rates use honest denominators (only rows where the "
                       "outcome is known)."))

# ── reference code lists ─────────────────────────────────────────────────
KIND = {"name": "kind", "in": "path", "required": True,
        "schema": S(enum=["carc", "rarc", "icd10", "cpt", "hcpcs",
                          "modifier"]),
        "description": "Which code list."}
add("/api/v1/reference/summary",
    get=op("Row counts and last-import time for every list", "reference"))
add("/api/v1/reference/{kind}/import",
    post=op("Import a code list from CSV (manager+)", "reference",
            description="Upsert on code. `apply=false` (default) previews; "
                        "`apply=true` writes and logs to reference_imports. "
                        "Caps: CARC/RARC/modifier 1 MB, others 50 MB.",
            params=[KIND],
            body=multipart({"file": S(format="binary"),
                            "apply": S(enum=["true", "false"])}, ["file"])))
add("/api/v1/reference/{kind}/search",
    get=op("Search a code list by code or description", "reference",
           params=[KIND, P_Q, P_LIMIT, P_OFFSET]))
add("/api/v1/reference/{kind}/delete",
    post=op("Delete one code from a list (manager+)", "reference",
            params=[KIND], body=jbody({"code": S()}, ["code"])))
add("/api/v1/reference/{kind}/clear",
    post=op("Clear an entire list (manager+)", "reference", params=[KIND],
            body=jbody({"confirm": B()}, ["confirm"])))

# ── audit ────────────────────────────────────────────────────────────────
add("/api/v1/audit",
    get=op("Audit log entries (manager+)", "audit",
           params=[
               {"name": "action", "in": "query", "schema": S()},
               {"name": "resource_type", "in": "query", "schema": S()},
               {"name": "user_id", "in": "query", "schema": S(format="uuid")},
               {"name": "username", "in": "query", "schema": S(),
                "description": "Actor name, incl. 'service:*' / 'anonymous'."},
               {"name": "start_date", "in": "query",
                "schema": S(format="date-time")},
               {"name": "end_date", "in": "query",
                "schema": S(format="date-time")},
               P_LIMIT, P_OFFSET]))
add("/api/v1/audit/actors",
    get=op("Everyone who appears in the log, with counts (manager+)", "audit"))
add("/api/v1/audit/stats",
    get=op("Audit summary statistics (manager+)", "audit"))

# ── users (admin only) ───────────────────────────────────────────────────
add("/api/v1/users",
    get=op("List users (admin)", "users"))
add("/api/v1/users/assignable",
    get=op("Users a worklist item can be assigned to (manager+)", "users"))
add("/api/v1/users/{user_id}",
    get=op("One user (admin)", "users", params=[path_param("user_id")]),
    patch=op("Update a user (admin)", "users",
             params=[path_param("user_id")],
             body=jbody({"email": S(), "full_name": S(), "role": S(),
                         "is_active": B()})),
    delete=op("Deactivate a user and release their queue items (admin)",
              "users", params=[path_param("user_id")]))
add("/api/v1/users/{user_id}/password",
    post=op("Set a user's password (admin)", "users",
            params=[path_param("user_id")],
            body=jbody({"new_password": S()}, ["new_password"])))
add("/api/v1/users/{user_id}/totp",
    post=op("Set a user's TOTP requirement (admin)", "users",
            params=[path_param("user_id")],
            body=jbody({"totp_required": B()}, ["totp_required"])))
add("/api/v1/users/{user_id}/totp/reset",
    post=op("Clear a user's enrolled TOTP secret (admin)", "users",
            params=[path_param("user_id")]))

# ── notifications ────────────────────────────────────────────────────────
add("/api/v1/notifications",
    get=op("Your notifications", "notifications",
           params=[{"name": "unread_only", "in": "query", "schema": B()},
                   P_LIMIT]))
add("/api/v1/notifications/read-all",
    post=op("Mark all your notifications read", "notifications"))
add("/api/v1/notifications/{notification_id}/read",
    post=op("Mark one notification read", "notifications",
            params=[path_param("notification_id")]))
add("/api/v1/notifications/generate-digests",
    post=op("Build today's deadline digests (admin or service)",
            "notifications", security=BEARER + SERVICE,
            description="Idempotent per (user, kind, day). Called from cron."))

# ── system ───────────────────────────────────────────────────────────────
add("/api/v1/system/health",
    get=op("Dependency health with latencies", "system", security=[]))

# ── retention (admin only) ───────────────────────────────────────────────
add("/api/v1/retention/audit",
    get=op("Audit-log size, age and what a prune would remove (admin)",
           "retention"))
add("/api/v1/retention/audit/prune",
    post=op("Delete audit entries past the retention window (admin)",
            "retention",
            description="Requires confirm=true; refuses a window under a "
                        "year; logs an 'audit_pruned' entry after deleting.",
            body=jbody({"older_than_days": I(), "confirm": B()},
                       ["confirm"])))

doc = {
    "openapi": "3.1.0",
    "info": {
        "title": "Denial Navigator API",
        "version": "1.0.0",
        "description": (
            "Self-hosted denial-management API (Rust / axum rewrite). Every "
            "browser request goes through this gateway; it enforces "
            "authentication, role-based authorisation, per-request account "
            "currency and HIPAA access auditing.\n\n"
            "Auth: `POST /api/v1/auth/login` returns a bearer token. When TOTP "
            "is required it returns an mfa-scope token usable only on the "
            "`/auth/totp/*`, `/auth/login/totp` and `/auth/me` paths until "
            "`POST /auth/login/totp` upgrades it. Sibling services present "
            "`X-Service-Key` instead."),
    },
    "servers": [
        {"url": "/", "description": "This gateway"},
    ],
    "tags": [
        {"name": t, "description": d} for t, d in [
            ("auth", "Login, TOTP, self-service password"),
            ("claims", "Claims and the dashboard rollup"),
            ("denials", "Denied service lines, CARC filters, appeal windows"),
            ("analyses", "LLM denial analyses and the generate pipeline"),
            ("appeals", "The operational worklist (appeals and other work)"),
            ("ingestion", "835/837 upload, parse and store"),
            ("knowledge", "Payer-policy documents and semantic search"),
            ("feedback", "Recommendation outcomes and analytics"),
            ("reference", "CARC/RARC/ICD-10/CPT/HCPCS/modifier code lists"),
            ("audit", "HIPAA access log (manager+)"),
            ("users", "User administration (admin)"),
            ("notifications", "Deadline digests and escalations"),
            ("system", "Health"),
            ("retention", "Audit-log retention (admin)"),
        ]
    ],
    "components": {
        "securitySchemes": {
            "bearerAuth": {"type": "http", "scheme": "bearer",
                           "bearerFormat": "JWT"},
            "serviceKey": {"type": "apiKey", "in": "header",
                           "name": "X-Service-Key"},
        },
        "responses": {
            "Unauthorized": {"description": "Missing or invalid credentials.",
                             "content": {"application/json": {"schema": {
                                 "type": "object",
                                 "properties": {"detail": {"type": "string"}}}}}},
            "Forbidden": {"description": "Authenticated but not allowed.",
                          "content": {"application/json": {"schema": {
                              "type": "object",
                              "properties": {"detail": {"type": "string"}}}}}},
            "NotFound": {"description": "No such resource.",
                         "content": {"application/json": {"schema": {
                             "type": "object",
                             "properties": {"detail": {"type": "string"}}}}}},
            "Conflict": {"description": "Conflicts with existing state.",
                         "content": {"application/json": {"schema": {
                             "type": "object",
                             "properties": {"detail": {"type": "string"}}}}}},
        },
    },
    "security": BEARER,
    "paths": dict(sorted(paths.items())),
}

OUT.write_text(json.dumps(doc, indent=2) + "\n")
print(f"wrote {OUT} ({len(doc['paths'])} paths)")
