# OpenClaim Navigator — Functional & Security Audit

**Audited against:** `plan.md` (treated as source of truth)
**Repo state:** HEAD `91305ce` (working tree clean), Linux, Rust + TypeScript
**Scope:** "Does it function as intended?" + "Is it safe to run in a hospital for claim-denial assistance?" + "What is missing from `plan.md`?"
**Method:** Independent, read-only verification of every high-severity claim against the current source (the prior `audit.md` baseline at `13d8591` was re-checked, not trusted). Build/test evidence was executed, not assumed. Findings cite `file:line`.

---

## 1. Verdict

| Question | Answer |
|----------|--------|
| **Does it function as intended?** | **Mostly, yes.** The core vertical slice (835/837 ingest → normalized claims → denial queue → CARC/RARC display → deterministic + AI recommendation → resolution → analytics) is built and working. Workspace compiles; 33/33 Rust tests pass; all 14 web pages render. |
| **Is it safe to use in a hospital as-is?** | **No — not yet.** The **4 Critical** defects (cross-tenant PHI in audit, PHI in audit metadata, silent claim merge, unbounded AI prompt storage) and 4 High items (advisory disclosures, PHI disclosure levels, cross-tenant digest, workflow state machine) are now **fixed** (2026-09-18). Remaining High gaps (no `AiProvider` abstraction, role-set mismatch, no metrics, thin threat model/checklist, no E2E) and the §6 plan gaps are still open. The *foundation* (RBAC, org scoping, PHI-bounded logging, prompt-injection defense, non-root deployment) is genuinely sound; the gaps are real but fixable. |
| **Is `plan.md` complete?** | **No.** The plan omits several controls a hospital deployment actually depends on (see §6). Several implemented divergences were never recorded back into the plan or an ADR. |

**Bottom line:** the deterministic, AI-off revenue-cycle workflow is production-usable *for a single trusted organization*. It is **not** safe to expose to real PHI in a multi-tenant or hospital setting until the 4 Critical items in §3 are fixed.

---

## 2. Functional verification (executed)

| Check | Result |
|-------|--------|
| `cargo check --workspace` | ✅ Compiles (19 warnings, 0 errors) |
| `cargo test --workspace` | ✅ **33 passed, 0 failed** |
| `npm run smoke` (web) | ✅ All 14 pages render (Dashboard, Claims, Denials, Appeals, Worklist, KnowledgeBase, Audit, Users, Upload, Settings, Login, Profile, Insights, Playbooks) |
| EDI parser | ✅ Defensive: delimiter auto-detect, hard limits (200k segments / 16 KiB / 256 elements / 25 MB), envelope validation, cargo-fuzz target |
| Deterministic fallback (AI off) | ✅ Works — plan §12.1 met |
| Org scoping in handlers checked | ✅ `get_claim`, `update_appeal`, `update_denial` all org-scoped; child queries keyed by an org-validated parent row |

**Caveats on "functions as intended":**
- The **E2E "happy path" is a shell script** (`scripts/test_synthetic_e2e.sh`) hitting a live stack, **not** an automated Playwright browser test. Plan §21.2/§21.4 and Slice A's final item all require Playwright E2E — **none exists**.
- The **AI evaluation harness reads the live DB, not synthetic offline cases** (`scripts/eval_model.py:38,66,81,174` connect via `asyncpg` to `denials`). This both violates §17.4 and is itself a PHI exposure (it reads real denials).
- **No separate `openclaim-worker` process** (plan §6.1). Recommendation jobs run as in-process `tokio::spawn` (`crates/api-gateway/src/routes/analyses.rs:544`); a gateway restart drops `pending`/`running` jobs, and failures are terminal (no retry/dead-letter) — diverges from §18.

---

## 3. Security findings

### 🔴 Critical — must fix before any real PHI

> **Status (2026-09-18): C1–C4 are fixed, and High items H1, H2, H4, H5 are fixed.** Each item below carries a ✅ note describing the remediation. The remaining High items (H3, H6–H10) are still open.

