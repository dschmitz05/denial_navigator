"""Reference-list imports, search, and deletion: six code lists.

CARC, RARC, ICD-10, CPT, HCPCS Level II, and modifiers. X12 revises the
published CARC list a few times a year, the in-house RARC list grows as
payers are added, ICD-10-CM / CPT change every October, and CMS refreshes
the HCPCS / modifier lists the same way. This module is how all six lists
get refreshed: a manager uploads the exported CSV, previews what would
change (nothing is written), and applies the import. ICD-10, CPT, HCPCS
and modifiers start empty and are populated by the first import - the
official lists are large, so they are not seeded in SQL. The same module
also serves search (browse one list by code or description) and
deletion (specific codes, or clear a whole list).

Design notes:

- The import is an UPSERT on the code. Codes the file mentions are
  inserted or updated; codes it does NOT mention are left exactly as
  they are. A partial export must not silently deactivate half the
  reference table - deactivation is only ever explicit, via the file's
  own active/status column.
- Every applied import is written to reference_imports (who, which
  file, what changed, when), and the access audit middleware records
  the request itself like any other.
- Column headers are mapped by alias, not by exact name: the published
  lists do not share one CSV format, and a human export is what this
  endpoint exists to accept.
"""

import csv
import io
import json
import logging
from datetime import date, datetime
from typing import Literal, Optional

from fastapi import APIRouter, File, Form, HTTPException, Query, Request, UploadFile
from pydantic import BaseModel

from api_gateway.services.access import principal
from api_gateway.services.db import get_connection

router = APIRouter()
logger = logging.getLogger("api_gateway.reference")

# CARC/RARC are small hand-maintained lists (the full CARC list is under
# 100 KB), so 1 MB bounds how much a mistaken upload (the wrong file, a
# full 835, a screenshot) can cost. ICD-10-CM and CPT are different: the
# official CMS/AMA downloads are ~15 MB and ~3 MB, and those ARE the files
# a manager will upload, so they get a much larger ceiling.
MAX_FILE_BYTES = {"carc": 1 << 20, "rarc": 1 << 20,
                  "icd10": 50 << 20, "cpt": 50 << 20,
                  "modifier": 1 << 20, "hcpcs": 50 << 20}

# kind -> table.
TABLES = {
    "carc": "carc_codes",
    "rarc": "rarc_codes",
    "icd10": "icd10_codes",
    "cpt": "cpt_codes",
    "modifier": "modifier_codes",
    "hcpcs": "hcpcs_codes",
}

# Every endpoint in this module takes one of these kinds; FastAPI turns the
# Literal into a 422 for anything else.
ReferenceKind = Literal["carc", "rarc", "icd10", "cpt", "modifier", "hcpcs"]

# kind -> the one extra column besides code/description/dates/active.
EXTRA_COLUMN = {"carc": "category", "rarc": "applicable_cagc"}

# Header aliases, lower-cased. A column is recognised by its name, so the
# same importer copes with the X12 export, a hand-typed CSV, and the SQL
# seed's column names.
ALIASES = {
    "code": {
        "code", "carc", "carc code", "carc_code", "rarc", "rarc code", "rarc_code", "code #",
        # ICD-10-CM (CMS download) and CPT (AMA / vendor exports).
        "icd10code", "icd10 code", "icd 10 code", "icd10", "diagnosis code", "diagnosable code",
        "cptcode", "cpt code", "cpt", "procedure code", "hcpcs code",
        # HCPCS Level II (CMS code status file) and modifiers (CMS modifier list).
        "hcpcs", "hcpcs code number", "code number", "service code", "service/procedure code",
        "modifier", "modifier code", "modifier_code", "mod", "mod #", "modifier #",
    },
    "description": {
        "description", "desc", "meaning", "definition", "text", "remark", "remark text",
        "short description", "long description", "shortdesc", "longdesc", "descriptor",
        "shortdescription", "longdescription", "shortdescriptor", "longdescriptor",
        # HCPCS Level II status file.
        "code description",
    },
    "category": {"category", "cagc category", "adjustment category"},
    "is_active": {"is_active", "active", "is active", "status", "enabled"},
    "applicable_cagc": {"applicable_cagc", "cagc", "applicable cagc", "adjustment group"},
    "effective_date": {"effective_date", "effective", "effective date", "effectivedate"},
    "expiration_date": {
        "expiration_date", "expiration", "expiration date", "expired", "expirationdate",
        "termination_date", "termination", "termination date", "terminated", "terminationdate",
    },
}

