#!/usr/bin/env python3
"""Split a CMS bulk LCD (Local Coverage Determination) export into one
knowledge-base document per policy.

CMS's MCD bulk export is one CSV with a row per LCD - title, indication,
coding guidelines, documentation requirements, bibliography, all as
HTML-formatted text. The knowledge-base upload endpoint
(POST /api/v1/knowledge/documents/upload) treats whatever file it's given as
ONE document to chunk and embed for semantic search, so uploading the raw
export as-is would mix every LCD nationwide into a single incoherent
document - not a size problem, a category-of-data problem.

This reads the export with a real CSV parser (a naive `wc -l` overcounts
because several fields hold embedded newlines inside quoted HTML), strips
markup from the rich-text fields, and writes one plain-text file per LCD,
named by its determination number. Only 'A' (active) LCDs are included by
default - 'P' (proposed) rows are not yet in effect, and surfacing them as
current guidance would be actively wrong for a billing decision.

Usage:
    python3 scripts/split_lcd_csv.py lcd.csv --out-dir lcd_documents
    python3 scripts/split_lcd_csv.py lcd.csv --out-dir lcd_documents \\
        --keyword "injection" --keyword "wheelchair"
    python3 scripts/split_lcd_csv.py lcd.csv --out-dir lcd_documents \\
        --ids L33252,L34563
    python3 scripts/split_lcd_csv.py lcd.csv --out-dir lcd_documents \\
        --status A,P --limit 20   # smoke-test a handful first

Upload the results with scripts/upload_lcd_documents.sh.
"""

from __future__ import annotations

import argparse
import csv
import html
import re
import sys
from pathlib import Path

# Rich-text fields worth keeping in the document body, in reading order.
# Left out deliberately: internal review/administrative fields (adv_meeting,
# comment_start_dt, draft_contact, revenue_para, issue_change, ...) that are
# CMS process metadata, not coverage guidance a denial appeal would cite.
BODY_FIELDS = [
    ("Indication", "indication"),
    ("Diagnoses that support coverage", "diagnoses_support"),
    ("Diagnoses that do not support coverage", "diagnoses_dont_support"),
    ("Coding guidelines", "coding_guidelines"),
    ("Documentation requirements", "doc_reqs"),
    ("Utilization guide", "util_guide"),
    ("Summary of evidence", "summary_of_evidence"),
    ("Analysis of evidence", "analysis_of_evidence"),
    ("Bibliography", "bibliography"),
]

TAG_RE = re.compile(r"<[^>]+>")
WHITESPACE_RE = re.compile(r"[ \t]+")
BLANK_LINES_RE = re.compile(r"\n{3,}")
SLUG_RE = re.compile(r"[^a-z0-9]+")


def strip_html(value: str) -> str:
    text = TAG_RE.sub(" ", value)
    text = html.unescape(text)
    text = WHITESPACE_RE.sub(" ", text)
    lines = [line.strip() for line in text.splitlines()]
    text = "\n".join(line for line in lines if line)
    return BLANK_LINES_RE.sub("\n\n", text).strip()


def slugify(title: str, max_len: int = 60) -> str:
    slug = SLUG_RE.sub("-", title.lower()).strip("-")
    return slug[:max_len].rstrip("-") or "untitled"


def build_document(row: dict[str, str]) -> str:
    # Some titles carry markup too, e.g. "Vitamin B<sub>12</sub> Injections".
    title_raw = row.get("title", "").strip()
    parts = [strip_html(title_raw) if title_raw else "(untitled LCD)"]
    meta_bits = []
    if row.get("determination_number"):
        meta_bits.append(f"Determination number: {row['determination_number']}")
    if row.get("orig_det_eff_date"):
        meta_bits.append(f"Effective: {row['orig_det_eff_date']}")
    if row.get("rev_eff_date"):
        meta_bits.append(f"Last revised: {row['rev_eff_date']}")
    if meta_bits:
        parts.append(" | ".join(meta_bits))

    for heading, field in BODY_FIELDS:
        raw = row.get(field, "").strip()
        if not raw:
            continue
        cleaned = strip_html(raw)
        if cleaned:
            parts.append(f"## {heading}\n\n{cleaned}")

    return "\n\n".join(parts) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("csv_path", type=Path, help="Path to the CMS LCD bulk export CSV")
    parser.add_argument("--out-dir", type=Path, required=True, help="Directory to write one .txt file per LCD into")
    parser.add_argument(
        "--status", default="A", help="Comma-separated status codes to include (default: A, active only)"
    )
    parser.add_argument(
        "--keyword",
        action="append",
        default=[],
        help="Only include LCDs whose title contains this (case-insensitive). Repeatable; OR'd together.",
    )
    parser.add_argument(
        "--ids",
        default="",
        help="Comma-separated determination_number or lcd_id values to include, ignoring --status/--keyword",
    )
    parser.add_argument("--limit", type=int, default=None, help="Stop after writing this many documents")
    args = parser.parse_args()

    csv.field_size_limit(sys.maxsize)  # rich-text fields exceed Python's 128 KB default

    wanted_ids = {v.strip() for v in args.ids.split(",") if v.strip()}
    wanted_status = {v.strip() for v in args.status.split(",") if v.strip()}
    keywords = [k.lower() for k in args.keyword]

    args.out_dir.mkdir(parents=True, exist_ok=True)

    written = 0
    skipped = 0
    with args.csv_path.open(newline="", encoding="utf-8-sig") as f:
        reader = csv.DictReader(f)
        for row in reader:
            if args.limit is not None and written >= args.limit:
                break

            if wanted_ids:
                if row.get("determination_number") not in wanted_ids and row.get("lcd_id") not in wanted_ids:
                    skipped += 1
                    continue
            else:
                if row.get("status") not in wanted_status:
                    skipped += 1
                    continue
                title_lower = row.get("title", "").lower()
                if keywords and not any(k in title_lower for k in keywords):
                    skipped += 1
                    continue

            title = row.get("title", "").strip()
            det_num = row.get("determination_number", "").strip() or row.get("lcd_id", "").strip() or "unknown"
            filename = f"lcd_{det_num}_{slugify(title)}.txt"
            (args.out_dir / filename).write_text(build_document(row), encoding="utf-8")
            written += 1

    print(f"Wrote {written} document(s) to {args.out_dir}/ ({skipped} row(s) skipped by filter)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
