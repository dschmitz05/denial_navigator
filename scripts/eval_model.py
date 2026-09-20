#!/usr/bin/env python3
"""Compare reasoning models on checked-in synthetic denial cases.

Swapping the model behind an RCM system is not a quality question in the
abstract: what matters is whether it returns parseable JSON, whether its
category and its recommended action agree, and whether it gets patient
responsibility right — because that last one is the difference between billing
a patient and writing off money you were entitled to collect.

Deliberately READ-ONLY. It loads only checked-in synthetic cases, asks the rag-engine to build the same
prompt the application would, and calls llama.cpp directly — bypassing
/analyses/generate so nothing is stored. Running an evaluation must not litter
the database with analyses nobody asked for.

Usage, one model at a time:

    # with the current model loaded
    ./scripts/eval_model.sh run
    # swap the model on the llama.cpp host, then
    ./scripts/eval_model.sh run
    # then
    ./scripts/eval_model.sh compare eval/results/<a>.json eval/results/<b>.json
"""

import argparse
import asyncio
import json
import os
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# Imported inside run() rather than here: `compare` only reads two JSON files
# and should work on the host, where the container's dependencies are absent.

RAG_ENGINE_URL = os.environ.get("RAG_ENGINE_URL", "http://rag-engine:8000")
LLAMA_BASE_URL = os.environ.get("LLAMA_BASE_URL", "http://10.10.10.98:8080")
LLM_MAX_TOKENS = int(os.environ.get("LLM_MAX_TOKENS", "2048"))
DISABLE_THINKING = os.environ.get("LLM_DISABLE_THINKING", "true").lower() not in ("0", "false", "no")

VALID_ACTIONS = {
    "coding_correction", "clinical_documentation", "appeal",
    "bill_patient", "no_action_required",
}
VALID_CATEGORIES = {
    "coding_error", "missing_info", "lack_of_preauth", "medical_necessity",
    "bundled_service", "duplicate_claim", "timely_filing",
    "non_covered_service", "patient_responsibility", "other",
}
# Mirrors the recoverable categories in crates/denial-engine/src/lib.rs: a category
# naming something fixable cannot also mean "nothing to do".
RECOVERABLE = {
    "coding_error", "missing_info", "bundled_service",
    "lack_of_preauth", "medical_necessity", "patient_responsibility",
}


def fetch_cases(limit, denial_ids=None):
    """The denials to evaluate, with the context the prompt needs."""
    path = Path(__file__).with_name("fixtures") / "model_eval_cases.json"
    cases = json.loads(path.read_text())
    if denial_ids:
        wanted = set(denial_ids)
        cases = [case for case in cases if case["id"] in wanted]
    return cases[:limit]


async def build_prompt(client, case, policies):
    resp = await client.post(
        f"{RAG_ENGINE_URL}/prompt/denial-analysis",
        json={
            "claim_id": case["claim_number"],
            "payer_name": case["payer_name"] or "",
            "cpt_code": case["cpt_code"] or "",
            "icd10_code": (case["icd_10_codes"] or [""])[0] if case["icd_10_codes"] else "",
            "cagc": case["cagc"],
            "carc_code": case["carc_code"] or "",
            "carc_definition": case["carc_description"] or "Unknown",
            "rarc_code": case["rarc_code"] or "",
            "rarc_definition": case["rarc_description"] or "Unknown",
            "retrieved_policies": policies,
        },
        timeout=30,
    )
    resp.raise_for_status()
    return resp.json()


async def retrieve_policies(client, case):
    """Same retrieval the application does, so the comparison is like for like."""
    query = f"{case['payer_name']} {case['cpt_code']} {case['carc_code']}"
    try:
        resp = await client.post(
            f"{RAG_ENGINE_URL}/search",
            json={"query": query, "top_k": 5, "filters": {}},
            timeout=60,
        )
        resp.raise_for_status()
        return [r.get("content", "") for r in resp.json().get("results", [])]
    except Exception:
        return []


