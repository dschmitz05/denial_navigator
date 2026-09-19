# Feature Backlog — Gaps Not Covered by plan.md or audit.md

Written 2026-09-19 against `ea5c3b3`. Every item here is **new**: it is not in
`plan.md` (including §19.2 and §35 future extensions) and not an open finding or
proposed plan addition in `audit.md` (H7–H10, M1–M10, §6 items 1–14). Where an
item overlaps one of those, the overlap is named and only the extra scope is
listed.

## Priority legend

| Priority | Meaning |
|---|---|
| **P0** | Wrong data, money, or security exposure today. Do before real use. |
| **P1** | High value; the product is materially weaker without it. |
| **P2** | Valuable; schedule after P1. |
| **P3** | Nice to have. |

## How to work an item (for opencode)

1. Take one item per branch and commit; the IDs (`FB-01`…) go in the commit subject.
2. Read the files listed under **Where** before changing anything; line numbers are as of `ea5c3b3`.
3. Schema changes are a new numbered file in `database/migrations/` **and** the matching change in `database/init.sql`.
4. New or changed routes: update `crates/api-gateway/openapi/generate.py`, run it, and make sure `--check` passes.
5. Keep every query organization-scoped; add a case to `scripts/test_organization_isolation.sh` for any new tenant-owned table.
6. Done means: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`, `npm run build` in `apps/web`, the item's acceptance criteria, and docs updated.

## Summary

| ID | Priority | Item |
|---|---|---|
| FB-01 | P0 | Dependency health checks that test real function, not reachability |
| FB-02 | P0 | 835 claim reversals, re-adjudication, and automatic denial outcomes |
| FB-03 | P0 | Write-off approval thresholds and segregation of duties |
| FB-04 | P0 | Forced password change for seeded and admin-reset passwords |
| FB-05 | P1 | Visible "AI degraded" state and fallback-rate tracking |
| FB-06 | P1 | Secondary-coverage check before billing the patient |
| FB-07 | P1 | Persist and surface PLB provider-level adjustments (recoupments, interest) |
| FB-08 | P1 | Overpayment and credit-balance tracking with refund deadlines |
| FB-09 | P1 | Unanswered-claim follow-up (837 sent, no 835 received) |
| FB-10 | P1 | Payer-specific deadline types beyond a single appeal window |
| FB-11 | P1 | Retrieval scoped by date of service and canonical payer |
| FB-12 | P1 | Calibrated retrieval threshold and a retrieval evaluation set |
| FB-13 | P2 | Embedding provenance and a re-index job |
| FB-14 | P2 | Knowledge-document expiry alerts and supersession |
| FB-15 | P2 | Denial attachments and appeal packet assembly |
| FB-16 | P2 | Structured payer-interaction log |
| FB-17 | P2 | Worklist ordering by expected recovery |
| FB-18 | P2 | Dependency and secret scanning in CI |
| FB-19 | P2 | README and operator docs describe the removed Python stack |
| FB-20 | P3 | NCCI PTP/MUE reference data to explain bundling denials |

---

## P0

### FB-01 — Dependency health checks that test real function

**Why.** On 2026-09-18/19 the System Health page reported *LLM provider: ok* and *Embedding provider: ok* while every analysis failed with a 404 (wrong model name) and the embedding server was not running. `probe()` treats any HTTP response as healthy; the embedding probe calls Ollama's `/api/tags`, which the llama.cpp embedding server does not have; neither probe sends `LLM_API_KEY`.

**Where.** `crates/api-gateway/src/routes/system.rs:12` (`probe`), `:47` (LLM `/v1/models`), `:50` (embedding `/api/tags`); `crates/llm-service/src/main.rs` health handler.

**Do.**
- LLM: call `/v1/models` with the bearer key, resolve `LLM_MODEL` (`auto` → first served model) and require that the resolved name is served. Report `degraded` with the reason when not.
- Embeddings: `POST {EMBED_BASE_URL}/v1/embeddings` with one short input and require a vector of length 768 (the `vector(768)` column).
- Status is `ok` only for a 2xx with the expected payload; 401/403/404 are `down` with the upstream message (reuse `upstream_error` from `crates/llm-service/src/llama.rs`).
- Cache results for 30 s so the page does not load the model servers.

**Acceptance.** Setting `LLM_MODEL=does-not-exist`, removing `LLM_API_KEY`, or stopping the embedding container each turns the matching row non-ok with a reason that names the cause.

**Tests.** Unit-test the payload validators; extend the E2E health check to assert on `detail`.

### FB-02 — 835 claim reversals, re-adjudication, and automatic outcomes

**Why.** The parser stores `claim_status_code` (CLP02) but nothing acts on it. A payer reversal (CLP02 `22`) followed by a corrected payment arrives as new claim-payment loops: a reprocessed claim leaves the original denial open, and a payer re-sending the same remittance in a new file can double count. It also means denial outcomes (`feedback_loop.was_paid_on_resubmit`, which drives `/denials/financial-summary` recovery rate) are only ever recorded by hand.

**Where.** `crates/edi-835/src/lib.rs:364`, `crates/api-gateway/src/routes/ingestion.rs` (`upsert_claim` at `:450`, denial creation), `database/init.sql` (`denials`, `feedback_loop`).

**Do.**
- Match incoming claim loops to prior ones on payer claim control number (CLP07) plus patient control number (CLP01).
- CLP02 `22`: mark the prior payment as reversed (new `reversed_at`, `reversed_by_remittance_id`); do not create denials from reversal CAS lines.
- A later loop for the same claim that pays a previously denied line: resolve that denial with outcome `paid_on_reprocess` and record the outcome for recovery analytics automatically.
- Identical (claim, line, CARC, amount) from a re-sent file must not create a second denial; add a uniqueness key.

**Acceptance.** A synthetic sequence (denial → reversal → paid reprocess) ends with one denial, resolved as paid, and the recovery rate reflects it. Re-ingesting the same content under a new filename creates nothing new.

**Tests.** Add fixtures with `scripts/generate_synthetic_835.py`; unit tests in `edi-835`; an ingestion integration test.

### FB-03 — Write-off approval thresholds and segregation of duties

**Why.** A `write_off` resolution removes real money from A/R, and any user who may resolve a worklist item can do it for any amount (`crates/api-gateway/src/routes/appeals.rs:624`). Hospital finance controls normally require approval above a threshold, by someone other than the requester.

**Where.** `crates/api-gateway/src/routes/appeals.rs`, `crates/api-gateway/src/routes/settings.rs` (org settings), `crates/auth/src/rbac.rs`, Worklist UI in `apps/web/src/pages/`.

**Do.**
- Organization setting `write_off_approval_threshold` (default $0 = always requires approval; configurable per org).
- A write-off at or above the threshold goes to `pending_approval`; approve/reject by `revenue_cycle_manager` or higher, never by the requester; record approver, time, reason; audit both steps.
- Manager view listing pending write-offs with amounts and reasons.

**Acceptance.** A specialist's $500 write-off with a $100 threshold is not final until a different manager approves it; the requester cannot approve it; both steps are in the audit log.

**Tests.** RBAC unit tests for approve/deny; isolation-script case for cross-org approval.

### FB-04 — Forced password change for seeded and admin-reset passwords

**Why.** Every fresh database gets `admin` / `admin123` unless `INIT_ADMIN_PASSWORD` is set (`database/docker-initdb.sh:31-36`), and the README tells operators to change it by hand. Nothing forces it, and an admin-reset password stays usable indefinitely. (Separate from audit §6 item 8, which covers MFA and session timeouts.)

**Where.** `database/docker-initdb.sh`, `crates/api-gateway/src/routes/auth.rs`, `crates/api-gateway/src/routes/users.rs` (reset), `crates/auth/src/rbac.rs` (token scope), Login/Profile pages.

**Do.**
- `users.must_change_password BOOLEAN`, set for the seeded admin and after any admin reset.
- Login with the flag issues a `password-change`-scope token that only permits `POST /auth/change-password` (mirror the existing MFA-scope pattern).
- Minimum 12 characters, reject the username and a small bundled list of common passwords.

**Acceptance.** First login as the seeded admin cannot reach any other route until the password changes.

**Tests.** Auth unit tests for the scope; E2E login flow.

---

## P1

### FB-05 — Visible "AI degraded" state and fallback-rate tracking

**Why.** When the LLM or retrieval fails, analyses quietly fall back to deterministic rules. On 2026-09-19 every analysis had been falling back (model 404 and a 403 on the policy search) with nothing in the UI to say so.

**Where.** `crates/api-gateway/src/routes/analyses.rs` (fallback branch), `crates/api-gateway/src/routes/system.rs`, dashboard/denial detail pages.

**Do.**
- Record the fallback reason on each analysis (`fallback_reason`: `llm_error`, `retrieval_error`, `no_evidence`).
- System health reports the fallback share over the last 24 h; `degraded` above a configurable threshold (default 20%).
- Banner on AI surfaces while degraded; per-analysis badge "rules-based: AI unavailable" with the reason.

**Acceptance.** Stopping the LLM produces the banner after the next analyses; restoring it clears it within the window.

### FB-06 — Secondary-coverage check before billing the patient

**Why.** The deterministic rule and the PR playbooks send every group-code PR balance to `bill_patient`. When the patient has secondary coverage, that balance belongs to the secondary payer; billing the patient first is a compliance and patient-experience problem.

**Where.** `crates/edi-837` (SBR/other-subscriber loops), `crates/edi-835` (NM1*TT crossover, MOA/MIA), `crates/api-gateway/src/routes/analyses.rs` (`deterministic_recommendation`), `crates/domain/src/lib.rs` (resolution types).

**Do.**
- Capture other-coverage indicators from the 837 (secondary SBR) and 835 (crossover NM1*TT, crossover-related RARCs).
- New resolution type `bill_secondary`; PR balances on claims with known other coverage get it instead of `bill_patient`.
- The recommendation consistency check (README "never offered as a write-off") also refuses `bill_patient` when secondary coverage is known.

**Acceptance.** A PR-1 denial on a claim with a secondary SBR is recommended `bill_secondary`.

### FB-07 — Persist and surface PLB provider-level adjustments

**Why.** PLB segments are parsed (`crates/edi-835/src/lib.rs:161`) and then dropped: nothing in the gateway or schema stores them. PLB carries recoupments/offsets (WO), forward balances (FB), and interest (L6); an offset silently reduces a payment for a different claim, which billing staff need to see and reconcile.

**Where.** `crates/edi-835/src/lib.rs`, `crates/edi-core/src/schema.rs:35-45`, ingestion, new migration.

**Do.**
- Table `provider_adjustments` (organization, remittance/file, payer, reason code, amount, reference number, fiscal period).
- When the reference identifies a claim (WO/FB references commonly carry the payer claim control number), link it.
- Remittance view listing PLB lines; a report of recoupments by payer and month.

**Acceptance.** A synthetic 835 with a WO PLB shows the offset on the remittance and on the referenced claim.

### FB-08 — Overpayment and credit-balance tracking with refund deadlines

**Why.** Nothing tracks overpayments (duplicate payments, payment above the allowed amount, a reversal with no recoupment). For Medicare and many state programs, an identified overpayment must be reported and returned within a fixed window (60 days from identification for Medicare), so this is a compliance obligation, not just finance. Deadline values must be configurable and reviewed by compliance staff.

**Where.** Builds on FB-02 and FB-07; new migration and route module; dashboard.

**Do.**
- Detection: payment > allowed, two payments for the same claim line, reversal without offset.
- Record `identified_at`, a configurable `refund_due_days` per payer type, and status (`identified`, `refunded`, `recouped`, `disputed`).
- Include overdue overpayments in the deadline digest.

**Acceptance.** A synthetic duplicate payment creates an overpayment with a due date and shows in the digest when overdue.

### FB-09 — Unanswered-claim follow-up (837 sent, no 835 received)

**Why.** Correlation knows which 837 claims have no remittance, but nothing surfaces claims the payer never answered. Those are lost to timely filing without ever becoming "denials".

**Where.** `crates/api-gateway/src/routes/ingestion.rs` (correlation), `claims`, a new worklist view.

**Do.**
- A claim submitted by 837 with no 835 after N days (per payer, default 30) appears in a "No response" queue with days outstanding and days left before the payer's timely-filing limit (FB-10).
- Resolution types: `status_inquiry` (276), `resubmit`, `payer_contact`.

**Acceptance.** A synthetic 837 with no matching 835 appears after the configured days; it leaves the queue when an 835 arrives.

### FB-10 — Payer-specific deadline types

**Why.** `payer_appeal_policies` holds one `appeal_window_days` per payer (`database/init.sql:436`). Real payer rules have several different clocks: timely filing (from DOS), corrected claim, reconsideration, first- and second-level appeal (the test contracts in `scripts/fixtures/test_knowledge.json` use 120 / 90 / 60 / 180 days). The recommended action's own deadline is what matters to the specialist.

**Where.** `database/init.sql` (`payer_appeal_policies`), `crates/api-gateway/src/routes/denials.rs` (appeal windows), deadline digest.

**Do.**
- `payer_deadline_rules(payer, deadline_type, days, anchor)` with anchors `date_of_service`, `remittance_date`, `prior_decision_date`.
- Compute the deadline for the recommended resolution type; show all applicable deadlines on the denial.
- Migrate existing `appeal_window_days` rows to `deadline_type = 'appeal_level_1'`.

**Acceptance.** A corrected-claim recommendation on an ACME denial shows a 90-day deadline from the remittance date.

### FB-11 — Retrieval scoped by date of service and canonical payer

**Why.** Two retrieval gaps, both verified:
1. The analysis search sends only payer and organization (`crates/api-gateway/src/routes/analyses.rs`, policy search), although the RAG engine supports `effective_on` (`crates/rag-engine/src/main.rs:155-158`). Policies that had expired or were not yet in effect on the date of service are used as evidence.
2. Payer matching is an exact case-insensitive name match (`crates/rag-engine/src/main.rs:146,228`). The sample data has claims from both `BlueCross BlueShield` and `BLUECROSS BLUESHIELD OF ILLINOIS`; the first never retrieves documents filed under the second. (Audit M1 notes the missing `payers` table; this is the minimum slice needed for retrieval.)

**Do.**
- Pass the service date as `effective_on` from the analysis path.
- `payers` + `payer_aliases` (name variants and 835 payer IDs from N1*PR/REF); resolve claims and documents to a canonical payer ID and filter on it; keep name matching as a fallback.

**Acceptance.** A claim from "BlueCross BlueShield" mapped to BCBS of Illinois retrieves the BCBSIL test documents; a document expired before the date of service is not retrieved.

### FB-12 — Calibrated retrieval threshold and a retrieval evaluation set

**Why.** `MIN_SIMILARITY` defaults to 0.25 (`crates/rag-engine/src/main.rs:69`), but with nomic-embed-text the *wrong* best document scores about 0.59–0.64, so the threshold filters nothing. A query with no relevant policy still feeds the model the least-bad documents as "evidence". The model evaluation harness does not measure retrieval.

**Do.**
- A retrieval eval set: queries labelled with the expected document(s), built from `scripts/fixtures/test_knowledge.json` (the 2026-09-19 prefix comparison used 8 such queries).
- A script reporting top-1 accuracy, MRR, and the score distribution of correct vs best-wrong matches; set the default threshold from it.
- A relative cut as well (drop results scoring more than X below the top result).
- When nothing passes, the analysis records `no_evidence` (FB-05) instead of citing weak matches.

**Acceptance.** The eval script runs in CI against a seeded stack or recorded vectors; the telehealth query for an ACME claim returns no evidence.

---

## P2

### FB-13 — Embedding provenance and a re-index job

**Why.** Chunks do not record which embedding model or task-prefix scheme produced their vectors. Changing `EMBEDDING_MODEL` or `EMBED_*_PREFIX` silently leaves old vectors in a different space, and there is no tool to re-embed.

**Do.** Store `embedding_model`, `embedding_prefix_scheme` and dimensions per chunk; the RAG health response flags mismatches; an admin re-index job (per organization, resumable) that re-embeds mismatched chunks; search only compares like with like.

### FB-14 — Knowledge-document expiry alerts and supersession

**Why.** Documents have `effective_date`, `expiration_date` and `version_label`, but nothing warns before a policy expires or links a new version to the one it replaces.

**Do.** A "superseded by" link when uploading a new version (the old one gets an expiration date and is excluded after it); a Knowledge Base filter and dashboard count for "expiring in 30 days" and "expired but still active".

### FB-15 — Denial attachments and appeal packet assembly

**Why.** Appeals need supporting documents (visit notes, authorizations, remittance excerpt), but there is no way to attach files to a denial or produce a reviewable packet. The plan's appeal-drafting guardrail (§35) covers letter text, not the packet or the submission record.

**Do.** Attach files to a denial or worklist item (reuse the object-storage backend); generate a PDF packet (draft letter for human edit, claim/remittance summary, attachment index) that is marked draft until a user approves it; record submission method, date and payer confirmation number. Attachments are PHI: org-scoped, audited on download, covered by retention.

### FB-16 — Structured payer-interaction log

**Why.** `payer_contact` is a resolution type, but calls and portal actions are free-text notes at best. Payer call reference numbers, representative names and promised follow-up dates are what an appeal or escalation later depends on.

**Do.** `payer_interactions(denial, channel, occurred_at, reference_number, representative, summary, follow_up_on, user)`; show them on the denial timeline; put due follow-ups in the digest.

### FB-17 — Worklist ordering by expected recovery

**Why.** The queue sorts by amount, deadline or created date. Specialists get more money back by working first the items that are both valuable and likely to be overturned.

**Do.** Expected recovery = open amount × historical overturn rate for (payer, CARC) from `feedback_loop` (fall back to the CARC-wide rate, then a prior) × a deadline-urgency factor. Add as a sort option and show the components on hover; never hide items.

### FB-18 — Dependency and secret scanning in CI

**Why.** CI builds an SBOM and runs Trivy on images, but runs no `cargo audit`/`cargo deny`, no `npm audit`, and no secret scan. `cargo audit` and gitleaks are installed locally and already clean.

**Do.** Add `cargo audit` (or `cargo deny check advisories licenses`), `npm audit --audit-level=high` for `apps/web`, and gitleaks over the pushed range to `.github/workflows/ci.yml`; document exceptions in `docs/dependency-policy.md`.

### FB-19 — README and operator docs describe the removed Python stack

**Why.** `README.md` currently says embeddings come from Ollama on `:11434`, starts the stack with `docker compose up` (the Python compose file is gone), seeds the admin with `scripts/seed_admin.py` via the Python image, and lists port 3443. The running stack is `docker-compose.rust.yml`, UI on 3444, embeddings from llama.cpp on `:8081`, admin created by `database/docker-initdb.sh`.

**Do.** Rewrite the Quick start, service/model tables and ports for the Rust stack; mention `scripts/seed_test_knowledge.sh` for test data; add a docs check to CI that fails if the README references files that no longer exist.

---

## P3

### FB-20 — NCCI PTP/MUE reference data to explain bundling denials

**Why.** CO-97 (bundled) and CO-151/MUE denials are explained today only by retrieved policy text. CMS publishes the NCCI procedure-to-procedure and medically-unlikely-edit tables quarterly; with them, a bundling denial can name the exact edit pair and whether a modifier is allowed (modifier indicator 0/1).

**Do.** Two new reference list types in `crates/api-gateway/src/routes/reference.rs` (reusing the import/preview/apply flow), and a deterministic check in the analysis that adds "column 1/column 2 pair, modifier indicator" to the evidence for CO-97. Post-denial explanation only; pre-bill scrubbing stays in plan §35.