# Positional layout when the file has no header row at all, per kind.
POSITIONAL = {
    "carc": ["code", "description", "category", "is_active",
             "effective_date", "expiration_date"],
    "rarc": ["code", "description", "applicable_cagc", "is_active",
             "effective_date", "expiration_date"],
    "icd10": ["code", "description", "is_active", "effective_date", "expiration_date"],
    "cpt": ["code", "description", "is_active", "effective_date", "expiration_date"],
    "modifier": ["code", "description", "is_active", "effective_date", "expiration_date"],
    "hcpcs": ["code", "description", "is_active", "effective_date", "expiration_date"],
}

# DB columns the planner may change on an existing row.
CHANGABLE = ("description", "category", "applicable_cagc",
             "effective_date", "expiration_date")


# ── Parsing helpers ──────────────────────────────────────────────


def _decode_bytes(raw: bytes) -> str:
    try:
        return raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        return raw.decode("cp1252")


def _parse_bool(value: Optional[str]) -> Optional[bool]:
    """True/False, or None when the cell does not say."""
    if value is None:
        return None
    s = str(value).strip().lower()
    if not s:
        return None
    # ICD-10-CM uses A/I in its Status column; CPT uses Added / In Effect /
    # Deleted in its Status column.
    if s in ("true", "t", "yes", "y", "1", "active", "a", "in effect", "effective", "added"):
        return True
    if s in ("false", "f", "no", "n", "0", "inactive", "i", "deactivated", "d", "disabled", "deleted"):
        return False
    return None


def _parse_date(value: Optional[str]) -> Optional[date]:
    """ISO (2025-11-01) or US-style (11/01/2025) dates; None when absent."""
    if value is None:
        return None
    s = str(value).strip()
    if not s:
        return None
    for fmt in ("%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%Y%m%d"):
        try:
            return datetime.strptime(s, fmt).date()
        except ValueError:
            continue
    raise ValueError(f"unrecognised date {value!r} (expected YYYY-MM-DD or MM/DD/YYYY)")


def _map_header(cells: list[str]) -> dict[str, int]:
    """field name -> column index for a header row."""
    mapped = {}
    for i, cell in enumerate(cells):
        name = cell.strip().lower()
        if not name:
            continue
        for field, aliases in ALIASES.items():
            if name in aliases and field not in mapped:
                mapped[field] = i
    return mapped


def _parse_file(raw: bytes, kind: str) -> tuple[list[dict], list[dict], int]:
    """(valid_rows, row_errors, rows_parsed) for one uploaded file.

    Each valid row is a dict with: code, description, and optional
    category / applicable_cagc / is_active / effective_date /
    expiration_date. Only fields the target table has are kept.
    """
    text = _decode_bytes(raw)
    rows = list(csv.reader(io.StringIO(text)))
    rows = [r for r in rows if any((c or "").strip() for c in r)]
    if not rows:
        return [], [], 0

    header = _map_header(rows[0])
    if "code" in header and "description" in header:
        data_rows, start_line = rows[1:], 2
    else:
        # No recognised header: the whole file is data, positionally.
        header = {name: i for i, name in enumerate(POSITIONAL[kind])}
        data_rows, start_line = rows, 1

    valid: list[dict] = []
    errors: list[dict] = []
    for offset, cells in enumerate(data_rows):
        line_no = start_line + offset

        def get(field: str) -> Optional[str]:
            if field not in header or header[field] >= len(cells):
                return None
            return cells[header[field]]

        code = (get("code") or "").strip()
        description = (get("description") or "").strip()
        if not code:
            errors.append({"row": line_no, "code": None, "reason": "missing code"})
            continue
        if len(code) > 20:
            errors.append({"row": line_no, "code": code, "reason": "code longer than 20 characters"})
            continue
        if not description:
            errors.append({"row": line_no, "code": code, "reason": "missing description"})
            continue
        try:
            active = _parse_bool(get("is_active"))
            eff = _parse_date(get("effective_date"))
            exp = _parse_date(get("expiration_date"))
        except ValueError as e:
            errors.append({"row": line_no, "code": code, "reason": str(e)})
            continue

        row = {"code": code, "description": description, "is_active": active,
               "effective_date": eff, "expiration_date": exp}
        extra = EXTRA_COLUMN.get(kind)
        if extra:
            row[extra] = (get(extra) or "").strip() or None
        valid.append(row)
    return valid, errors, len(data_rows)


