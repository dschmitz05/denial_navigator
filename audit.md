# OpenClaim Navigator — Plan Compliance Audit

**Audited against:** `plan.md` (treated as source of truth)
**Repo state:** HEAD `13d8591`, working tree dirty (8 modified files, untracked migrations `028`/`029`)
**Method:** Four independent read-only code audits (M0–2, M3–6, M7–10, security/PHI/hygiene) with spot-verification of the highest-severity claims. Evidence cited as `file:line`.

---

## 1. Executive Summary

The implementation is **substantially further along than a typical mid-project state** — most milestones are genuinely built and functional (EDI parsing, denial queue, knowledge base + hybrid retrieval, playbooks, feedback loop, analytics, ingestion sources, hardened deployment). No real PHI is tracked in git, and the core security controls (RBAC, org scoping, rate limiting, security headers, PHI-bounded logging) are real, not cosmetic.

However, the plan's checkboxes **overstate completion in several places**. The most important findings, in order of severity:

### 🔴 Critical — must fix before any real PHI touches this system

1. **Cross-tenant PHI leak via audit labels.** The audit middleware's `label_query()` lookups (`crates/audit/src/lib.rs:137-159`) fetch `claim_number` + `patient_name` from other tables **without an `organization_id` filter**. The *main* request handler is org-scoped, but a requester probing another tenant's claim UUID gets a 404 *and* the other org's `claim_number`/`patient_name` written into their own audit row. This is a real broken-object-authorization path and directly violates plan §0.6, §8.10, §16.1 (IDOR), and §16.5 ("never log patient name").
2. **Audit metadata persists PHI.** `patient_name` (and the raw request `query` string) are stored in `audit_log.details` (`crates/audit/src/lib.rs:123-135, ~355`). Plan §8.10: "Never place PHI or raw claim payloads in audit metadata." These rows are retained ~6 years.
3. **Claim correlation silently merges — the exact behavior the plan forbids.** 837→835 correlation is a plain `ON CONFLICT (organization_id, claim_number) DO UPDATE` upsert that **hardcodes `correlation_confidence = 1.00`** (`crates/api-gateway/src/routes/ingestion.rs:373-392`). There is no scored matcher, none of the §9.4 candidate fields are used, no matching reasons are stored, and no ambiguous-match confirmation exists. The `ambiguous` status and `correlation_confidence` column are dead schema. This violates §9.4 ("Never silently merge ambiguous claims") and fails Milestone 3's exit criteria by construction.

### 🟠 High — material gaps vs. the plan