**C1. Cross-tenant PHI leak via the audit label lookup.** ✅
`crates/audit/src/lib.rs:137-165` — `label_query()` fetches `claim_number` **and `patient_name`** from `claims`/`denials`/`appeals_queue`/`ai_analyses` with **no `organization_id` filter**. The audit row itself is attributed to the *requester's* org (`record()`, `lib.rs:231-236` derives `organization_id` from the caller's membership), but the `details` payload is populated by `label_for()` (`lib.rs:171-215`) from the unscoped lookup. **Effect:** an authenticated user in Org A who probes `/api/v1/claims/<Org-B-uuid>` gets a 404 from the (correctly scoped) handler, *and* Org B's `claim_number` + `patient_name` is written into an audit row readable by Org A's auditors. A direct broken-object-authorization / cross-tenant PHI leak. Violates plan §0.6, §8.10, §16.1 (IDOR), §16.5.

> **Fix:** every org-scoped label query now appends `AND organization_id = (SELECT organization_id FROM organization_memberships WHERE user_id = $2 …)` so a cross-tenant UUID probe resolves to no row; `label_for()` takes the caller's `user_id` and refuses to label when it is absent. `patient_name` was also removed (see C2).

**C2. Audit metadata persists PHI.**
`crates/audit/src/lib.rs:343-359` — the middleware inserts the `label_for()` output (incl. `patient_name`), the **raw request `query` string**, and `username` into `audit_log.details`. Plan §8.10: "Never place PHI or raw claim payloads in audit metadata." These rows are retained for years (audit retention floor). Violates §8.10, §16.5.

> **Fix:** `patient_name` removed from `LABEL_COLUMNS` and all label queries; the raw `query` string is no longer persisted (the `path` already identifies the endpoint); `generate_analysis` no longer writes `patient_name` into its audit record.

**C3. 837→835 claim correlation silently merges — the exact behavior the plan forbids.**
`crates/api-gateway/src/routes/ingestion.rs:373-392` — `ON_CONFLICT_837` is a plain upsert on `(organization_id, claim_number)` that **hardcodes `correlation_confidence = 1.00`** and sets `correlation_status = 'matched'`. There is **no scored matcher**, none of the §9.4 candidate fields (subscriber ID, statement/service dates, billed amount, provider IDs) are used, no matching reasons are stored, and there is **no ambiguous-match confirmation**. The `correlation_confidence` column and `ambiguous` status are effectively dead schema. Violates §9.4 ("Never silently merge ambiguous claims") and fails Milestone 3's exit criteria by construction.

> **Fix:** the 837 path is now a scored deterministic matcher (`correlation_score`, `upsert_claim`). Weights: claim_number 0.5 (base, always matches), service_from 0.1, service_to 0.1, billed amount 0.15, provider NPI 0.1, payer 0.15 (case-insensitive); a field missing on the 837 earns nothing. Threshold 0.6: a corroborated match merges and records the real `correlation_confidence`; an unconfirmed match sets `correlation_status = 'ambiguous'` and does **not** merge over the 835. 5 unit tests lock in the threshold behavior.

**C4. Full AI prompts/responses persisted unconditionally, with no retention.**
`crates/api-gateway/src/routes/analyses.rs:219-246` — `INSERT INTO ai_analyses … raw_prompt, raw_response …` binds both **unconditionally**. At the `full` disclosure level the claim reference (a quasi-identifier) is written to disk. **No retention/prune exists for `ai_analyses`** — `retention.rs` only prunes `audit_log`. Violates §17.3 ("avoid storing full prompts by default") and §16.4.

> **Fix:** `raw_prompt`/`raw_response` are now bound as `NULL` unless `AI_STORE_RAW_ARTIFACTS=true` (new `store_raw_ai_artifacts` config, default **false**), so the secure default stores only the structured, redacted result. Gated in the gateway's `store_analysis`, which covers both the LLM service and the deterministic fallback. A deliberate, admin-only, org-scoped prune was added: `GET/POST /api/v1/retention/ai[\/prune]` (mirrors the audit-log prune: retention floor, `confirm=true`, self-auditing).

 ### 🟠 High — material gaps vs. the plan

> **Status (2026-09-19): H1–H6 are fixed** (✅ notes below). H7–H10 remain open.

