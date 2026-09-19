# Denial Navigator — Architecture

Denial Navigator is a self-hosted denial-management system for a billing team.
It ingests X12 835 remittance (and 837 submission) files, explains each denial
with a local LLM grounded in the practice's own payer policies, and gives
billing staff a worklist to act on. No PHI leaves the deployment.

This document describes what the system actually does today. Where a design
choice is non-obvious, the reason is given — those are the parts that get
"corrected" back into bugs otherwise.

> **Rewrite in progress.** The backend is being reimplemented in Rust
> (`crates/`, an axum + sqlx workspace). The Python API gateway has been
> removed; the remaining Python services (`ediparser/`, `rag-engine/`,
> `llm-service/`) are still in the tree. The stack runs from
> `docker-compose.rust.yml`. Behaviour, routes, the
> database schema and the wire contract are identical — this document
> describes both, and calls out a difference only where one exists.

---

## Request path

```
        .835 / .837 upload                        browser
                │                                    │
                ▼                                    ▼
        ┌───────────────┐                  ┌────────────────────┐
        │  EDI Parser   │                  │  Frontend (nginx)  │
        │   :8000/int   │                  │  TLS :3443         │
        └───────┬───────┘                  └─────────┬──────────┘
                │ parsed JSON                        │ /api/* proxied
                └──────────────┐        ┌────────────┘
                               ▼        ▼
                     ┌────────────────────────────┐
                     │       API Gateway          │
                     │  auth · RBAC · audit · CRUD│
                     └───┬──────────┬─────────┬───┘
                         │          │         │
              ┌──────────┘          │         └──────────┐
              ▼                     ▼                    ▼
      ┌──────────────┐      ┌──────────────┐    ┌─────────────────┐
      │  RAG Engine  │      │ LLM Service  │    │ PostgreSQL 17   │
      │  retrieval   │─────▶│  reasoning   │    │  + pgvector     │
      └───────┬──────┘      └───────┬──────┘    └─────────────────┘
              │                     │
              ▼                     ▼
   llama.cpp embeddings      llama.cpp chat
   10.10.10.98:8081          10.10.10.98:8080
   nomic-embed-text          Qwen3.x (switchable)
```

Only the frontend publishes a port. Every other service is reachable on the
Docker network alone — the gateway, parser, RAG engine and database are not
bound to a host interface, so the TLS listener on `:3443` is the entire
external surface. (The Rust stack additionally binds the gateway to
`127.0.0.1:18000` for local `curl`; still loopback-only.)

---

## Services

### Frontend (`apps/web/`)

React 19 + Vite, served by nginx, which also terminates TLS and proxies
`/api/*` to the gateway. `TLS_MODE` selects `self-signed` (generated at
container start), `provided` (mount your own cert), or `off` (behind an
existing reverse proxy). The build is unchanged between the two backends — the
app calls relative `/api/v1/...` paths, so only nginx's upstream differs.

Pages: Dashboard, Claims, Denials, Worklist, Appeals, Upload, Knowledge Base,
Insights, Audit, Users, Profile, Settings, Login.

`npm run smoke` renders every page in a headless browser and fails on a console
error. It exists because a temporal-dead-zone bug — a `useState` declared below
the `useEffect` that read it — shipped a white screen that no unit test caught.

### API Gateway (`crates/api-gateway/`)

Everything the browser touches goes through here; the browser never speaks to
another service directly.

| | Python (removed) | Rust |
|---|---|---|
| framework | FastAPI on asyncpg, `--workers 4` | axum 0.8 on sqlx, multi-threaded Tokio |
| auth | PyJWT (HS256), bcrypt cost 12 | `jsonwebtoken` (HS256), `bcrypt` cost 12 |
| TOTP secret encryption | PyFernet | `crates/common/totp.rs` reimplements the Fernet format (AES-128-CBC + HMAC-SHA256) byte-for-byte, so both read and write the same `users.totp_secret` |

Layers, outermost first: **audit → access control → CORS → body limit**. Audit
is outermost deliberately, so a request rejected by access control is still
recorded. Both are Tower middleware; in the Rust build they are
`from_fn_with_state` layers over the whole router.

