#!/usr/bin/env python3
"""Generate crates/api-gateway/openapi/openapi.json.

The detailed request and response schemas below are rendered to the static
document that `/docs`, `/redoc` and `/openapi.json` serve.  The operation
inventory is derived from the Axum route declarations: generation fails if a
Rust route/method is missing from this document or the document describes a
route/method that no longer exists.

    python3 crates/api-gateway/openapi/generate.py
"""
import json
import pathlib
import re
import sys

OUT = pathlib.Path(__file__).with_name("openapi.json")
CLIENT_OUT = pathlib.Path(__file__).parents[3] / "apps" / "web" / "src" / "api" / "generated.ts"
ROUTES_DIR = pathlib.Path(__file__).parents[1] / "src" / "routes"

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


ROUTE_PREFIXES = {
    "auth": "/api/v1/auth",
    "claims": "/api/v1/claims",
    "denials": "/api/v1/denials",
    "analyses": "/api/v1/analyses",
    "appeals": "/api/v1/appeals",
    "ingestion": "/api/v1/ingestion",
    "knowledge": "/api/v1/knowledge",
    "lcd_import": "/api/v1/knowledge/lcd-import",
    "feedback": "/api/v1/feedback",
    "reference": "/api/v1/reference",
    "audit": "/api/v1/audit",
    "users": "/api/v1/users",
    "organizations": "/api/v1/organizations",
    "notifications": "/api/v1/notifications",
    "playbooks": "/api/v1/playbooks",
    "system": "/api/v1/system",
    "retention": "/api/v1/retention",
    "settings": "/api/v1/settings",
    "write_offs": "/api/v1/write-offs",
    "overpayments": "/api/v1/overpayments",
    "payers": "/api/v1/payers",
}
HTTP_METHODS = {"get", "post", "put", "patch", "delete"}


def rust_route_operations():
    """Return operation pairs declared by the Axum route modules.

    This intentionally extracts only the stable Router builder grammar used by
    the gateway. It is not a Rust parser; new routing styles need an explicit
    update here so that schema coverage cannot silently become incomplete.
    """
    operations = set()
    for module, prefix in ROUTE_PREFIXES.items():
        source = (ROUTES_DIR / f"{module}.rs").read_text()
        cursor = 0
        while True:
            start = source.find(".route(", cursor)
            if start < 0:
                break
            index = start + len(".route(")
            depth = 1
            while index < len(source) and depth:
                if source[index] == "(":
                    depth += 1
                elif source[index] == ")":
                    depth -= 1
                index += 1
            if depth:
                raise RuntimeError(f"unclosed .route call in {module}.rs")
            route = source[start + len(".route("):index - 1]
            cursor = index
            match = re.match(r'\s*"([^"]+)"\s*,(.*)', route, flags=re.DOTALL)
            if not match:
                raise RuntimeError(f"unable to read route in {module}.rs: {route!r}")
            suffix, handlers = match.groups()
            path = prefix if suffix == "/" else prefix + suffix
            for method in re.findall(r'\b(get|post|put|patch|delete)\s*\(', handlers):
                operations.add((path, method))
    return operations


def validate_rust_route_coverage(document):
    """Fail when generated documentation and Axum's public API diverge."""
    documented = {
        (path, method)
        for path, methods in document["paths"].items()
        for method in methods
        if method in HTTP_METHODS
    }
    routed = rust_route_operations()
    missing = sorted(routed - documented)
    stale = sorted(documented - routed)
    if missing or stale:
        messages = []
        if missing:
            messages.append("undocumented Rust operations: " + ", ".join(
                f"{method.upper()} {path}" for path, method in missing))
        if stale:
            messages.append("OpenAPI operations absent from Rust routes: " + ", ".join(
                f"{method.upper()} {path}" for path, method in stale))
        raise RuntimeError("; ".join(messages))