- **H1. No user-facing AI-safety disclosures** (plan §17.1). No "advisory only / validate before acting / AI can be wrong" messaging anywhere in `apps/web/src` (grep: zero hits). ✅ **Fixed:** an advisory-only disclosure ("AI-generated and **advisory only** — the AI can be wrong. Verify against the claim and payer rules before acting.") is now shown on the four AI-surface pages: `Denials`, `Worklist`, `Claims`, `Appeals`.
- **H2. PHI disclosure levels diverge and are env-only.** Plan §12.2: `none` / `deidentified` / `limited_phi` / `full_context`, **default `deidentified`**. Code: `none` / `limited` / `full` (`crates/ai/src/prompt.rs:6-19`), **default `Limited`** (`prompt.rs:194`), set by env var, not admin-configurable. *Mitigant:* the prompt builder sends **no patient name or DOB** — only the claim reference (redacted at `limited`/`none`) + CPT/ICD/CARC/RARC + payer + retrieved policies. ✅ **Fixed:** the enum is now the four plan levels (`none`/`deidentified`/`limited_phi`/`full_context`, legacy names still parse), the default is `deidentified`, and the level is **admin-configurable** — stored in a new `system_settings` table (migration 030), set via admin-only `GET/PUT /api/v1/settings/phi-disclosure`, and passed per-request to the RAG engine which applies it (falling back to its configured default).
- **H3. No `AiProvider` / `RecommendationProvider` abstractions** (plan §5.5/§12). Only `EmbeddingProvider` exists; the chat path is a monolithic `LlamaClient`. ✅ **Fixed:** shared `AiProvider` and gateway `RecommendationProvider` seams now wrap the OpenAI-compatible transport and deterministic fallback without changing external behavior.
- **H4. Deadline-digest job leaks across tenants.** `crates/api-gateway/src/routes/notifications.rs:253-281` — the overdue-denial `COUNT/SUM` query and the `SELECT id FROM users WHERE role = ANY(…)` manager query are **both unscoped by org**, then notifications are fanned out to **all** orgs' managers with a **cross-org aggregate** count/amount. (Lower severity than C1 — aggregate figures, not patient names — but still a cross-tenant information bleed and mis-targeted notifications.) ✅ **Fixed:** `generate_digests` now iterates per-organization; the orphan-denial aggregate joins `claims` and is scoped by `c.organization_id`, and the manager fan-out is scoped via `organization_memberships` to the same org's manager-role members.
- **H5. Workflow state machine is not enforced.** `crates/api-gateway/src/routes/denials.rs:702-749` — `update_denial` accepts **any** `CHECK`-valid status from **any** state. `denials.status` is a flat 8-value set (`open, analyzed, in_progress, in_appeal, appealed, overruled, resolved, written_off`), not the §30 state machine. Transitions are neither validated nor individually audited. ✅ **Fixed:** `update_denial` now fetches the current status (org-scoped), validates the transition against an `allowed_transitions` map, rejects invalid moves with `409 Conflict`, and records each transition in the denial audit log.
- **H6. Role set does not match plan §2.2.** `database/init.sql:353` allows only 5 roles (`billing_specialist, billing_manager, rcm_director, admin, auditor`). The plan's `system_admin`, `security_admin`, `revenue_cycle_manager`, `coding_specialist`, `read_only` are absent. ✅ **Fixed:** migration 032 normalizes legacy roles, constrains users and memberships to all seven plan roles, and updates API/RBAC/UI role handling.
- **H7. No OpenTelemetry / metrics** (plan §20). Only structured logs + `/health/live` + `/health/ready`.
- **H8. Threat model is a 24-line summary** (`docs/threat-model.md`) and **does not individually address** the §16.1 enumerated threats (IDOR, cross-tenant, SQLi/XSS/CSRF, SSRF, prompt injection, malicious admin, supply chain, object-store/backup/export exposure, excessive retention).
- **H9. Deployment security checklist is minimal** (23 lines) — no BAA, network segmentation, HIDS, DLP, incident-response, or breach-notification items.
- **H10. No Playwright E2E** (see §2).

### 🟡 Medium — divergences, hygiene, and missing polish