Routes (`routes/` — 17 modules, mounted under `/api/v1`): `auth`, `claims`,
`denials`, `analyses`, `appeals`, `ingestion`, `knowledge`, `feedback`,
`reference`, `audit`, `users`, `notifications`, `playbooks`, `system`,
`retention`, `settings`, `write-offs`.

The access decision — public paths → identity → MFA confinement → account
currency → role authorisation — is one function
(`rbac::decide` / `AccessControlMiddleware`). It is snapshotted off the
request into an owned struct before the account-currency query, because
`axum`'s request body is `!Sync` and a middleware future may not hold a borrow
across an `.await`.

`reference` maintains the six code lists: CARC and RARC (X12 revises them a
few times a year, the in-house RARC list grows as payers are added), ICD-10
and CPT (updated every October), and HCPCS Level II and modifier codes
(CMS refreshes them with the annual HCPCS update). Import is an upsert on the
code: rows in the file are added or updated, codes absent from the file are
left untouched, and deactivation is only ever explicit via the file's own
status column. A preview (dry run) is always available before an apply, and
every apply is logged to `reference_imports`. Header names are matched by
alias so the official CMS/AMA downloads and hand-typed CSVs both import;
CARC/RARC/modifier uploads are capped at 1 MB, ICD-10/CPT/HCPCS at 50 MB
(the official ICD-10-CM download is ~15 MB). ICD-10, CPT, HCPCS and modifier
lists start empty and are populated by the first import. Lists can also be
searched (code or description, paginated) and mutated row-by-row: individual
codes can be deleted and a whole list cleared, with manager-up write access
and every mutation audit-logged.

`generate_analysis` and the knowledge upload/search endpoints are damped by a
per-caller in-process rate limiter (`ANALYSES_RATE_LIMIT`,
`KNOWLEDGE_RATE_LIMIT`, `INGESTION_RATE_LIMIT`, per minute). The
cross-worker login throttle counts failures out of `audit_log` instead —
an in-process counter with four workers gave an attacker four times the
budget.

**API docs.** `/docs` (Swagger UI), `/redoc` and `/openapi.json` are served
by the gateway, with the JS/CSS vendored under `/static/docs/` — nothing
reaches a CDN, so the docs work air-gapped. The removed FastAPI gateway generated
its schema by reflection; the Rust build has no equivalent, so `crates/api-gateway/openapi/`
holds a hand-maintained document (with a generator script) that is compiled
into the binary.

### EDI Parser (`ediparser/` · `crates/ediparser/`)

Parses X12 835 and 837 into structured JSON. Delimiters are read from the ISA
rather than assumed, and the ISA must be at the start of the file — searching
for the string "ISA" anywhere would happily read delimiters out of a subscriber
name.

A polling watcher picks up files dropped into the dropzone. It only considers
`.835`, `.837`, `.edi` and `.txt`, and waits for a file's size to stop changing
before parsing, so a half-copied upload is not parsed into a silently truncated
claim set. Already-parsed files are seeded from the output directory at
startup, because the processed set is in memory and a restart would otherwise
reparse the entire history.

The watcher is a single background task inside the one service process. The
Python service pins `uvicorn --workers 1` for the same reason: two pollers
over one directory ingest every dropped file twice.

Both directories hold PHI and are pruned on a daily sweep
(`PARSED_RETENTION_DAYS`, default 30). The database is the record; these files
are a working copy.

### RAG Engine (`rag-engine/` · `crates/rag-engine/`)

Chunks policy text, embeds it with `nomic-embed-text` (768-dim) through
llama.cpp's OpenAI-compatible endpoint, and ranks chunks by cosine distance in
pgvector with an **HNSW** index. Embeddings are requested in batches — one
request per chunk made a forty-chunk document forty sequential round trips.

nomic-embed-text is an asymmetric retrieval model: it is trained with
`search_document: ` on stored passages and `search_query: ` on queries, and
retrieves noticeably worse without them. The provider adds the prefixes for any
`nomic-embed*` model and none for other models; `EMBED_DOCUMENT_PREFIX` /
`EMBED_QUERY_PREFIX` override them (`none` disables). The prefixes are part of
the vector space — changing them, or the model, means re-embedding every stored
chunk, or old chunks silently stop matching new queries.