# ── Change planning and execution ────────────────────────────────


def _plan(valid_rows: list[dict], existing: dict[str, dict]) -> dict[str, dict]:
    """Compare parsed rows with the current table.

    Maps code -> {action, row, current, changes}. `changes` lists the
    DB columns an UPDATE would touch; for deactivate/reactivate rows it
    also carries any field changes riding on the same row.
    """
    plan: dict[str, dict] = {}
    for row in valid_rows:
        code = row["code"]
        current = existing.get(code)
        if current is None:
            plan[code] = {"action": "add", "row": row, "current": None, "changes": []}
            continue

        changes = []
        for col in CHANGABLE:
            new = row.get(col)
            if new is None or new == "":
                continue
            if new != current.get(col):
                changes.append(col)

        if row["is_active"] is not None and row["is_active"] != bool(current.get("is_active", True)):
            action = "deactivate" if row["is_active"] is False else "reactivate"
        elif changes:
            action = "update"
        else:
            action = "unchanged"
        plan[code] = {"action": action, "row": row, "current": current, "changes": changes}
    return plan


async def _apply_plan(conn, kind: str, plan: dict[str, dict]) -> dict[str, int]:
    """Execute the plan on this connection (inside a transaction)."""
    table = TABLES[kind]
    counts = {"add": 0, "update": 0, "deactivate": 0, "reactivate": 0, "unchanged": 0}
    for code, item in plan.items():
        action, row = item["action"], item["row"]
        if action == "add":
            await _insert(conn, kind, row)
        elif action == "unchanged":
            pass
        else:
            sets, args = [], [code]
            if action in ("deactivate", "reactivate") and row["is_active"] is not None:
                sets.append(f"is_active = ${len(args) + 1}")
                args.append(row["is_active"])
            for col in item["changes"]:
                sets.append(f"{col} = ${len(args) + 1}")
                args.append(row[col])
            if sets:
                sets.append("updated_at = NOW()")
                await conn.execute(
                    f"UPDATE {table} SET {', '.join(sets)} WHERE code = $1", *args
                )
        counts[action] += 1
    return counts


async def _insert(conn, kind: str, row: dict) -> None:
    """Insert a new code. `extra` is the kind's extra column (if any)."""
    table = TABLES[kind]
    extra = EXTRA_COLUMN.get(kind)
    if extra:
        sql = f"""
            INSERT INTO {table} (code, description, {extra}, is_active,
                                 effective_date, expiration_date)
            VALUES ($1, $2, $3, COALESCE($4, TRUE),
                    COALESCE($5::date, CURRENT_DATE), $6::date)
            """
        args = [row["code"], row["description"], row[extra], row["is_active"],
                row["effective_date"], row["expiration_date"]]
    else:
        sql = f"""
            INSERT INTO {table} (code, description, is_active,
                                 effective_date, expiration_date)
            VALUES ($1, $2, COALESCE($3, TRUE),
                    COALESCE($4::date, CURRENT_DATE), $5::date)
            """
        args = [row["code"], row["description"], row["is_active"],
                row["effective_date"], row["expiration_date"]]
    await conn.execute(sql, *args)


# ── Endpoints ────────────────────────────────────────────────────


