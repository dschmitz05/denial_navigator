#!/usr/bin/env python3
"""Retrieval evaluation for the knowledge base (FB-12).

Runs the labelled queries in scripts/fixtures/retrieval_eval.json against the
chunks stored in a running stack, the way the RAG engine searches them: the
query is embedded by the same embedding server with the model's query prefix,
scored by cosine similarity in pgvector, and scoped to the query's payer plus
payer-agnostic documents. Documents must have been loaded with
scripts/seed_test_knowledge.sh.

Reports top-1 accuracy, MRR, and the similarity of the correct document
against the best wrong one, then applies the engine's cut (absolute
MIN_SIMILARITY and RELATIVE_CUT below the top result) and reports what an
analysis would receive. With --check it exits non-zero when the configured cut
misses the targets, which is how CI uses it. With --sweep it searches for the
cut that keeps the most correct evidence while returning nothing for the
queries no document answers.

Needs only the Python standard library, docker, and network access to the
embedding server.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
DEV_ORG = "00000000-0000-0000-0000-000000000001"
TOP_K = 5
# Defaults the RAG engine ships with; keep in step with crates/rag-engine.
DEFAULT_MIN_SIMILARITY = 0.62
DEFAULT_RELATIVE_CUT = 0.04


@dataclass
class Chunk:
    title: str
    similarity: float
    keyword: bool
    keyword_score: float
    content: str
    anchored: bool = True


def embed(base_url: str, model: str, prefix: str, text: str) -> list[float]:
    body = json.dumps({"model": model, "input": [prefix + text]}).encode()
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/v1/embeddings",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)["data"][0]["embedding"]


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def score_chunks(args: argparse.Namespace, payer: str, query: str, vector: list[float]) -> list[Chunk]:
    """Every in-scope chunk with its cosine similarity and keyword match."""
    vec = sql_literal("[" + ",".join(f"{x:.7f}" for x in vector) + "]")
    q = sql_literal(query)
    sql = f"""
        SELECT json_agg(r) FROM (
          SELECT kd.title, kc.content,
                 1 - (kc.embedding <=> {vec}::vector) AS similarity,
                 to_tsvector('english', kc.content) @@ websearch_to_tsquery('english', {q}) AS keyword,
                 LEAST(ts_rank_cd(to_tsvector('english', kc.content), websearch_to_tsquery('english', {q})), 1.0) AS keyword_score
          FROM knowledge_chunks kc JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id
          WHERE kc.embedding IS NOT NULL AND kd.status <> 'archived'
            AND kd.organization_id = {sql_literal(args.organization)}
            AND (kd.payer_name IS NULL OR lower(kd.payer_name) = lower({sql_literal(payer)}))
        ) r
    """
    out = subprocess.run(
        ["docker", "exec", "-i", args.db_container, "psql", "-q", "-v", "ON_ERROR_STOP=1",
         "-U", args.db_user, "-d", args.db_name, "-At"],
        input=sql, capture_output=True, text=True, check=True,
    ).stdout.strip()
    rows = json.loads(out) if out else []
    return [Chunk(r["title"], r["similarity"], r["keyword"], r["keyword_score"], r["content"]) for r in rows or []]


# Mirrors rag-engine: a query term carried by more than this share of the
# scoped chunks says nothing about which document answers the query.
ANCHOR_MAX_SHARE = 0.25
STOP_WORDS = set("""the and for was were are with without when that this these those not any all each per its
from such shall may must than then also other more most least less same only into under over about which what how
have has can after before claim claims service services provider member patient payer plan denied denial code date
cpt icd icd-10 icd10 carc rarc hcpcs""".split())


def query_tokens(query: str) -> list[str]:
    out: list[str] = []
    for raw in re.split(r"[^A-Za-z0-9.\-]+", query):
        token = raw.strip(".-").lower()
        if len(token) >= 3 and token not in STOP_WORDS and token not in out:
            out.append(token)
    return out


def word_in(token: str, text: str) -> bool:
    return re.search(r"(?<![a-z0-9])" + re.escape(token) + r"(?![a-z0-9])", text.lower()) is not None


def anchor_tokens(query: str) -> list[str]:
    """Codes when the query has any, else its content words (see rag-engine)."""
    tokens = query_tokens(query)
    codes = [t for t in tokens if any(c.isdigit() for c in t)]
    return codes or tokens


def mark_anchored(query: str, chunks: list[Chunk]) -> None:
    """Flags each chunk the way the engine's `anchors` CTE does."""
    anchors = [
        t for t in anchor_tokens(query)
        if sum(word_in(t, c.content) for c in chunks) <= -(-ANCHOR_MAX_SHARE * len(chunks) // 1)
    ]
    for c in chunks:
        c.anchored = not anchors or any(word_in(a, c.content) for a in anchors)


def retrieve(chunks: list[Chunk], min_similarity: float, relative_cut: float) -> list[str]:
    """Document titles the engine returns, best first (mirrors search_similar)."""
    passing = [c for c in chunks if c.anchored and c.similarity >= min_similarity]
    if not passing:
        return []
    top = max(c.similarity for c in passing)
    kept = [c for c in passing if c.similarity >= top - relative_cut]
    kept.sort(key=lambda c: 0.7 * c.similarity + 0.3 * c.keyword_score, reverse=True)
    titles: list[str] = []
    for c in kept[:TOP_K]:
        if c.title not in titles:
            titles.append(c.title)
    return titles


def best_by_document(chunks: list[Chunk]) -> list[tuple[str, float]]:
    best: dict[str, float] = {}
    for c in chunks:
        best[c.title] = max(best.get(c.title, -1.0), c.similarity)
    return sorted(best.items(), key=lambda kv: kv[1], reverse=True)


@dataclass
class Outcome:
    top1: float
    mrr: float
    recall: float
    precision: float
    negatives_empty: float
    positives_empty: int


def evaluate(cases, min_similarity: float, relative_cut: float) -> Outcome:
    positives = [c for c in cases if c["relevant"]]
    negatives = [c for c in cases if not c["relevant"]]
    top1 = rr = recall = 0.0
    precisions = []
    empty = 0
    for case in positives:
        got = retrieve(case["chunks"], min_similarity, relative_cut)
        relevant = set(case["relevant"])
        if not got:
            empty += 1
            continue
        top1 += got[0] in relevant
        rank = next((i for i, t in enumerate(got, 1) if t in relevant), None)
        rr += 1 / rank if rank else 0
        recall += rank is not None
        precisions.append(sum(t in relevant for t in got) / len(got))
    neg_empty = sum(not retrieve(c["chunks"], min_similarity, relative_cut) for c in negatives)
    n = len(positives)
    return Outcome(
        top1=top1 / n,
        mrr=rr / n,
        recall=recall / n,
        precision=sum(precisions) / len(precisions) if precisions else 0.0,
        negatives_empty=neg_empty / len(negatives) if negatives else 1.0,
        positives_empty=empty,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cases", default=str(ROOT / "fixtures" / "retrieval_eval.json"))
    parser.add_argument("--embed-url", default=os.environ.get("EMBED_BASE_URL", "http://localhost:8081"))
    parser.add_argument("--model", default=os.environ.get("EMBEDDING_MODEL", "nomic-embed-text"))
    parser.add_argument("--query-prefix", default=os.environ.get("EMBED_QUERY_PREFIX") or "search_query: ")
    parser.add_argument("--db-container", default=os.environ.get("DB_CONTAINER", "denialnav-rust-postgres"))
    parser.add_argument("--db-user", default=os.environ.get("POSTGRES_USER", "denial_nav"))
    parser.add_argument("--db-name", default=os.environ.get("POSTGRES_DB", "denial_navigator"))
    parser.add_argument("--organization", default=DEV_ORG)
    parser.add_argument("--min-similarity", type=float, default=float(os.environ.get("MIN_SIMILARITY", DEFAULT_MIN_SIMILARITY)))
    parser.add_argument("--relative-cut", type=float, default=float(os.environ.get("RELATIVE_CUT", DEFAULT_RELATIVE_CUT)))
    parser.add_argument("--sweep", action="store_true", help="search for the best cut")
    parser.add_argument("--check", action="store_true", help="fail when the cut misses the targets")
    parser.add_argument("--min-top1", type=float, default=0.8)
    parser.add_argument("--min-recall", type=float, default=0.8)
    # Not 1.0: a query for a service no policy covers still names a procedure
    # code the payer's fee schedule lists, and that document is a real match
    # for the code even though it does not answer the denial.
    parser.add_argument("--min-unanswerable-empty", type=float, default=0.75)
    args = parser.parse_args()
    prefix = "" if args.query_prefix == "none" else args.query_prefix

    cases = json.loads(Path(args.cases).read_text())["queries"]
    for case in cases:
        vector = embed(args.embed_url, args.model, prefix, case["query"])
        case["chunks"] = score_chunks(args, case["payer"], case["query"], vector)
        mark_anchored(case["query"], case["chunks"])
        if not case["chunks"]:
            print(f"No chunks in scope for {case['query']!r}; seed the knowledge base first.", file=sys.stderr)
            return 2

    print("Raw similarity (best chunk per document, no cut)")
    print(f"{'query':<58} {'rank':>4} {'correct':>8} {'best wrong':>10}  keyword")
    ranks, correct_scores, wrong_scores, negative_scores = [], [], [], []
    for case in cases:
        ranked = best_by_document(case["chunks"])
        relevant = set(case["relevant"])
        wrong = next((s for t, s in ranked if t not in relevant), 0.0)
        keyword = sorted({c.title[7:40] for c in case["chunks"] if c.keyword})
        label = case["query"][:58]
        if relevant:
            rank = next(i for i, (t, _) in enumerate(ranked, 1) if t in relevant)
            right = next(s for t, s in ranked if t in relevant)
            ranks.append(rank)
            correct_scores.append(right)
            wrong_scores.append(wrong)
            print(f"{label:<58} {rank:>4} {right:>8.3f} {wrong:>10.3f}  {', '.join(keyword)}")
        else:
            negative_scores.append(wrong)
            print(f"{label:<58} {'-':>4} {'-':>8} {wrong:>10.3f}  {', '.join(keyword)}")
    print(
        f"\ntop-1 {sum(r == 1 for r in ranks) / len(ranks):.2f}  MRR {sum(1 / r for r in ranks) / len(ranks):.2f}"
        f"  correct {min(correct_scores):.3f}-{max(correct_scores):.3f}"
        f"  best wrong {min(wrong_scores):.3f}-{max(wrong_scores):.3f}"
        f"  unanswerable best {min(negative_scores):.3f}-{max(negative_scores):.3f}"
    )

    if args.sweep:
        results = []
        for t100 in range(40, 86):
            for r100 in [*range(2, 21), 100]:
                t, r = t100 / 100, r100 / 100
                o = evaluate(cases, t, r)
                results.append((o.negatives_empty, o.recall, o.precision, o.top1, t, r, o))
        results.sort(key=lambda x: (x[0], x[1], x[2], x[3], -x[4]), reverse=True)
        print("\nBest cuts (unanswerable empty, recall, precision, top-1)")
        for *_, t, r, o in results[:10]:
            print(f"  MIN_SIMILARITY={t:.2f} RELATIVE_CUT={r:.2f}  unanswerable empty {o.negatives_empty:.2f}"
                  f"  recall {o.recall:.2f}  precision {o.precision:.2f}  top-1 {o.top1:.2f}")

    o = evaluate(cases, args.min_similarity, args.relative_cut)
    print(
        f"\nWith MIN_SIMILARITY={args.min_similarity:.2f} RELATIVE_CUT={args.relative_cut:.2f}:"
        f" top-1 {o.top1:.2f}  MRR {o.mrr:.2f}  recall {o.recall:.2f}  precision {o.precision:.2f}"
        f"  unanswerable empty {o.negatives_empty:.2f}  answerable left empty {o.positives_empty}"
    )
    if args.check:
        failures = []
        if o.negatives_empty < args.min_unanswerable_empty:
            failures.append(
                f"queries no document answers returned evidence ({o.negatives_empty:.2f} empty"
                f" < {args.min_unanswerable_empty})"
            )
        if o.top1 < args.min_top1:
            failures.append(f"top-1 {o.top1:.2f} < {args.min_top1}")
        if o.recall < args.min_recall:
            failures.append(f"recall {o.recall:.2f} < {args.min_recall}")
        if failures:
            print("FAIL: " + "; ".join(failures), file=sys.stderr)
            return 1
        print("Retrieval evaluation passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