Retrieval is filtered by payer: a denial is argued from documents whose
`payer_name` matches the claim's payer, plus documents with a NULL
`payer_name`, which are payer-agnostic (a CMS LCD, a CPT guideline) and stay in
scope for every denial. Archived documents are excluded, and results below
`MIN_SIMILARITY` are dropped rather than padded out with weak matches.

### LLM Service (`llm-service/` · `crates/llm-service/`)

Builds the denial prompt, calls llama.cpp through the shared OpenAI-compatible
`AiProvider`, parses the JSON answer, and stores prompt, response, model name
and token counts in `ai_analyses` for audit. The gateway selects either its LLM
or deterministic fallback through an internal `RecommendationProvider` boundary;
authorization, tenant scoping, evidence validation, and persistence remain in
the gateway.

`LLM_MODEL=auto` follows whatever model the LLM server currently serves rather
than trusting a name pinned in `.env`: the served name is resolved (and cached
for a minute) and sent on every request. llama-server ignores the `model`
field, but vLLM rejects an unknown name with 404, so a literal `auto` once made
every analysis fall back. A pinned name must be one the server serves; System
health reports it when it is not.

**Degraded analyses are recorded and shown.** `ai_analyses.fallback_reason` is
`llm_error` when the model call failed and deterministic rules were used,
`retrieval_error` when the policy search failed, and `no_evidence` when it
found nothing, so the model answered without evidence. `GET /analyses/status`
reports the organization as degraded when its 3 most recent analyses all have a
reason (an outage now) or when that share over 24 hours reaches
`AI_DEGRADED_THRESHOLD` (default 0.2). The Denials, Worklist, Claims and
Appeals pages then show a banner, each such analysis carries a badge, and
System health has an "AI analyses (24 h)" row.

The health endpoint is bounded to two seconds because the gateway's probe
budget is three; an unbounded probe made a busy model look like a dead service.

### Shared core (`crates/common/`)

`denial_common` is linked by every Rust service so the security-sensitive code
exists once: `config` (fail-fast secret validation), `db` (the sqlx pool),
`auth`, `totp`, `rbac`, `audit`, `ratelimit`, the downstream HTTP `clients`,
the `AppError` → HTTP mapping, and `pgjson` (a `PgRow` → JSON renderer that
also decodes `NUMERIC`, which sqlx will not give you as `f64`).

---

## Data model

Core tables:

| Table | Holds |
|---|---|
| `claims` | one row per claim, with a status rolled up from its denials |
| `denials` | one row per denied service line, with the filing deadline |
| `ai_analyses` | LLM output, prompt, response, model, token counts |
| `appeals_queue` | the operational worklist: appeals *and* non-appeal work |
| `feedback_loop` | whether a recommendation was accepted and whether it paid |
| `write_off_requests` | write-offs held for a manager's approval, and the decision |

Reference: `carc_codes`, `rarc_codes`, `icd10_codes`, `cpt_codes`,
`hcpcs_codes`, `modifier_codes`, `reference_imports`, `knowledge_documents`,
`knowledge_chunks`. Compliance:
`audit_log`, `users`, `ingestion_log`, `notifications`.

Constraints that carry real weight:

- A **partial unique index** on `appeals_queue(denial_id)` over open rows. The
  application also checks before inserting, but check-and-insert is two round
  trips; the index is what actually prevents two clicks creating two queue
  items, and the insert catches its violation and returns the same 409.
- `ai_analyses.claim_id` has a foreign key to `claims`, like its siblings.
- `appeal_deadline_for(payer, remit_date)` computes filing deadlines in SQL, so
  ingestion, backfill and recompute cannot disagree.
- **Write-offs need a second person above a threshold.** Writing a denial off,
  whether by closing a `write_off` worklist item or setting the denial to
  `written_off`, goes through `routes/write_offs.rs::gate`. At or above the
  organization's `write_off_approval_threshold` (0 = always) nothing changes:
  the API answers 202 `pending_approval` and records a `write_off_requests` row
  (one pending per denial, by partial unique index). A revenue cycle manager or
  system administrator approves or rejects it under `/api/v1/write-offs`; a
  `CHECK` and the handler both refuse a decision by the requester. Approval
  writes the denial off and closes its worklist item; every step is audited.
  `scripts/test_write_off_approval.sh` covers it end to end.