@router.get("/reference/summary", response_model=dict)
async def reference_summary():
    """Counts and last-import record for each reference list."""
    out = {}
    for kind, table in TABLES.items():
        async with get_connection() as conn:
            row = await conn.fetchrow(
                f"SELECT COUNT(*) AS total, COUNT(*) FILTER (WHERE is_active) AS active "
                f"FROM {table}"
            )
            last = await conn.fetchrow(
                """
                SELECT imported_at, imported_by, filename, rows_added, rows_updated,
                       rows_deactivated, rows_parsed
                  FROM reference_imports
                 WHERE kind = $1
                 ORDER BY imported_at DESC
                 LIMIT 1
                """,
                kind,
            )
        out[kind] = {
            "total": row["total"],
            "active": row["active"],
            "last_import": None if last is None else {
                "at": last["imported_at"],
                "by": last["imported_by"],
                "filename": last["filename"],
                "added": last["rows_added"],
                "updated": last["rows_updated"],
                "deactivated": last["rows_deactivated"],
            },
        }
    return out


@router.post("/reference/{kind}/import", response_model=dict)
async def reference_import(
    request: Request,
    kind: ReferenceKind,
    file: UploadFile = File(...),
    apply: bool = Form(False, description="false = dry run, nothing is written"),
):
    """Import a CARC/RARC/ICD-10/CPT/HCPCS/modifier list from CSV.

    Without `apply` this is a preview: the file is parsed and compared
    with the current list, and the response says exactly what an apply
    would add, update, or deactivate. With `apply=true` the same plan is
    executed in one transaction and logged to reference_imports.
    """
    cap = MAX_FILE_BYTES[kind]
    raw, total = [], 0
    while True:
        chunk = await file.read(1 << 20)
        if not chunk:
            break
        total += len(chunk)
        if total > cap:
            raise HTTPException(status_code=413,
                                 detail=f"File too large: maximum {cap // (1024 * 1024)} MB for this list")
        raw.append(chunk)
    raw = b"".join(raw)
    if not raw.strip():
        raise HTTPException(status_code=400, detail="File is empty")

    valid_rows, row_errors, rows_parsed = _parse_file(raw, kind)
    if not valid_rows:
        raise HTTPException(
            status_code=400,
            detail="No usable rows found - expected a CSV with at least 'code' and "
                   f"'description' columns. {len(row_errors)} row(s) rejected, see row_errors "
                   if row_errors else
                   "No usable rows found - expected a CSV with at least 'code' and 'description' columns.",
        )

    table = TABLES[kind]
    async with get_connection() as conn:
        existing_rows = await conn.fetch(f"SELECT * FROM {table}")
        existing = {r["code"]: dict(r) for r in existing_rows}
        plan = _plan(valid_rows, existing)

        file_codes = {row["code"] for row in valid_rows}
        codes_not_in_file = sum(1 for code in existing if code not in file_codes)

        if apply:
            async with conn.transaction():
                counts = await _apply_plan(conn, kind, plan)
                who = principal(request)
                await conn.execute(
                    """
                    INSERT INTO reference_imports
                        (kind, filename, rows_parsed, rows_added, rows_updated,
                         rows_deactivated, codes_not_in_file, row_errors, imported_by)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9)
                    """,
                    kind, file.filename, rows_parsed,
                    counts["add"],
                    counts["update"] + counts["deactivate"] + counts["reactivate"],
                    counts["deactivate"], codes_not_in_file,
                    json.dumps(row_errors), who.username,
                )
        else:
            counts = {a: sum(1 for p in plan.values() if p["action"] == a)
                      for a in ("add", "update", "deactivate", "reactivate", "unchanged")}

    sample = [
        {"code": item["row"]["code"], "description": item["row"]["description"],
         "action": item["action"]}
        for item in list(plan.values())[:10]
    ]

    return {
        "kind": kind,
        "mode": "applied" if apply else "dry-run",
        "filename": file.filename,
        "rows_parsed": rows_parsed,
        "valid_rows": len(valid_rows),
        "row_errors": row_errors,
        "changes": {
            "add": counts["add"],
            "update": counts["update"],
            "deactivate": counts["deactivate"],
            "reactivate": counts["reactivate"],
            "unchanged": counts["unchanged"],
        },
        "codes_not_in_file": codes_not_in_file,
        "sample": sample,
    }