# ── auth ──────────────────────────────────────────────────────────────────
add("/api/v1/auth/login",
    post=op("Password login", "auth", security=[],
            body=jbody({"username": S(), "password": S(), "organization_id": S(format="uuid")},
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
                            "system_admin", "security_admin",
                            "revenue_cycle_manager", "billing_specialist",
                            "coding_specialist", "auditor", "read_only"])},
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
add("/api/v1/claims/export.csv",
    get=op("Export claims as CSV (manager+)", "claims",
           params=[{"name": "status", "in": "query", "schema": S()}, P_Q],
           responses={"200": {"description": "CSV export.", "content": {
               "text/csv": {"schema": S()}}}}))
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
               {"name": "payer_name", "in": "query", "schema": S()},
               {"name": "min_amount", "in": "query", "schema": N()},
               {"name": "max_amount", "in": "query", "schema": N()},
               {"name": "min_age_days", "in": "query", "schema": I()},
               {"name": "max_age_days", "in": "query", "schema": I()},
               {"name": "owner", "in": "query", "schema": S(),
                "description": "User UUID, or 'unassigned'."},
               {"name": "facility_type_code", "in": "query", "schema": S()},
               {"name": "cagc", "in": "query", "schema": S()},
               {"name": "claim_id", "in": "query", "schema": S(),
                "description": "Claim NUMBER, not id."},
               P_Q,
               {"name": "priority", "in": "query", "schema": B(),
                "description": "Only denials with a deadline in 14 days."},
               {"name": "sort", "in": "query", "schema": S(enum=["amount", "deadline", "created"]),
                "description": "Queue ordering field."},
               {"name": "descending", "in": "query", "schema": B(),
                "description": "Reverse the selected ordering."},
               {"name": "cursor", "in": "query", "schema": S(),
                "description": "Opaque cursor returned by the prior response."},
               P_LIMIT, P_OFFSET],
           responses={
               "200": {
                   "description": "Cursor page.",
                   "content": {"application/json": {"schema": {
                       "type": "object",
                       "properties": {"items": ARR(OBJ), "next_cursor": S()},
                       "required": ["items"],
                   }}},
               },
           }))
add("/api/v1/denials/bulk-carc",
    get=op("Denial counts and amounts grouped by CARC", "denials"))


def ok(schema):
    return {"200": {"description": "OK", "content": {
        "application/json": {"schema": schema}}},
        "401": {"$ref": "#/components/responses/Unauthorized"},
        "403": {"$ref": "#/components/responses/Forbidden"}}


def exposure_rows(key, key_desc):
    return ARR({"type": "object", "properties": {
        key: S(description=key_desc),
        "denial_count": I(),
        "total_denied_amount": N(),
        "avg_denial_amount": N()},
        "required": [key, "denial_count", "total_denied_amount",
                     "avg_denial_amount"]})


ACTIVE = ("Active denials only (open, analyzed, in_progress, in_appeal) "
          "in the caller's organization.")

by_payer = exposure_rows("payer_name", "'Unknown' when the claim has none.")
by_payer["items"]["properties"].update(
    overdue_count=I(description="Denials past their appeal deadline."),
    nearest_appeal_deadline=S(format="date", nullable=True))
add("/api/v1/denials/by-payer",
    get=op("Active denial exposure grouped by payer", "denials",
           description=ACTIVE + " Sorted by total denied amount.",
           responses=ok(by_payer)))
add("/api/v1/denials/by-root-cause",
    get=op("Active denial exposure grouped by AI root cause", "denials",
           description=ACTIVE + " Uses each denial's latest analysis; "
                       "falls back to its category, then 'Unclassified'.",
           responses=ok(exposure_rows(
               "root_cause", "Root-cause summary, category, or 'Unclassified'."))))
aging = exposure_rows("bucket", "Age since the payer's denial date.")
aging["items"]["properties"]["bucket"]["enum"] = [
    "0–30 days", "31–60 days", "61–90 days", "91+ days", "Unknown"]
aging["items"]["properties"]["bucket_order"] = I(
    description="1–4 by age; 5 for Unknown (no denial date).")
add("/api/v1/denials/aging-buckets",
    get=op("Active denial exposure grouped by age", "denials",
           description=ACTIVE + " Empty buckets are omitted.",
           responses=ok(aging)))
add("/api/v1/denials/financial-summary",
    get=op("Denied vs recovered dollars across all denials", "denials",
           description="Uses each denial's latest recorded resubmission "
                       "outcome; denials with no outcome count as unresolved.",
           responses=ok({"type": "object", "properties": {
               "total_denials": I(),
               "denied_dollars": N(),
               "recovered_dollars": N(),
               "not_recovered_dollars": N(),
               "unresolved_dollars": N(),
               "outcome_known_count": I(),
               "recovered_count": I(),
               "recovery_rate": N(nullable=True, description=
                   "recovered_count / outcome_known_count; null when no "
                   "outcome is known.")},
               "required": ["total_denials", "denied_dollars",
                            "recovered_dollars", "not_recovered_dollars",
                            "unresolved_dollars", "outcome_known_count",
                            "recovered_count", "recovery_rate"]})))
add("/api/v1/denials/resolution-timing",
    get=op("Days from denial to terminal appeal/worklist outcome", "denials",
           description="Counts denials with a denial date and at least one "
                       "queue item in a terminal outcome (approved, overruled, "
                       "resolved, denied_again, cancelled), timed to the most "
                       "recent such item.",
           responses=ok({"type": "object", "properties": {
               "resolved_count": I(),
               "average_resolution_days": N(nullable=True),
               "median_resolution_days": N(nullable=True)},
               "required": ["resolved_count", "average_resolution_days",
                            "median_resolution_days"]})))