- **M1. Schema diverges from §8.** No `import_batches`, `source_files`, `service_lines`, `adjustments`, `remarks`, `payers`, `recommendations` tables. Service lines are embedded in `claims.raw_835_data JSONB`; adjustments live inside the `denials` row.
- **M2. Root-cause taxonomy is a hardcoded 10-value `CHECK`** on `ai_analyses.denial_category`, not a seeded/configurable/org-customizable table (§10.2).
- **M3. API hygiene gaps (§13.2):** no request IDs, no machine-readable error `code`, no `Idempotency-Key`; the `version` column is incremented but not used for optimistic-concurrency conflict detection.
- **M4. `.env.example:11` ships a real internal IP** — `LLAMA_BASE_URL=http://10.10.10.98:8080`.
- **M5. Docs contradiction.** `docs/RELEASE_READINESS.md:38-39` claims "OIDC/Keycloak and multi-organization tenant isolation remain deployment roadmap work," but both are implemented (`crates/auth/src/rbac.rs:555,582-601`, `organization_memberships`, `scripts/test_organization_isolation.sh`).
- **M6. Governance files missing (§23.2):** no `.github/ISSUE_TEMPLATE/*`, no `PULL_REQUEST_TEMPLATE.md`; only **1 of 10** planned ADRs exists (`docs/adr/0001-modular-monolith-migration.md`).
- **M7. `QTY` segments unhandled** in the 835 parser (Milestone 1 lists QTY; no `QTY` handling in `crates/`).
- **M8. Legacy Python service dirs still tracked** (`ediparser/`, `llm-service/`, `rag-engine/` Python) alongside their Rust replacements — dead weight + extra audit surface.
- **M9. `justfile` missing several §22.1 commands** (`worker`, `migrate`, `seed`, `db-reset`, `synthetic-data`, `security-check`).
- **M10. Real PHI files present in the working tree.** `.hdi835.dat`, `.835cg.txt/.pdf`, `.fcso835.txt`, `.uhc835.txt`, `icd10_october2026_0.csv`, `.835fields.tmp` are **gitignored (not in git — good)** but **exist on disk** in the repo root. They are real-looking payer EDI files. This is a local PHI-hygiene risk: any backup, sync, or shared mount of the working directory would export them.

### 🟢 Positive — genuinely well done (verified)

- **No real PHI in git.** All suspect root files are untracked/gitignored; tracked fixtures (`scripts/sample_835/837p/837i.txt`, `database/seed/`) are synthetic (Doe/Smith, 555 range, `999`/`1987…` NPI prefixes, `PAT00x` IDs).
- **No PHI in tracing statements** (grep across `crates/` for patient/claim/dob/member in `tracing::` = clean). `safe_error()` (`crates/common/src/logging.rs:10-32`) bounds and redacts transport/config diagnostics.
- **RBAC is centralized and enforced** (`crates/auth/src/rbac.rs:384-423`); every human request must resolve to an **active org membership** before repositories use tenant context (`rbac.rs:582-601`); per-request account-currency check (`is_active`, `role_changed`, `credentials_changed`, `rbac.rs:429-477`).
- **Prompt-injection defense** is present in the system prompt (`crates/ai/src/prompt.rs:114-116`): retrieved text is untrusted evidence, never instructions; the LLM service rejects `evidence_id`s not in the retrieved set (no fabricated citations).
- **Deterministic fallback works** with AI off; **hybrid retrieval** (vector + lexical); **playbook approval lifecycle** with manager gating; **similar-resolved-case retrieval deliberately excludes patient identifiers**.
- **Deployment is production-minded:** non-root containers, production compose with fail-fast secrets, reverse-proxy TLS example, backup/restore with a `--verify-only` drill, SBOM + Trivy in CI.

---

## 4. Plan-section compliance matrix (re-verified at `91305ce`)