class DeleteCodesBody(BaseModel):
    codes: list[str]


@router.get("/reference/{kind}/search", response_model=dict)
async def reference_search(
    kind: ReferenceKind,
    q: str = Query("", description="Matched against code and description"),
    page: int = Query(1, ge=1),
    page_size: int = Query(50, ge=1, le=200),
):
    """Search one reference list by code or description.

    `q` is matched as a substring, case-insensitively, against both the code
    and the description. Empty `q` simply browses the list. Results are
    paginated; `total` is the full match count across all pages.
    """
    table = TABLES[kind]
    needle = q.strip()
    # Escape LIKE metacharacters so a search for "50%" is not treated as a
    # wildcard by mistake.
    like = "%" + needle.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"

    async with get_connection() as conn:
        if needle:
            row = await conn.fetchrow(
                f"SELECT COUNT(*) AS n FROM {table} "
                f"WHERE code ILIKE $1 OR description ILIKE $1",
                like,
            )
            rows = await conn.fetch(
                f"""
                SELECT code, description, is_active, effective_date,
                       expiration_date, updated_at
                  FROM {table}
                 WHERE code ILIKE $1 OR description ILIKE $1
                 ORDER BY code
                 LIMIT $2 OFFSET $3
                """,
                like, page_size, (page - 1) * page_size,
            )
        else:
            row = await conn.fetchrow(f"SELECT COUNT(*) AS n FROM {table}")
            rows = await conn.fetch(
                f"""
                SELECT code, description, is_active, effective_date,
                       expiration_date, updated_at
                  FROM {table}
                 ORDER BY code
                 LIMIT $1 OFFSET $2
                """,
                page_size, (page - 1) * page_size,
            )
    return {
        "kind": kind,
        "q": needle,
        "total": row["n"],
        "page": page,
        "page_size": page_size,
        "pages": (row["n"] + page_size - 1) // page_size,
        "items": [
            {"code": r["code"], "description": r["description"],
             "is_active": r["is_active"], "effective_date": r["effective_date"],
             "expiration_date": r["expiration_date"], "updated_at": r["updated_at"]}
            for r in rows
        ],
    }


@router.post("/reference/{kind}/delete", response_model=dict)
async def reference_delete(request: Request, kind: ReferenceKind, body: DeleteCodesBody):
    """Delete specific codes from one reference list.

    Codes that are not in the list are reported, not an error - a delete is a
    reconciliation, and the response says exactly what happened. The audit
    middleware records who deleted what.
    """
    codes = sorted({c.strip() for c in body.codes if c and c.strip()})
    if not codes:
        raise HTTPException(status_code=400, detail="Provide at least one code to delete")
    if len(codes) > 10000:
        raise HTTPException(status_code=400,
                            detail="Too many codes in one request (maximum 10000)")
    table = TABLES[kind]
    async with get_connection() as conn:
        status = await conn.execute(
            f"DELETE FROM {table} WHERE code = ANY($1::varchar[])", codes
        )
    deleted = int(status.rsplit(" ", 1)[-1])
    logger.info(f"reference delete: kind={kind} requested={len(codes)} "
                f"deleted={deleted} by={principal(request).username}")
    return {"kind": kind, "requested": len(codes), "deleted": deleted,
            "not_found": len(codes) - deleted}


@router.post("/reference/{kind}/clear", response_model=dict)
async def reference_clear(request: Request, kind: ReferenceKind):
    """Delete EVERY code in one reference list.

    The list is empty afterwards, until the next import. Use it when a list
    was imported from a bad file and should be rebuilt from scratch rather
    than cleaned up row by row. The audit middleware records who cleared it.
    """
    table = TABLES[kind]
    async with get_connection() as conn:
        status = await conn.execute(f"DELETE FROM {table}")
    deleted = int(status.rsplit(" ", 1)[-1])
    logger.info(f"reference clear: kind={kind} deleted={deleted} "
                f"by={principal(request).username}")
    return {"kind": kind, "deleted": deleted}