add("/api/v1/denials/carc-options",
    get=op("Distinct CARC codes present in denials, for filters", "denials",
           params=[{"name": "status", "in": "query", "schema": S()}]))
add("/api/v1/denials/appeal-windows",
    get=op("Per-payer appeal filing windows", "denials"),
    put=op("Set a payer's appeal window (manager+)", "denials",
           body=jbody({"payer_name": S(), "appeal_window_days": I(),
                       "notes": S()}, ["payer_name", "appeal_window_days"])))
add("/api/v1/denials/deadline-rules",
    get=op("Payer deadline rules for the caller's organization", "denials",
           description="timely_filing counts from the date of service; "
                       "corrected_claim and reconsideration from the remittance "
                       "date; appeal_level_2 from the first appeal's decision. "
                       "payer_name '*' is the organization default."),
    put=op("Create or replace a payer deadline rule (manager+)", "denials",
           body=jbody({"payer_name": S(), "deadline_type": S(enum=[
               "timely_filing", "corrected_claim", "reconsideration",
               "appeal_level_2", "payer_response"]), "days": I(minimum=1, maximum=3650),
               "notes": S()}, ["payer_name", "deadline_type", "days"])))
add("/api/v1/denials/deadline-rules/{rule_id}",
    delete=op("Delete a payer deadline rule (manager+)", "denials",
              params=[path_param("rule_id")]))
add("/api/v1/denials/{denial_id}",
    get=op("One denial with codes, analysis, recommended resolution and deadlines",
           "denials", params=[path_param("denial_id")]),
    patch=op("Update a denial", "denials",
             description="Setting status to written_off at or above the "
                         "organization's write-off approval threshold returns "
                         "202 with status pending_approval and changes nothing "
                         "until a manager approves it (see write-offs).",
             params=[path_param("denial_id")],
             body=jbody({"status": S(),
                         "appeal_deadline": S(format="date")})))
add("/api/v1/denials/{denial_id}/interactions",
    get=op("A denial's payer-interaction history, most recent first", "denials",
           params=[path_param("denial_id")]),
    post=op("Record a payer call or portal action", "denials",
            description="channel is phone, portal, fax, mail, email or other. "
                        "follow_up_on, if the payer promised a date to check "
                        "back by, surfaces this in the deadline digest until "
                        "it is marked complete.",
            params=[path_param("denial_id")],
            body=jbody({"channel": S(), "summary": S(),
                        "occurred_at": S(format="date-time"),
                        "reference_number": S(), "representative": S(),
                        "follow_up_on": S(format="date")},
                       ["channel", "summary"])))
add("/api/v1/denials/{denial_id}/interactions/{interaction_id}/complete",
    post=op("Mark a promised follow-up done", "denials",
            params=[path_param("denial_id"), path_param("interaction_id")]))
add("/api/v1/denials/{denial_id}/attachments",
    get=op("A denial's attached files", "denials", params=[path_param("denial_id")]),
    post=op("Attach a file to a denial (visit notes, an authorization, a "
            "remittance excerpt)", "denials",
            description="Max 25 MB. Every download is audit-logged like any "
                        "other read.",
            params=[path_param("denial_id")],
            body=multipart({"file": S(format="binary")}, ["file"]),
            responses=CREATED))