| Plan § | Requirement | Status | Evidence |
|--------|-------------|--------|----------|
| §0.6 / §16.5 | Never log PHI / patient name | ✅ | Fixed — `patient_name` removed, label queries org-scoped (C1/C2) |
| §8.10 | No PHI in audit metadata | ✅ | Fixed — no `patient_name`, no raw `query` in `details` (C2) |
| §9.4 | Scored correlation, no silent merge | ✅ | Fixed — scored matcher, ambiguous matches not merged (C3) |
| §17.3 | Don't store full prompts by default | ✅ | Fixed — gated behind `AI_STORE_RAW_ARTIFACTS` (default off) (C4) |
| §16.4 | Retention for AI records | ✅ | Fixed — admin-only `/retention/ai` prune (C4) |
| §17.1 | User-facing advisory disclosures | ✅ | Fixed — advisory-only disclosure on the 4 AI-surface pages (H1) |
| §2.2 | 7 RBAC roles | ✅ | Fixed — seven canonical roles with normalized legacy data (H6) |
| §20 | OpenTelemetry metrics | ❌ | None (H7) |
| §21.2/§21.4 | Playwright E2E + happy path | ❌ | Shell-script only (H10) |
| §5.5/§12 | `AiProvider`/`RecommendationProvider` | ✅ | Fixed — shared AI and recommendation provider seams (H3) |
| §12.2 | PHI levels, default `deidentified` | ✅ | Fixed — 4 plan levels, default `deidentified`, admin-configurable (H2) |
| §10.2 | Configurable root-cause taxonomy | ⚠️ | Hardcoded 10-value CHECK (M2) |
| §13.2 | Request IDs / idempotency / error codes / optimistic concurrency | ⚠️ | Cursor pagination + org scoping ✅; rest absent (M3) |
| §16.1 | Threat model covers enumerated threats | ⚠️ | 24-line summary (H8) |
| §18 | Durable Postgres job queue | ⚠️ | In-process `tokio::spawn`, terminal on failure |
| §30 | Enforced, audited workflow state machine | ✅ | Fixed — `allowed_transitions` validation + per-transition audit (H5) |
| §7 | Repository layout | ⚠️ | No `packages/`, migrations under `database/`, extra crates |
| §8 | Normalized domain model | ⚠️ | Service lines/adjustments/remarks not normalized (M1) |
| §22.1 | justfile command set | ⚠️ | Missing several commands (M9) |
| §23.2 | Governance files | ⚠️ | No ISSUE/PR templates; 1/10 ADRs (M6) |
| §17.4 | Offline synthetic eval harness | ⚠️ | Reads live DB (M — see §2) |
| §16.2 | App security controls (TLS, CSP, limits, rate-limit) | ✅ | Present |
| §15.2 | Centralized authorization + org scoping | ✅ | Verified (see §3 positive) |
| §12.1 | Deterministic fallback with AI off | ✅ | Verified |
| §9.5 | Synthetic, labeled fixtures | ✅ | Synthetic in substance |

---

## 5. Hospital-use (HIPAA) readiness

The code's *technical* foundation is appropriate for a covered-entity deployment: RBAC + org isolation, PHI-bounded logging, prompt-injection defense, non-root hardened containers, backup/restore, SBOM/Trivy. The four **Critical** defects (C1–C4) are fixed, and six High items are now fixed: AI-safety disclosures (H1), admin-configurable PHI levels (H2), provider abstractions (H3), org-scoped digests (H4), audited workflow transitions (H5), and canonical seven-role RBAC (H6). Remaining High items H7–H10 and the §6 plan gaps are still open, so until those are closed and implemented this should still be treated as a **single-trusted-tenant, synthetic-data-only** system.

---

## 6. What is MISSING from `plan.md` (should be added)

The plan is strong on *what to build* and *deterministic-before-generative*, but it omits controls a hospital deployment actually depends on. Recommend adding these sections:

1. **Data classification scheme.** The plan never defines a classification (PHI / de-identified / public) for data elements. This is the foundation for every other control (minimization, retention, access). Add a table classifying each field (patient name, DOB, member ID, NPI, claim reference, codes) and the allowed handling per class.
2. **De-identification standard for the AI boundary.** §12.2 lists levels but never specifies the field-level redaction mapping or which standard applies (HIPAA Safe Harbor vs. Expert Determination). Add the exact identifier inventory and the rule that "de-identified" means no §164.514 identifiers, with a test that verifies de-identified output contains none.
3. **Business Associate / AI-provider data-processing section.** §16.6 covers HIPAA *positioning* but never addresses the central question: is the (even self-hosted) model/embedding provider a Business Associate? What must a BAA cover, and how is it verified that PHI stays inside the approved boundary? Add an explicit section + a "PHI boundary" diagram.
4. **PHI-access audit trail.** §8.10 has `audit_events` but does not require a **who-viewed-which-patient** trail. HIPAA §164.312(b) requires audit controls for ePHI access. Add an explicit requirement to log PHI-field access (role, user, resource, timestamp) — *without* duplicating the PHI itself into the log (which is exactly what C2 does wrong).
5. **Incident response & breach notification.** No section exists. Add detection, containment, HHS/individual breach-notification, and forensics procedures.
6. **Legal hold on retention.** §16.4 has configurable retention but no **legal-hold** mechanism to pause deletion during litigation/investigation — required for healthcare records.
7. **Application-layer encryption & key management.** §5.3 defers encryption to "deployment infrastructure." Add field-level encryption (or an explicit decision not to) for the most sensitive fields, plus key management and **encrypted backups**.
8. **MFA & session-management spec.** The code has TOTP, but the plan never *requires* MFA for PHI access, nor specifies session timeout, idle lock, or concurrent-session limits for a hospital terminal environment.
9. **Network segmentation.** The architecture diagram never addresses segmentation between the app, model server, object store, and EHR/clearinghouse — a core HIPAA technical safeguard. Add a network diagram with trust boundaries.
10. **Audit-log integrity / tamper-evidence.** §16.2 mentions "audit log integrity controls" but specifies nothing. Add hash-chaining / append-only / WORM requirements.
11. **DR / RPO-RTO.** §26 mentions backup/restore but no RPO/RTO targets or failover for a hospital needing continuity.
12. **Human-approval workflow spec for AI actions.** §0.8/§12 say "no silent mutation" but never specify *who* must approve, what the approval record looks like, or how a rejected AI recommendation is tracked. Add the exact approval workflow.
13. **Fail-closed production auth.** §15.1 mentions a "development-only local auth mode" but never specifies how it is disabled/enforced in production. Add a fail-closed requirement (production refuses to boot without OIDC + real secrets).
14. **Correlation scoring spec.** §9.4 lists candidate fields but no scoring algorithm, threshold, or the ambiguous-match UX — the gap that led directly to the C3 silent-merge implementation. Add the algorithm + a "confidence < threshold ⇒ require user confirmation" rule.

---

## 7. Prioritized remediation

**Do first — before any real PHI:**
1. ✅ Scope `label_query()` by `organization_id` and **stop persisting `patient_name`/raw `query`** into `audit_log.details`; use non-PHI labels (resource id + type only). `crates/audit/src/lib.rs:122-165,343-359`.
2. ✅ Gate `raw_prompt`/`raw_response` storage behind a deployment flag (default off, or only when disclosure is `none`/`deidentified`) and add retention/prune for `ai_analyses`. `crates/api-gateway/src/routes/analyses.rs:219-246`.
3. ✅ Rebuild 837→835 correlation as a **scored deterministic matcher** using the §9.4 fields; store confidence + matching reasons; add an explicit ambiguous-match confirmation flow (or stop auto-merging below a threshold). `crates/api-gateway/src/routes/ingestion.rs:373-392`.
4. ✅ Scope the deadline-digest job to the requesting organization. `crates/api-gateway/src/routes/notifications.rs:253-281`.
5. ✅ Add the **AI advisory-only disclosures** to the Recommendation UI and Denial Detail (H1).

**High value:**
6. Add **Playwright** E2E for the §21.4 happy path; wire `scripts/test_organization_isolation.sh` into CI.
7. ✅ Make PHI level **admin-configurable** with a `deidentified` default and introduce `AiProvider`/`RecommendationProvider` traits (H2/H3).
8. ✅ Enforce the §30 workflow state machine in `update_denial` and audit each transition (H5).
9. ✅ Align the role set with §2.2 and normalize legacy roles (H6).
10. Add request IDs, machine-readable error codes, idempotency keys; use `version` for optimistic concurrency (M3).
11. Make the eval harness **offline/synthetic** (stop reading the live DB).

**Hygiene / docs / plan:**
12. Expand `docs/threat-model.md` to cover every §16.1 threat; expand the deployment checklist (H8/H9).
13. Remove the real internal IP from `.env.example`; **remove the real EDI files from the working tree** (M4/M10).
14. Add the **§6 plan sections** and record the implemented divergences (correlation, schema, 5-role model, PHI levels) as ADRs.
15. Add `.github/ISSUE_TEMPLATE/*` + `PULL_REQUEST_TEMPLATE.md`; write the remaining ADRs (M6).
16. Decide on the legacy Python service dirs — delete or clearly mark deprecated (M8).

---

## 8. Note on plan hygiene

Per plan §0.11 ("Update this plan.md as architectural decisions are made"), several real divergences were **never recorded**: the exact-match correlation approach, the reduced schema, the 5-role model, the `none/limited/full` PHI levels, and the in-process job model. Each should get an ADR so the plan and the code stop silently disagreeing. Separately, the plan itself is missing the hospital-critical sections enumerated in §6.