def score(parsed, case):
    """What actually matters about an answer, per denial."""
    checks = {}
    checks["json_valid"] = isinstance(parsed, dict) and not parsed.get("parse_error")
    if not checks["json_valid"]:
        return checks, "unparseable"

    action = parsed.get("required_action")
    category = parsed.get("denial_category")

    checks["action_in_enum"] = action in VALID_ACTIONS
    checks["category_in_enum"] = category in VALID_CATEGORIES
    checks["has_explanation"] = bool((parsed.get("explanation") or "").strip())
    steps = parsed.get("steps")
    checks["has_steps"] = isinstance(steps, list) and len(steps) > 0

    # The contradiction that prompted this work: a fixable category paired
    # with "nothing to do".
    checks["category_action_agree"] = not (
        action == "no_action_required" and category in RECOVERABLE
    )

    # A PR adjustment is collectible from the patient. Calling it a write-off
    # is the single most expensive mistake this model can make.
    if case["cagc"] == "PR":
        checks["pr_not_written_off"] = action != "no_action_required"

    # needs_appeal has to follow from required_action, or the queue routes on
    # a flag that disagrees with the recommendation it came with.
    needs_appeal = parsed.get("needs_appeal")
    if isinstance(needs_appeal, bool):
        checks["appeal_flag_consistent"] = needs_appeal == (action == "appeal")
        if needs_appeal:
            checks["letter_when_appealing"] = bool((parsed.get("draft_appeal_letter") or "").strip())

    return checks, action


async def run(args):
    import httpx

    cases = fetch_cases(args.limit, args.denial_ids)

    if not cases:
        print("No denials to evaluate. Ingest a file first.", file=sys.stderr)
        return 1

    async with httpx.AsyncClient(timeout=300) as client:
        models = (await client.get(f"{LLAMA_BASE_URL}/v1/models")).json()
        loaded = [m["id"] for m in models.get("data", [])]
        model_name = args.model or (loaded[0] if loaded else "unknown")
        print(f"Model loaded on {LLAMA_BASE_URL}: {model_name}")
        print(f"Evaluating {len(cases)} denial(s)…\n")

        results = []
        for i, case in enumerate(cases, 1):
            policies = await retrieve_policies(client, case)
            prompt = await build_prompt(client, case, policies)

            payload = {
                "model": model_name,
                "messages": [
                    {"role": "system", "content": prompt["system"]},
                    {"role": "user", "content": prompt["user"]},
                ],
                "stream": False,
                "temperature": args.temperature,
                "max_tokens": LLM_MAX_TOKENS,
            }
            if DISABLE_THINKING:
                payload["chat_template_kwargs"] = {"enable_thinking": False}

            started = time.monotonic()
            try:
                resp = await client.post(f"{LLAMA_BASE_URL}/v1/chat/completions", json=payload)
                resp.raise_for_status()
                data = resp.json()
                raw = data["choices"][0]["message"].get("content") or ""
                if not raw.strip():
                    raw = data["choices"][0]["message"].get("reasoning_content") or ""
                usage = data.get("usage", {})
                finish = data["choices"][0].get("finish_reason")
                error = None
            except Exception as e:
                raw, usage, finish, error = "", {}, None, f"{type(e).__name__}: {e}"
            elapsed = time.monotonic() - started

            cleaned = raw.strip()
            if cleaned.startswith("```json"):
                cleaned = cleaned[7:]
            if cleaned.startswith("```"):
                cleaned = cleaned[3:]
            if cleaned.endswith("```"):
                cleaned = cleaned[:-3]
            try:
                parsed = json.loads(cleaned.strip())
            except (json.JSONDecodeError, ValueError):
                parsed = {"parse_error": True}

            checks, action = score(parsed, case)
            passed = sum(1 for v in checks.values() if v)
            print(f"  [{i}/{len(cases)}] {case['claim_number']:10} "
                  f"{case['cagc']}/{case['carc_code']:<4} "
                  f"{elapsed:6.1f}s  {passed}/{len(checks)} checks  {action}"
                  + (f"  ERROR {error}" if error else "")
                  + ("  TRUNCATED" if finish == "length" else ""))

            results.append({
                "denial_id": str(case["id"]),
                "claim_number": case["claim_number"],
                "cagc": case["cagc"], "carc_code": case["carc_code"],
                "seconds": round(elapsed, 2),
                "finish_reason": finish,
                "error": error,
                "checks": checks,
                "required_action": parsed.get("required_action"),
                "denial_category": parsed.get("denial_category"),
                "prompt_tokens": usage.get("prompt_tokens"),
                "completion_tokens": usage.get("completion_tokens"),
                "policies_retrieved": len(policies),
            })

    out = {
        "model": model_name,
        "run_at": datetime.now(timezone.utc).isoformat(),
        "temperature": args.temperature,
        "max_tokens": LLM_MAX_TOKENS,
        "disable_thinking": DISABLE_THINKING,
        # Recorded so a later run can be told to use exactly the same denials.
        "denial_ids": [r["denial_id"] for r in results],
        "results": results,
        "summary": summarise(results),
    }
    path = Path(args.out or f"{args.out_dir}/{model_name.replace('/', '_')}.json")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, indent=2))
    print(f"\nWrote {path}")
    print_summary(out["summary"], model_name)
    return 0