add("/api/v1/denials/{denial_id}/attachments/{attachment_id}",
    get=op("Download an attachment", "denials",
           description="Returns the file bytes with its original content "
                       "type and filename.",
           params=[path_param("denial_id"), path_param("attachment_id")],
           responses={"200": {"description": "The file.", "content": {
               "application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}},
               "404": {"$ref": "#/components/responses/NotFound"}}),
    delete=op("Remove an attachment", "denials",
              params=[path_param("denial_id"), path_param("attachment_id")]))

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
add("/api/v1/analyses/status",
    get=op("Whether AI analyses are degraded (last 24 h, caller's organization)",
           "analyses",
           description="Degraded when the 3 most recent analyses all fell back "
                       "to deterministic rules or ran without policy evidence, "
                       "or when that share over 24 hours reaches "
                       "AI_DEGRADED_THRESHOLD (default 0.2; needs 3+ analyses).",
           responses={"200": {"description": "OK", "content": {"application/json": {
               "schema": {"type": "object", "properties": {
                   "degraded": B(), "recent_all_degraded": B(),
                   "window_hours": I(), "analyses": I(),
                   "fallback_share": N(), "threshold": N(),
                   "reasons": {"type": "object", "properties": {
                       "llm_error": I(), "retrieval_error": I(),
                       "no_evidence": I()}}}}}}},
               "401": {"$ref": "#/components/responses/Unauthorized"}}))
add("/api/v1/analyses/generate-jobs",
    post=op("Queue asynchronous recommendation generation", "analyses",
            description="Returns a durable job ID; poll its status endpoint for the result.",
            body=jbody({"denial_id": S(format="uuid"),
                        "temperature": N(default=0.3)}, ["denial_id"]),
            responses={"200": {"description": "Queued", "content": {
                "application/json": {"schema": OBJ}}},
                "429": {"description": "Rate limited."},
                "401": {"$ref": "#/components/responses/Unauthorized"}}))
add("/api/v1/analyses/generate-jobs/{job_id}",
    get=op("Get asynchronous recommendation job status", "analyses",
           params=[{"name": "job_id", "in": "path", "required": True,
                    "schema": S(format="uuid")}],
           responses={"200": {"description": "Job status", "content": {
               "application/json": {"schema": OBJ}}},
               "404": {"$ref": "#/components/responses/NotFound"}}))

# ── appeals / worklist ───────────────────────────────────────────────────
add("/api/v1/appeals",
    get=op("The worklist: appeals and non-appeal work", "appeals",
           description="`sort=expected_recovery` orders by open amount x "
                       "historical overturn rate for (payer, CARC) x a "
                       "deadline-urgency factor (1.0-2.0); each row carries "
                       "overturn_rate, overturn_rate_basis (payer_carc, carc "
                       "or prior) and urgency_factor so the UI can show how "
                       "the number was built. Never removes an item, only "
                       "reorders. Default sort is created_at.",
           params=[{"name": "outcome_status", "in": "query", "schema": S()},
                   {"name": "resolution_type", "in": "query", "schema": S()},
                   {"name": "category", "in": "query",
                    "schema": S(enum=["appeal", "worklist"])},
                   {"name": "assigned_user_id", "in": "query",
                    "schema": S(format="uuid")},
                   {"name": "sort", "in": "query",
                    "schema": S(enum=["created_at", "expected_recovery"])},
                   {"name": "descending", "in": "query", "schema": B()},
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
    patch=op("Update outcome / notes / assignment / submission record", "appeals",
             description="Closing a write_off item successfully at or above the "
                         "organization's approval threshold returns 202 with "
                         "status pending_approval and changes nothing until a "
                         "manager approves it (see write-offs). "
                         "submission_method and payer_confirmation_number "
                         "record how and when the packet actually went to "
                         "the payer; setting submission_method requires an "
                         "approved packet for this appeal (400 otherwise), "
                         "so a submission can never be recorded against "
                         "content nobody reviewed.",
             params=[path_param("appeal_id")],
             body=jbody({"outcome_status": S(), "notes": S(),
                         "payer_response": S(format="date"),
                         "final_outcome": S(),
                         "submission_method": S(enum=["portal", "fax", "mail", "email"]),
                         "payer_confirmation_number": S()})))
add("/api/v1/appeals/{appeal_id}/letter",
    get=op("The drafted appeal letter for this item", "appeals",
           params=[path_param("appeal_id")]))
add("/api/v1/appeals/{appeal_id}/assign",
    post=op("Assign the item to a user (manager+)", "appeals",
            params=[path_param("appeal_id")],
            body=jbody({"assigned_user_id": S(format="uuid", nullable=True)})))
add("/api/v1/appeals/{appeal_id}/packet",
    get=op("The appeal packet's status", "appeals",
           params=[path_param("appeal_id")]),
    post=op("(Re)generate the appeal packet", "appeals",
            description="A PDF assembling the claim/remittance summary, the "
                        "draft appeal letter (or the analysis explanation if "
                        "no letter was drafted) and an index of the denial's "
                        "attachments. Replaces any previous packet for this "
                        "appeal and resets its status to draft, since "
                        "regenerating changes content a prior approval "
                        "reviewed.",
            params=[path_param("appeal_id")]))
add("/api/v1/appeals/{appeal_id}/packet/download",
    get=op("Download the packet PDF", "appeals",
           params=[path_param("appeal_id")],
           responses={"200": {"description": "The PDF.", "content": {
               "application/pdf": {"schema": {"type": "string", "format": "binary"}}}},
               "404": {"$ref": "#/components/responses/NotFound"}}))
add("/api/v1/appeals/{appeal_id}/packet/approve",
    post=op("Mark the packet content reviewed", "appeals",
            description="Not a submission record - see PATCH /appeals/{id} "
                        "for submission_method and payer_confirmation_number, "
                        "which track what actually happened with the payer.",
            params=[path_param("appeal_id")]))

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
           description="`expiry=expiring_soon` (within `expiring_within_days`, "
                       "default 30) or `expiry=expired_active` (already past "
                       "expiration but not archived) filters to documents "
                       "flagged by /documents/expiry-summary.",
           params=[{"name": "source_type", "in": "query", "schema": S()},
                   {"name": "status", "in": "query", "schema": S()},
                   {"name": "expiry", "in": "query",
                    "schema": S(enum=["expiring_soon", "expired_active"])},
                   {"name": "expiring_within_days", "in": "query",
                    "schema": I(default=30)},
                   P_LIMIT]),
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
add("/api/v1/knowledge/documents/expiry-summary",
    get=op("Counts of documents expiring soon or already expired but active", "knowledge",
           description="Excludes archived documents either way. Backs the "
                       "Knowledge Base filter and a dashboard count.",
           params=[{"name": "within_days", "in": "query", "schema": I(default=30)}]))
add("/api/v1/knowledge/documents/{document_id}/supersede",
    post=op("Link the document that replaced this one (manager+)", "knowledge",
            description="Sets superseded_by and brings expiration_date forward "
                        "to today if it was later or unset; never pushes an "
                        "earlier expiration date back.",
            params=[path_param("document_id")],
            body=jbody({"new_document_id": S(format="uuid")}, ["new_document_id"])))
add("/api/v1/knowledge/search",
    post=op("Semantic search over indexed policy chunks", "knowledge",
            description="Read-only. Rate limited (KNOWLEDGE_RATE_LIMIT/min).",
            body=jbody({"query": S(), "top_k": I(default=5),
                        "filters": OBJ}, ["query"])))
add("/api/v1/knowledge/reindex",
    post=op("Re-embed one batch of chunks with stale embedding provenance (manager+)", "knowledge",
            description="A config change to EMBEDDING_MODEL or an EMBED_*_PREFIX leaves "
                        "already-embedded chunks in a vector space the new config can no "
                        "longer compare against; those chunks are excluded from vector "
                        "search until re-embedded. Call repeatedly until the response's "
                        "`done` is true - the mismatch count is in Settings > Service "
                        "health. Resumable: each call re-selects whatever still "
                        "mismatches, so it converges regardless of where a previous call "
                        "stopped.",
            body=jbody({"limit": I(default=25)})))
add("/api/v1/knowledge/lcd-import",
    post=op("Stage a CMS bulk LCD export for import (manager+)", "knowledge",
            description="Accepts either format CMS publishes the bulk export "
                        "in - a CSV, or the raw .mdb/.accdb Access database "
                        "it was generated from (detected from the filename, "
                        "or the Jet/ACE file signature if that's missing or "
                        "doesn't say .csv/.mdb/.accdb). Splits it into one "
                        "document per LCD (CMS's export is a bulk file, not "
                        "a single policy) and stages the filtered result "
                        "server-side. Returns a job_id; call POST "
                        "/knowledge/lcd-import/{job_id}/batch repeatedly to "
                        "actually index them. Max 180 MB. Defaults to "
                        "status=A (active) only.",
            params=[{"name": "status", "in": "query", "schema": S(default="A")},
                    {"name": "keyword", "in": "query", "schema": S(),
                     "description": "Comma-separated, OR'd, matched against the LCD title."},
                    {"name": "limit", "in": "query", "schema": I()}],
            body=multipart({"file": S(format="binary")}, ["file"])))
add("/api/v1/knowledge/lcd-import/{job_id}/batch",
    post=op("Index the next batch of a staged LCD import (manager+)", "knowledge",
            description="Call repeatedly until the response's `done` is true. "
                        "Resumable: safe to retry after a failure, since it "
                        "always continues from the job's saved progress.",
            params=[path_param("job_id"),
                    {"name": "limit", "in": "query", "schema": I(default=3)}]))

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
add("/api/v1/feedback/similar-resolved",
    get=op("Find similar successful resolved cases using non-PHI features", "feedback",
           description="Ranks only payer, CARC, CPT, and adjustment group; excludes patient, claim-number, and free-text fields.",
           params=[{"name": "denial_id", "in": "query", "required": True,
                    "schema": S(format="uuid")}, P_LIMIT]))

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
add("/api/v1/reference/ncci/{kind}/import",
    post=op("Preview or apply a CMS NCCI PTP/MUE CSV", "reference",
            params=[{"name": "kind", "in": "path", "required": True,
                     "schema": S(enum=["ptp", "mue"])}],
            body=multipart({"file": S(format="binary"), "apply": B()}, ["file"])))

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

# ── organizations (admin only, platform-level) ──────────────────────────
add("/api/v1/organizations",
    get=op("List organizations, with member counts (admin)", "organizations"),
    post=op("Create an organization and its first system_admin user (admin)",
            "organizations",
            description="The only way to provision a new tenant - nothing "
                        "else in this app can create a membership in an "
                        "organization other than the caller's own.",
            body=jbody({"slug": S(description="lowercase letters, digits, "
                                              "and hyphens only"),
                        "name": S(),
                        "admin_username": S(), "admin_email": S(),
                        "admin_password": S(), "admin_full_name": S()},
                       ["slug", "name", "admin_username", "admin_email",
                        "admin_password"]),
            responses=CREATED))

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
add("/api/v1/retention/ai",
    get=op("AI-analysis history size, age and what a prune would remove "
           "(admin)", "retention",
           description="Scoped to the caller's organization through each "
                       "analysis's claim."))
add("/api/v1/retention/ai/prune",
    post=op("Delete AI analyses past the retention window (admin)",
            "retention",
            description="Organization-scoped. Requires confirm=true; refuses "
                        "a window under the retention floor; logs an "
                        "'ai_analyses_pruned' entry after deleting.",
            body=jbody({"older_than_days": I(), "confirm": B()},
                       ["confirm"])))

# ── provider-level adjustments (PLB) ─────────────────────────────────────
add("/api/v1/ingestion/provider-adjustments",
    get=op("PLB provider-level adjustments from 835s", "ingestion",
           description="Recoupments (WO), forward balances (FB), interest (L6) "
                       "and other payment changes that belong to no patient "
                       "claim. Positive amounts reduced a payment.",
           params=[{"name": "claim_number", "in": "query", "schema": S(),
                    "description": "Only lines whose reference names this claim."},
                   {"name": "reason_code", "in": "query", "schema": S()},
                   P_LIMIT]))
add("/api/v1/ingestion/provider-adjustments/summary",
    get=op("PLB totals by month, payer and reason", "ingestion"))

# ── unanswered claims ────────────────────────────────────────────────────
add("/api/v1/claims/unanswered",
    get=op("Submitted claims with no remittance past the payer's response time",
           "claims",
           description="Claims submitted on an 837 with no 835 after the "
                       "payer_response deadline rule (30 days if none), with "
                       "days left before timely filing when a rule exists. A "
                       "claim leaves the list when its first 835 arrives."))
add("/api/v1/claims/{claim_id}/followups",
    post=op("Record a follow-up on an unanswered claim", "claims",
            params=[path_param("claim_id")],
            body=jbody({"action": S(enum=["status_inquiry", "resubmitted",
                                          "payer_contact"]), "note": S()},
                       ["action"])))

# ── payers and aliases ───────────────────────────────────────────────────
add("/api/v1/payers",
    get=op("Payers with their aliases, and payer names not yet mapped", "payers",
           description="Claims and documents name payers however their source "
                       "spelled them; a document matches a claim when both "
                       "names (or the claim's payer ID) are aliases of one payer."),
    post=op("Create a payer; its name becomes its first alias (manager+)", "payers",
            body=jbody({"name": S()}, ["name"])))
add("/api/v1/payers/{payer_id}",
    delete=op("Delete a payer and its aliases (manager+)", "payers",
              params=[path_param("payer_id")]))
add("/api/v1/payers/{payer_id}/aliases",
    post=op("Add a name or payer-ID alias (manager+)", "payers",
            description="409 when the alias already belongs to a payer.",
            params=[path_param("payer_id")],
            body=jbody({"alias": S(), "kind": S(enum=["name", "payer_id"])}, ["alias"])))
add("/api/v1/payers/{payer_id}/aliases/{alias_id}",
    delete=op("Remove an alias (manager+)", "payers",
              params=[path_param("payer_id"), path_param("alias_id")]))

# ── overpayments ─────────────────────────────────────────────────────────
add("/api/v1/overpayments",
    get=op("Overpayments found in remittances, with refund deadlines", "overpayments",
           description="paid_above_allowed: a line paid more than its allowed "
                       "amount; duplicate_payment: the claim paid again under a "
                       "different payer claim control number with no reversal. "
                       "A PLB WO recoupment naming the claim marks it recouped.",
           params=[{"name": "status", "in": "query",
                    "schema": S(enum=["identified", "refunded", "recouped", "disputed"])}]))
add("/api/v1/overpayments/{id}/status",
    post=op("Record what happened to an overpayment (manager+)", "overpayments",
            params=[path_param("id")],
            body=jbody({"status": S(enum=["identified", "refunded", "recouped", "disputed"]),
                        "note": S()}, ["status"])))

# ── write-off approval ───────────────────────────────────────────────────
add("/api/v1/write-offs",
    get=op("Write-off requests awaiting or past a decision", "write-offs",
           params=[{"name": "status", "in": "query",
                    "schema": S(enum=["pending", "approved", "rejected"],
                                default="pending")}]))
add("/api/v1/write-offs/{id}/approve",
    post=op("Approve a write-off and write the denial off (manager+)",
            "write-offs",
            description="Refused (403) for the person who requested it.",
            params=[path_param("id")],
            body={"required": False, "content": {"application/json": {
                "schema": {"type": "object", "properties": {"note": S()}}}}}))
add("/api/v1/write-offs/{id}/reject",
    post=op("Reject a write-off request (manager+)", "write-offs",
            description="Refused (403) for the person who requested it.",
            params=[path_param("id")],
            body=jbody({"note": S()}, ["note"])))

# ── settings (admin) ─────────────────────────────────────────────────────
add("/api/v1/settings/phi-disclosure",
    get=op("PHI disclosure level for AI prompts (admin)", "settings"),
    put=op("Set the PHI disclosure level (admin)", "settings",
           body=jbody({"level": S(enum=["none", "deidentified",
                                        "limited_phi", "full_context"])},
                      ["level"])))
add("/api/v1/settings/overpayment-refund",
    get=op("Days from identifying an overpayment to its refund deadline (admin)",
           "settings"),
    put=op("Set the overpayment refund window (admin)", "settings",
           description="Many payers set this by rule (60 days for Medicare); "
                       "confirm it with compliance staff.",
           body=jbody({"days": I(minimum=1, maximum=3650)}, ["days"])))
add("/api/v1/settings/write-off-approval",
    get=op("Write-off approval threshold for the caller's organization (admin)",
           "settings"),
    put=op("Set the write-off approval threshold (admin)", "settings",
           description="Write-offs at or above this amount need a manager's "
                       "approval; 0 means every write-off does.",
           body=jbody({"threshold": N(minimum=0)}, ["threshold"])))

# ── playbooks ────────────────────────────────────────────────────────────
PLAYBOOK_INPUT = {
    "name": S(), "description": S(), "triggers": OBJ, "recommendation": OBJ,
}
add("/api/v1/playbooks",
    get=op("List institutional playbooks", "playbooks",
           params=[{"name": "status", "in": "query", "schema": S()}]),
    post=op("Create an institutional playbook (manager+)", "playbooks",
            body=jbody(PLAYBOOK_INPUT, ["name"]), responses=CREATED))
add("/api/v1/playbooks/test",
    post=op("Find approved playbooks matching denial facts", "playbooks",
            body=jbody({"carc_code": S(), "cagc": S(), "payer_name": S()})))
add("/api/v1/playbooks/{id}",
    post=op("Update a playbook and return it to draft (manager+)", "playbooks",
            params=[path_param("id")], body=jbody(PLAYBOOK_INPUT, ["name"])))
add("/api/v1/playbooks/{id}/approve",
    post=op("Approve a draft playbook (manager+)", "playbooks",
            params=[path_param("id")]))
add("/api/v1/playbooks/{id}/archive",
    post=op("Archive a playbook (manager+)", "playbooks",
            params=[path_param("id")]))

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
            ("organizations", "Tenant provisioning (admin)"),
            ("notifications", "Deadline digests and escalations"),
            ("playbooks", "Manager-curated deterministic resolution rules"),
            ("write-offs", "Write-off approval above the organization threshold"),
            ("overpayments", "Overpayments and their refund deadlines"),
            ("payers", "Payers and the names and IDs that resolve to them"),
            ("settings", "Organization and system settings (admin)"),
            ("system", "Health"),
            ("retention", "Audit-log and AI-analysis retention (admin)"),
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

def ts_type(schema):
    """Return a conservative TypeScript representation for an OpenAPI schema."""
    if not schema:
        return "unknown"
    if "enum" in schema:
        return " | ".join(json.dumps(value) for value in schema["enum"])
    kind = schema.get("type")
    if kind == "string":
        return "string"
    if kind in ("integer", "number"):
        return "number"
    if kind == "boolean":
        return "boolean"
    if kind == "array":
        return f"Array<{ts_type(schema.get('items'))}>"
    if kind == "object":
        properties = schema.get("properties", {})
        if not properties:
            return "Record<string, unknown>"
        required = set(schema.get("required", []))
        members = []
        for name, value in properties.items():
            optional = "" if name in required else "?"
            members.append(f"{json.dumps(name)}{optional}: {ts_type(value)}")
        if schema.get("additionalProperties"):
            members.append("[key: string]: unknown")
        return "{ " + "; ".join(members) + " }"
    return "unknown"


def operation_type(operation):
    path_params = {}
    query_params = {}
    required_query = set()
    for parameter in operation.get("parameters", []):
        location = parameter.get("in")
        if location not in ("path", "query"):
            continue
        target = path_params if location == "path" else query_params
        target[parameter["name"]] = ts_type(parameter.get("schema"))
        if location == "query" and parameter.get("required"):
            required_query.add(parameter["name"])

    content = operation.get("requestBody", {}).get("content", {})
    body = None
    if "application/json" in content:
        body = ts_type(content["application/json"].get("schema"))
    elif content:
        # Upload callers deliberately provide FormData so file bodies are not
        # accidentally JSON encoded.
        body = "FormData"

    parts = []
    if path_params:
        fields = "; ".join(f"{json.dumps(k)}: {v}" for k, v in path_params.items())
        parts.append(f"path: {{ {fields} }}")
    if query_params:
        fields = "; ".join(
            f"{json.dumps(k)}{' ' if k in required_query else '?'}: {v}"
            for k, v in query_params.items())
        parts.append(f"query?: {{ {fields} }}")
    if body:
        optional = "" if operation.get("requestBody", {}).get("required") else "?"
        parts.append(f"body{optional}: {body}")
    return "{ " + "; ".join(parts) + " }" if parts else "Record<string, never>"


def generate_client(document):
    operations = []
    for path, methods in document["paths"].items():
        for method, operation in methods.items():
            if method not in {"get", "post", "put", "patch", "delete"}:
                continue
            operations.append((f"{method.upper()} {path}", operation_type(operation)))
    definitions = "\n".join(f"  {json.dumps(key)}: {value};" for key, value in operations)
    return f'''// This file is generated by crates/api-gateway/openapi/generate.py. Do not edit.
// Regenerate with: pnpm generate:api

export type ApiOperation = keyof ApiOperations

export interface ApiOperations {{
{definitions}
}}

export type ApiRequestOptions<Operation extends ApiOperation> = ApiOperations[Operation] & {{
  signal?: AbortSignal
  headers?: HeadersInit
}}

export class ApiError extends Error {{
  constructor(public readonly response: Response, public readonly payload: unknown) {{
    super(`API request failed (${{response.status}})`)
    this.name = 'ApiError'
  }}
}}

/** Small fetch client. The app's fetch wrapper supplies bearer auth. */
export class ApiClient {{
  constructor(private readonly baseUrl = '') {{}}

  async request<Operation extends ApiOperation, Response = unknown>(
    operation: Operation,
    options: ApiRequestOptions<Operation> = {{}} as ApiRequestOptions<Operation>,
  ): Promise<Response> {{
    const [method, template] = operation.split(' ', 2) as [string, string]
    const pathValues = (options as {{ path?: Record<string, string | number> }}).path || {{}}
    const path = template.replace(/{{([^}}]+)}}/g, (_, key) => encodeURIComponent(String(pathValues[key])))
    const query = (options as {{ query?: Record<string, string | number | boolean | undefined> }}).query
    const search = new URLSearchParams()
    if (query) for (const [key, value] of Object.entries(query)) if (value !== undefined) search.set(key, String(value))
    const body = (options as {{ body?: unknown }}).body
    const headers = new Headers(options.headers)
    const init: RequestInit = {{ method, headers, signal: options.signal }}
    if (body instanceof FormData) init.body = body
    else if (body !== undefined) {{ headers.set('Content-Type', 'application/json'); init.body = JSON.stringify(body) }}
    const response = await fetch(`${{this.baseUrl}}${{path}}${{search.size ? `?${{search}}` : ''}}`, init)
    if (response.status === 204) return undefined as Response
    const payload = await response.json().catch(() => undefined)
    if (!response.ok) throw new ApiError(response, payload)
    return payload as Response
  }}
}}

export const api = new ApiClient()
'''


validate_rust_route_coverage(doc)

json_output = json.dumps(doc, indent=2) + "\n"
client_output = generate_client(doc)
if "--check" in sys.argv[1:]:
    stale = (not OUT.exists() or OUT.read_text() != json_output
             or not CLIENT_OUT.exists() or CLIENT_OUT.read_text() != client_output)
    if stale:
        print("OpenAPI artifacts are stale; run: pnpm generate:api", file=sys.stderr)
        sys.exit(1)
else:
    OUT.write_text(json_output)
    CLIENT_OUT.parent.mkdir(parents=True, exist_ok=True)
    CLIENT_OUT.write_text(client_output)
    print(f"wrote {OUT} and {CLIENT_OUT} ({len(doc['paths'])} paths)")