Schema changes live in `database/migrations/`, numbered and managed by SQLx.
`init.sql` remains the complete schema snapshot for fresh Compose volumes. On
first API startup, the gateway verifies that snapshot, records the historical
migrations in SQLx's ledger, and then applies any later numbered migrations.
This avoids replaying intermediate historical scripts over the current schema
while retaining checksum-verified, transactional upgrades going forward.

---

## Security

**Authentication.** JWT (HS256), bcrypt cost 12, optional TOTP with
Fernet-encrypted secrets. Sessions are revocable: `users.sessions_valid_from`
is compared against the token's `iat`, and it is stored via
`date_trunc('second', NOW())` because `iat` truncates to whole seconds — a
sub-second timestamp revoked every token the instant it was issued.

**Passwords someone else chose are replaced at first sign-in.**
`users.must_change_password` is set for the seeded admin, for accounts an
administrator registers, after an administrator reset, and (migration 037) for
any account still on the documented default. Sign-in then returns only a
10-minute token with scope `password_change`, which the middleware confines to
`POST /auth/change-password` and `GET /auth/me`; TOTP, when required, comes
first, so a leaked default password alone cannot set a new one. New passwords
must be at least 12 characters, must not contain the username or be a common
password (`crates/auth/src/password.rs`), and must differ from the current one.

**Authorisation.** Roles are `system_admin`, `security_admin`,
`revenue_cycle_manager`, `billing_specialist`, `coding_specialist`, `auditor`,
and `read_only`, enforced by resource/path permission tables and, for
specialists, by row-level queue scoping. The role is baked into the token, so
every request re-checks it against the account: a demotion takes effect
immediately rather than at the next login. Changing a user's role also bumps
`sessions_valid_from`. This account-currency check is one indexed primary-key
lookup per request, deliberately not cached — a cache TTL is exactly the
window in which a revoked session still works.

**Service credentials.** `EDIPARSER_SERVICE_API_KEY` and
`LLM_SERVICE_API_KEY` authenticate their respective callbacks through
`X-Service-Key`, compared in constant time. The gateway derives the service
identity from the key, ignores caller-supplied service names, and limits each
service to its one callback route. Private parser, RAG, and LLM endpoints use
separate per-target internal credentials as well.

**Login throttling** counts failures out of `audit_log`, not in process memory,
because with multiple workers an in-process counter gives an attacker several
times the stated budget. TOTP verification is throttled the same way.

**Client IP** is taken from `X-Real-IP`, falling back to the *right-most*
`X-Forwarded-For` entry, and **only** when the request actually arrived from a
trusted proxy. `TRUSTED_PROXY_NETWORKS` is a comma-separated list of CIDRs (or
bare addresses); an entry that does not parse is dropped, and if the list ends
up empty the forwarded headers are ignored entirely and the peer address is
used.

**Audit.** Every action against a claim is recorded with actor, IP, user agent
and the affected record's *claim number*, not just its UUID. The UI renders
entries in plain English. The audit layer sits outside access control, so
401/403 attempts are logged too.

**Containers** run as a non-root user. PHI never leaves the deployment: no
outbound calls, and the models run on the operator's own llama.cpp host. Rust
services fail to start if their required credentials or `TOTP_FERNET_KEY` is
unset, a known placeholder, or (for Fernet) not an exact 32-byte key.

---

## Reliability

- **Ingestion is one transaction.** A failure partway through used to leave the
  file logged as `completed` with only some of its claims written — and because
  the file hash was already recorded, the retry was refused as a duplicate.
- **835 upserts never overwrite a non-zero charge with zero.** A remittance
  reports what was paid, not always what was billed.
- **Duplicate denials are skipped, not re-inserted**, and the reported count
  comes from `RETURNING`, so "offered" and "stored" are separate numbers. A
  payer re-sending a remittance with a new production date is still a
  duplicate: an active denial with the same claim, line, procedure, group,
  CARC and amounts is never inserted again.