def summarise(results):
    every_check = sorted({k for r in results for k in r["checks"]})
    summary = {"cases": len(results)}
    for check in every_check:
        applicable = [r for r in results if check in r["checks"]]
        passed = [r for r in applicable if r["checks"][check]]
        summary[check] = {
            "passed": len(passed), "of": len(applicable),
            "rate": round(len(passed) / len(applicable), 3) if applicable else None,
        }
    times = [r["seconds"] for r in results if not r["error"]]
    summary["seconds"] = {
        "median": round(statistics.median(times), 1) if times else None,
        "total": round(sum(times), 1) if times else None,
    }
    summary["errors"] = sum(1 for r in results if r["error"])
    summary["truncated"] = sum(1 for r in results if r["finish_reason"] == "length")
    completions = [r["completion_tokens"] for r in results if r.get("completion_tokens")]
    summary["median_completion_tokens"] = (
        int(statistics.median(completions)) if completions else None
    )
    return summary


def print_summary(s, model_name):
    print(f"\n{model_name} — {s['cases']} denial(s)")
    print("-" * 62)
    for key, val in s.items():
        if isinstance(val, dict) and "rate" in val:
            rate = "—" if val["rate"] is None else f"{val['rate'] * 100:5.1f}%"
            print(f"  {key:26} {rate}  ({val['passed']}/{val['of']})")
    print(f"  {'median seconds':26} {s['seconds']['median']}")
    print(f"  {'median output tokens':26} {s['median_completion_tokens']}")
    print(f"  {'errors / truncated':26} {s['errors']} / {s['truncated']}")


def compare(args):
    a, b = json.loads(Path(args.a).read_text()), json.loads(Path(args.b).read_text())
    if set(a.get("denial_ids", [])) != set(b.get("denial_ids", [])):
        print("WARNING: the two runs used different denials, so this is not a "
              "like-for-like comparison.\n", file=sys.stderr)

    print(f"{'':30} {a['model'][:14]:>14} {b['model'][:14]:>14}")
    print("-" * 62)
    keys = [k for k in a["summary"] if isinstance(a["summary"][k], dict) and "rate" in a["summary"][k]]
    for k in keys:
        ra, rb = a["summary"][k]["rate"], b["summary"].get(k, {}).get("rate")
        fa = "—" if ra is None else f"{ra*100:.0f}%"
        fb = "—" if rb is None else f"{rb*100:.0f}%"
        arrow = ""
        if ra is not None and rb is not None:
            arrow = "  better" if rb > ra else ("  worse" if rb < ra else "")
        print(f"  {k:28} {fa:>14} {fb:>14}{arrow}")
    print(f"  {'median seconds':28} {a['summary']['seconds']['median']:>14} "
          f"{b['summary']['seconds']['median']:>14}")
    print(f"  {'errors':28} {a['summary']['errors']:>14} {b['summary']['errors']:>14}")
    print(f"  {'truncated':28} {a['summary']['truncated']:>14} {b['summary']['truncated']:>14}")

    # Where they actually disagree, which is more informative than the rates.
    by_id = {r["denial_id"]: r for r in b["results"]}
    diffs = [(r, by_id[r["denial_id"]]) for r in a["results"]
             if r["denial_id"] in by_id
             and r["required_action"] != by_id[r["denial_id"]]["required_action"]]
    if diffs:
        print(f"\nDisagreements on {len(diffs)} denial(s):")
        for ra, rb in diffs:
            print(f"  {ra['claim_number']:10} {ra['cagc']}/{ra['carc_code']:<4} "
                  f"{str(ra['required_action']):22} -> {rb['required_action']}")
    return 0


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="evaluate the model currently loaded")
    r.add_argument("--limit", type=int, default=20)
    r.add_argument("--out")
    r.add_argument("--out-dir", default="eval/results")
    r.add_argument("--model", help="override the name sent to llama.cpp")
    r.add_argument("--temperature", type=float, default=0.3)
    r.add_argument("--denial-ids", nargs="*", dest="denial_ids",
                   help="evaluate exactly these, to repeat an earlier run")

    c = sub.add_parser("compare", help="compare two result files")
    c.add_argument("a")
    c.add_argument("b")
    c.add_argument("--out-dir", default="eval/results", help=argparse.SUPPRESS)

    args = p.parse_args()
    if args.cmd == "run":
        sys.exit(asyncio.run(run(args)))
    sys.exit(compare(args))


if __name__ == "__main__":
    main()