4. **Full AI prompts/responses are persisted by default** (`ai_analyses.raw_prompt` / `raw_response`, `database/init.sql:130-131`, populated unconditionally at `crates/api-gateway/src/routes/analyses.rs:245-246`). Plan §17.3 says *avoid storing full prompts by default if they may contain PHI*. At the `full` disclosure level, PHI is written to disk. No retention/prune exists for `ai_analyses` (§16.4).
5. **No provider abstractions.** `RecommendationProvider` and `AiProvider` traits do not exist (Milestone 5, plan §5.5/§12). Only `EmbeddingProvider` exists (`crates/ai/src/embed.rs:13`). The chat AI path is a monolithic `LlamaClient` (`crates/llm-service/src/llama.rs`).
6. **PHI disclosure levels diverge.** Plan §12.2: `none` / `deidentified` / `limited_phi` / `full_context`, **default `deidentified`**. Code: `none` / `limited` / `full` (`crates/ai/src/prompt.rs:6-10`), **default `limited`**, env-var only (not admin-configurable). Redaction covers only the claim reference, not a field-level mapper.
7. **No user-facing AI-safety disclosures** (plan §17.1). No "advisory only / validate before acting / AI can be wrong" messaging anywhere in the UI.
8. **Observability is a stub.** No OpenTelemetry, no metrics of any kind (plan §20). Only structured logs + `/health/live` + `/health/ready`.
9. **Role set does not match plan §2.2.** DB allows only 5 roles (`billing_specialist, billing_manager, rcm_director, admin, auditor`, `database/init.sql:353`). The plan's `system_admin`, `security_admin`, `revenue_cycle_manager`, `coding_specialist`, `read_only` are absent.
10. **CI's web smoke test is broken.** `npm run smoke` bundles `apps/web/scripts/smoke-render.mjs`, which imports `../src/pages/*.jsx` (e.g. `Dashboard.jsx`), but every page is now `.tsx`. esbuild will fail to resolve — the CI `npm run smoke` step (`.github/workflows/ci.yml:38`) cannot pass as written.
11. **No Playwright E2E** anywhere (plan §21.2, §21.4, and Slice A's final item all require it). The §21.4 critical happy-path scenario is only covered by a shell script (`scripts/test_synthetic_e2e.sh`) that hits a running stack, not an automated browser E2E.

### 🟡 Medium — divergences and missing polish

- **Schema diverges from plan §8.** No `import_batches`, `source_files`, `service_lines`, `adjustments`, `remarks`, `payers`, `denial_cases`, `recommendations`, or `denial_cases` tables. Service lines are embedded in `claims.raw_835_data JSONB` (`database/init.sql:58`); adjustments live inside the `denials` row. Plan §8.4/8.5 explicitly wanted normalized tables.
- **Workflow state machine diverges from plan §30** and transitions are **not validated in app code** — `PATCH /denials/{id}` accepts any valid CHECK-constraint status from any state (`crates/api-gateway/src/routes/denials.rs:702-749`).
- **QTY segments are unhandled** in the 835 parser (Milestone 1 explicitly lists QTY; grep for `QTY` in `crates/` = 0 hits).
- **Root-cause taxonomy** is a hardcoded 10-value CHECK on `ai_analyses.denial_category` (`database/init.sql:140-143`), not a seeded/configurable/org-customizable taxonomy attached to denial cases (plan §10.2).
- **Recommendation jobs are in-process `tokio::spawn` tasks** (`crates/api-gateway/src/routes/analyses.rs:514-586`), not a durable Postgres-backed queue (§18). A gateway restart loses `pending` jobs; failures are terminal (no retry/dead-letter).
- **Threat model is a summary, not the §16.1 list.** `docs/threat-model.md` (24 lines) covers controls + logging boundary only; most enumerated threats (IDOR, SQLi/XSS/CSRF, SSRF, prompt injection, malicious admin, supply chain, object-store/backup/export exposure) are not individually addressed.
- **API hygiene (§13.2):** no request IDs, no machine-readable error `code` field, no `Idempotency-Key` header, and the `version` column is incremented but never used for optimistic-concurrency conflict detection.
- **Evaluation harness is not offline/synthetic.** `scripts/eval_model.py` reads **live DB denials**, not synthetic offline cases (§17.4), and lacks citation precision/coverage and hallucination-rate metrics.
- **Docs contradiction.** `docs/RELEASE_READINESS.md:38` claims "OIDC/Keycloak and multi-organization tenant isolation remain deployment roadmap work," but both are implemented (`crates/auth/src/rbac.rs:266-310`, `organization_memberships`, `scripts/test_organization_isolation.sh`).
- **Governance files missing** (plan §23.2): `.github/ISSUE_TEMPLATE/*` and `.github/PULL_REQUEST_TEMPLATE.md`. Only 1 of 10 planned ADRs exists (`docs/adr/0001-modular-monolith-migration.md`).
- **`docs/architecture.md` (lowercase, 9 lines, stale) coexists with `docs/ARCHITECTURE.md` (369 lines, current).** Both are tracked; the lowercase one should be deleted.
- **Legacy Python stack is still tracked** alongside its Rust replacements (`api-gateway/`, `ediparser/`, `llm-service/`, `rag-engine/` Python dirs) — dead weight and an extra audit surface.
- **Secrets are plain `String` fields deriving `Debug`** (`crates/common/src/config.rs`); no `secrecy`-style wrapper (plan §16.3). Mitigations exist (`env_required_secret()` boot guard, `safe_error()` redaction).
- **`.env.example` ships a real internal IP:** `LLAMA_BASE_URL=http://10.10.10.98:8080` (line 11). Minor info leak; replace with `http://localhost:8080`.
- **Missing docs** (plan §7): `deployment.md` (only a security checklist exists), `edi-support.md`, `ai-safety.md`.

### 🟢 Positive — genuinely well done

- **No real PHI in the repo.** All suspect root files (`.hdi835.dat`, `.*835.*`, `icd10_october2026_0.csv`) are **untracked and gitignored**. Tracked fixtures (`scripts/sample_835.txt`, `sample_837p/i.txt`, `database/seed/sample_data.sql`) use synthetic data (Doe/Smith, 555 phone range, `999`/`1987…` NPI prefixes, `PAT00x` IDs).
- **Parser is defensively built:** delimiter auto-detection (`crates/edi-core/src/common.rs:78-123`), hard limits (200k segments, 16 KiB/segment, 256 elements, 25 MB upload, 64 MB body), envelope validation, and a cargo-fuzz target.
- **RBAC is centralized and enforced** (`crates/auth/src/rbac.rs:348-379,533,582-601`); org scoping verified across claims/denials/analyses/retention/feedback queries.
- **Citation integrity is enforced:** the LLM service rejects any `evidence_id` not in the retrieved set (`crates/llm-service/src/main.rs:338-349`) — no fabricated citations.
- **Deterministic fallback works** when AI is unavailable (`crates/api-gateway/src/routes/analyses.rs:405-477`) — plan §12.1 requirement met.
- **Storage abstraction is real** (local + hand-rolled SigV4 S3, `crates/storage/src/lib.rs`), **hybrid retrieval** (0.7 vector + 0.3 lexical, `crates/rag-engine/src/main.rs:97-153`), **playbook approval lifecycle** with manager gating, and a **similar-resolved-case retrieval that deliberately excludes patient identifiers** (`crates/api-gateway/src/routes/feedback.rs:402-435`).
- **Deployment is production-minded:** non-root containers (`Dockerfile.rust:38,40`), production compose with fail-fast secrets, reverse-proxy TLS example, backup/restore with `--verify-only` drill, SBOM + Trivy in CI.

---

## 2. Milestone Scorecard

Legend: ✅ verified · ⚠️ partial/divergent · ❌ missing

| Milestone | ✅ | ⚠️ | ❌ | Net assessment |
|-----------|----|----|----|----------------|
| **0** Foundation | 14 | 4 | 0 | Solid. pnpm declared but npm used; OpenAPI/TS client are Python-generated type maps, not utoipa + real client. |
| **1** X12 Core + 835 | 15 | 4 | 0 | Strong. QTY unhandled; tokenizer in-memory (not streaming); fixtures lack delimiter/edge variety. |
| **2** Denial Work Queue | 8 | 7 | 1 | **Activity timeline missing entirely.** Detail UI is a modal, not the plan's tabbed view; saved views are localStorage-only; bulk op queues resolutions, not owner assignment. |
| **3** 837 Correlation | 5 | 3 | 2 | **Weakest milestone.** Scored correlation + ambiguous-match confirmation (the headline feature) is missing — replaced by an exact-match upsert. No 837 unit/fuzz tests. |
| **4** Knowledge Base | 12 | 4 | 0 | Strong. HTML input unsupported; no tags/org-only flags; pgvector migration unconditional. |
| **5** Recommendation Engine | 12 | 6 | 2 | Functional but `RecommendationProvider`/`AiProvider` abstractions missing; no advisory UI; in-process jobs; eval harness not offline. |
| **6** Playbooks + Feedback | 11 | 0 | 0 | **Best milestone.** Fully verified. |
| **7** Auth/RBAC/Security | 8 | 6 | 1 | Roles diverge from plan; threat model thin; container scan is FS not image; isolation test not in CI. |
| **8** Analytics | 13 | 0 | 0 | **Fully verified.** All endpoints + Dashboard + Insights UI + audited CSV export. |
| **9** Ingestion + Deploy | 11 | 1 | 0 | Strong. Helm chart exists but thin (no migration hooks/tests). |
| **10** Release Readiness | 8 | 4 | 1 | No privacy/security review doc; fuzz campaign + backup/upgrade tests have no committed evidence. `v1.0.0` tag + signed checksums correctly absent. |

**Bottom line:** Milestones **6, 8, 9** are clean. Milestone **3** is the biggest overstatement — its defining requirement (scored, confirm-gated correlation) is not implemented.

---

## 3. Plan-Section Compliance Matrix

| Plan § | Requirement | Status | Evidence |
|--------|-------------|--------|----------|
| §0.6 | Never log PHI/payloads/tokens/secrets/raw EDI | ⚠️ | Logs are clean, but **audit metadata stores `patient_name` + raw query** (see Critical #1/#2). |
| §5.5 / §12 | `AiProvider` / `RecommendationProvider` abstractions | ❌ | No traits; monolithic `LlamaClient`. Only `EmbeddingProvider` exists. |
| §7 | Repository layout | ⚠️ | No `packages/`, no top-level `fixtures/`, migrations under `database/`, extra crates (`common`, `llm-service`, `rag-engine`, `ediparser`). |
| §8 | Domain model | ⚠️ | Service lines/adjustments/remarks not normalized into tables; several planned tables absent. |
| §9.4 | Scored correlation, no silent merge | ❌ | Exact-match upsert, hardcoded 1.00, no ambiguity flow. |
| §9.5 | Fixtures synthetic + labeled | ✅ | Synthetic in substance; in-repo labeling is via code/docs, not the files themselves. |
| §10.2 | Configurable root-cause taxonomy | ⚠️ | Hardcoded 10-value CHECK, not a table. |
| §12.2 | PHI levels, default `deidentified` | ⚠️ | Levels are `none/limited/full`, default `limited`, env-only. |
| §13.2 | Request IDs / idempotency / error codes / optimistic concurrency | ⚠️ | Cursor pagination + org scoping ✅; the rest absent. |
| §16.1 | Threat model covers the enumerated threats | ⚠️ | 24-line summary; most threats not individually addressed. |
| §16.3 | Secrets wrapped to avoid debug output | ⚠️ | Plain `String` + `Debug`; boot guard + redaction mitigate. |
| §16.4 | Retention for AI records | ❌ | No prune/retention for `ai_analyses`. |
| §16.5 | Never log patient name / member ID / DOB | ❌ | Violated via audit `details` (see Critical #2). |
| §17.1 | User-facing advisory disclosures | ❌ | Absent from UI. |
| §17.3 | Don't store full prompts by default | ❌ | `raw_prompt`/`raw_response` always stored. |
| §18 | Postgres-backed durable job queue | ⚠️ | In-process `tokio::spawn`; lost on restart. |
| §20 | OpenTelemetry metrics | ❌ | No metrics/OTel. |
| §21.2/§21.4 | Playwright E2E + critical happy path | ❌ | No Playwright; shell-script E2E only. |
| §22.1 | One-command dev + justfile command set | ⚠️ | `just dev` works; missing `worker`, `migrate`, `seed`, `db-reset`, `synthetic-data`, `security-check`. |
| §23.2 | Governance files | ⚠️ | Missing ISSUE_TEMPLATE + PR template; 1/10 ADRs. |
| §2.2 | 7 RBAC roles | ❌ | Only 5 roles, different names. |

---

## 4. Recommended Actions (prioritized)

### Do first — security blockers (before any real PHI)
1. **Scope `label_query()` by `organization_id`** and **stop persisting `patient_name`/raw `query`** into `audit_log.details`. Replace with non-PHI labels (resource id + type only). `crates/audit/src/lib.rs:123-159`.
2. **Gate `raw_prompt`/`raw_response` storage** behind a deployment flag (default off, or store only when disclosure level is `none`/`deidentified`), and add retention/prune for `ai_analyses`.
3. **Rebuild claim correlation** as a scored deterministic matcher using the §9.4 candidate fields, store confidence + matching reasons, and add an explicit ambiguous-match confirmation flow (or, at minimum, stop auto-merging when confidence < 1.0 and require user action).
4. **Add the AI advisory-only disclosures** to the Recommendation UI and Denial Detail.

### High value
5. Fix the broken CI smoke test (`.jsx` → `.tsx` imports) — currently the CI `npm run smoke` step cannot pass.
6. Add **Playwright** E2E for the §21.4 happy path; wire `scripts/test_organization_isolation.sh` into CI.
7. Introduce the `AiProvider` / `RecommendationProvider` traits; make PHI level admin-configurable with a `deidentified` default.
8. Align the role set with plan §2.2 (or document the deviation via an ADR).
9. Add request IDs, machine-readable error codes, and idempotency keys; use the `version` column for optimistic concurrency.

### Hygiene / docs
10. Delete stale `docs/architecture.md` (lowercase); add `deployment.md`, `edi-support.md`, `ai-safety.md`; correct the OIDC/multi-org "roadmap" claim in `RELEASE_READINESS.md`.
11. Remove the real internal IP from `.env.example`; wrap secrets in a non-`Debug` type.
12. Add `.github/ISSUE_TEMPLATE/*` + `PULL_REQUEST_TEMPLATE.md`; write the remaining ADRs (esp. one documenting the correlation and schema divergences).
13. Decide on the legacy Python service dirs — either delete them or clearly mark them deprecated.
14. Commit the pending `028`/`029` org-isolation migrations (currently untracked).

---

## 5. Notes on Plan Hygiene

Per plan §0.11 ("Update this plan.md as architectural decisions are made"), several real divergences have **not** been recorded: the exact-match correlation approach, the reduced schema, the 5-role model, the `none/limited/full` PHI levels, and the Python→Rust migration (partially covered by ADR-0001). These should each get an ADR so the plan and the code stop silently disagreeing.