- **Reprocessed claims settle their denials** (`crates/api-gateway/src/reprocessing.rs`).
  A reversal (CLP02 `22`) marks the claim `reversed_at` and is not stored as a
  claim, so its negated totals never overwrite it and the corrected loop that
  follows keeps the real claim number; its CAS lines are not denials. When a
  later remittance pays a claim or line that has an active denial from an
  earlier file, and no longer applies that group and CARC there, the denial
  closes itself: `overruled` if it was under appeal, else `resolved`, with
  `resolution_source = 'remittance'`, the recovered amount, an audit entry, and
  its open worklist items closed. Recovery analytics count these as recovered.
  `scripts/test_reprocessing.sh` exercises the whole sequence.
- **Bulk queueing resolves the whole batch in one query** instead of three per
  denial, and reports partial success rather than failing the batch.
- **The connection pool** is created once at startup with a liveness check;
  there is no per-request connect.
- **PDF policy uploads** run text extraction on a blocking thread inside
  `catch_unwind` — a malformed PDF returns 400, it does not take the worker
  down — and a scan with no extractable text is rejected rather than indexed
  as an empty document.

---

## Operations

```bash
./scripts/setup.sh                     # first run (Python stack)

# Python stack (default)
docker compose ps
docker compose logs -f <service>

# Rust stack (parallel; distinct ports and volumes)
printf 'TOTP_FERNET_KEY=%s\n' "$(openssl rand -base64 32)" >> .env   # once
docker compose -f docker-compose.rust.yml up -d --build
#   UI  https://localhost:3444/     API  http://127.0.0.1:18000/
#   docs at https://localhost:3444/docs
docker compose -f docker-compose.rust.yml down        # keep data
docker compose -f docker-compose.rust.yml down -v     # drop data

./scripts/backup.sh                    # verified pg_dump
./scripts/restore.sh <file>
./scripts/eval_model.py                # score a model against known denials
./scripts/send_deadline_digests.sh     # filing-deadline notifications
```

For fast local iteration on the Rust services without rebuilding images,
`.rust-stack/` runs the four release binaries on the host against a throwaway
database (`.rust-stack/up.sh` / `down.sh`).

Migrations (Python stack; the Rust `docker-initdb.sh` does this automatically):

```bash
docker exec -i denial-navigator-postgres \
  psql -U denial_nav -d denial_navigator -v ON_ERROR_STOP=1 \
  < database/migrations/0NN_name.sql
```

Health: `GET /api/v1/system/health` reports every dependency with latency, and
distinguishes essential services from optional ones.

---

## Layout

```
denial-navigator/
├── docker-compose.yml              Python stack (default)
├── docker-compose.rust.yml         Rust stack (parallel)
├── Dockerfile.rust                 multi-target build for all four Rust services
├── Cargo.toml / Cargo.lock         Rust workspace
├── database/
│   ├── init.sql                    schema, views, triggers
│   ├── migrations/                 numbered, idempotent
│   ├── seed/                       CARC/RARC reference data
│   └── docker-initdb.sh            fresh-container bootstrap
├── crates/
│   ├── common/                     config, db, auth, totp, rbac, audit,
│   │                               ratelimit, clients, error, pgjson
│   ├── api-gateway/
│   │   ├── src/routes/             14 route modules
│   │   ├── src/{middleware,docs,state}.rs
│   │   ├── openapi/                hand-maintained OpenAPI + generator
│   │   └── static/docs/            vendored Swagger UI + ReDoc (air-gapped)
│   ├── ediparser/                  x835, x837, schema, watch
│   ├── rag-engine/                 chunk, embed, prompt
│   └── llm-service/                llama client
├── ediparser/ rag-engine/ llm-service/
│                                   the remaining Python services
├── apps/web/
│   ├── src/pages/                  13 pages
│   ├── src/lib/                    authFetch, auditText
│   └── scripts/smoke-render.mjs    npm run smoke
├── .rust-stack/                    host-run rig for local iteration
├── scripts/
└── docs/
```
