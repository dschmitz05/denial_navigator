# Denial Navigator — Architecture

Denial Navigator is a self-hosted denial-management system for a billing team.
It ingests X12 835 remittance (and 837 submission) files, explains each denial
with a local LLM grounded in the practice's own payer policies, and gives
billing staff a worklist to act on. No PHI leaves the deployment.

This document describes what the system actually does today. Where a design
choice is non-obvious, the reason is given — those are the parts that get
"corrected" back into bugs otherwise.

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
external surface.

---

## Services

### Frontend (`frontend/`)

React 19 + Vite, served by nginx, which also terminates TLS and proxies
`/api/*` to the gateway. `TLS_MODE` selects `self-signed` (generated at
container start), `provided` (mount your own cert), or `off` (behind an
existing reverse proxy).

Pages: Dashboard, Claims, Denials, Worklist, Appeals, Upload, Knowledge Base,
Insights, Audit, Users, Profile, Settings, Login.

`npm run smoke` renders every page in a headless browser and fails on a console
error. It exists because a temporal-dead-zone bug — a `useState` declared below
the `useEffect` that read it — shipped a white screen that no unit test caught.

### API Gateway (`api-gateway/`)

FastAPI on asyncpg, `--workers 4`. Everything the browser touches goes through
here; the browser never speaks to another service directly.

Middleware, outermost first: `AuditMiddleware` → CORS → `AccessControlMiddleware`.
Audit is outermost deliberately, so a request rejected by access control is
still recorded.

Routes: `claims`, `denials`, `appeals`, `analyses`, `knowledge`, `ingestion`,
`feedback`, `audit`, `auth`, `users`, `notifications`, `retention`, `system`,
`reference`.

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

### EDI Parser (`ediparser/`)

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

Both directories hold PHI and are pruned on a daily sweep
(`PARSED_RETENTION_DAYS`, default 30). The database is the record; these files
are a working copy.

### RAG Engine (`rag-engine/`)

Chunks policy text, embeds it with `nomic-embed-text` (768-dim) through
llama.cpp's OpenAI-compatible endpoint, and ranks chunks by cosine distance in
pgvector with an **HNSW** index. Embeddings are requested in batches — one
request per chunk made a forty-chunk document forty sequential round trips.

Retrieval is filtered by payer: a denial is argued from documents whose
`payer_name` matches the claim's payer, plus documents with a NULL
`payer_name`, which are payer-agnostic (a CMS LCD, a CPT guideline) and stay in
scope for every denial. Archived documents are excluded, and results below
`MIN_SIMILARITY` are dropped rather than padded out with weak matches.

### LLM Service (`llm-service/`)

Builds the denial prompt, calls llama.cpp, parses the JSON answer, and stores
prompt, response, model name and token counts in `ai_analyses` for audit.

`LLM_MODEL=auto` follows whatever model llama.cpp currently has loaded rather
than trusting a name pinned in `.env`. The host runs one `llama-server` at a
time and systemd swaps the model; llama.cpp ignores the `model` field in a
request, so a stale name never breaks a call — it just records the wrong model
against the analysis, which is the one field the audit trail must not lie about.

The health endpoint is bounded to two seconds because the gateway's probe
budget is three; an unbounded probe made a busy model look like a dead service.

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

Schema changes live in `database/migrations/`, numbered, and every one is
idempotent — they are applied by re-running, not by a version table.

---

## Security

**Authentication.** JWT (HS256, PyJWT), bcrypt cost 12, optional TOTP with
Fernet-encrypted secrets. Sessions are revocable: `users.sessions_valid_from`
is compared against the token's `iat`, and it is stored via
`date_trunc('second', NOW())` because `iat` truncates to whole seconds — a
sub-second timestamp revoked every token the instant it was issued.

**Authorisation.** Roles are `billing_specialist`, `billing_manager`,
`rcm_director`, `admin`, enforced by `PERMISSIONS`/`PATH_PERMISSIONS` and, for
specialists, by row-level queue scoping. The role is baked into the token, so
every request re-checks it against the account: a demotion takes effect
immediately rather than at the next login. Changing a user's role also bumps
`sessions_valid_from`.

**Service credentials.** `SERVICE_API_KEY` authenticates service-to-service
calls through `X-Service-Key` only. It is not accepted as a user credential —
when it was, a leaked key could act as whatever user the request claimed.

**Login throttling** counts failures out of `audit_log`, not in process memory,
because with `--workers 4` an in-process counter gives an attacker four times
the stated budget. TOTP verification is throttled the same way.

**Client IP** is taken from `X-Real-IP`, falling back to the *right-most*
`X-Forwarded-For` entry. nginx appends with `$proxy_add_x_forwarded_for`, so
the left-most entry is whatever the client sent — checking only the peer
address is not enough to make the left-most entry trustworthy.

**Audit.** Every action against a claim is recorded with actor, IP, user agent
and the affected record's *claim number*, not just its UUID. The UI renders
entries in plain English.

**Containers** run as a non-root user. PHI never leaves the deployment: no
outbound calls, and the models run on the operator's own llama.cpp host.

---

## Reliability

- **Ingestion is one transaction.** A failure partway through used to leave the
  file logged as `completed` with only some of its claims written — and because
  the file hash was already recorded, the retry was refused as a duplicate.
- **835 upserts never overwrite a non-zero charge with zero.** A remittance
  reports what was paid, not always what was billed.
- **Duplicate denials are skipped, not re-inserted**, and the reported count
  comes from `RETURNING`, so "offered" and "stored" are separate numbers.
- **Bulk queueing resolves the whole batch in one query** instead of three per
  denial, and reports partial success rather than failing the batch.
- **The connection pool** is checked with the public `is_closing()`, and is
  created lazily on first use; there is no lifespan handler.

---

## Operations

```bash
./scripts/setup.sh                     # first run
docker compose ps                      # service state
docker compose logs -f <service>
./scripts/backup.sh                    # verified pg_dump
./scripts/restore.sh <file>
./scripts/eval_model.py                # score a model against known denials
./scripts/send_deadline_digests.sh     # filing-deadline notifications
./scripts/check_docs_offline.sh        # docs must work air-gapped
```

Migrations:

```bash
docker exec -i denial-navigator-postgres \
  psql -U denial_nav -d denial_navigator -v ON_ERROR_STOP=1 \
  < database/migrations/0NN_name.sql
```

Health: `GET /api/v1/system/health` reports every dependency with latency, and
distinguishes essential services from optional ones.

API docs are fully self-contained — Swagger UI and ReDoc assets are vendored,
not pulled from a CDN, because this is expected to run air-gapped.

---

## Layout

```
denial-navigator/
├── docker-compose.yml
├── database/
│   ├── init.sql                    schema, views, triggers
│   ├── migrations/                 numbered, idempotent
│   └── seed/                       CARC/RARC reference data
├── ediparser/
│   ├── main.py                     FastAPI + retention sweep
│   ├── parser/                     x12_parser (835), x12_837, x12_common,
│                                   schema
│   └── watch/watcher.py            dropzone poller
├── rag-engine/
│   ├── main.py                     chunk, embed, vector search
│   └── prompts/denial_analysis.py
├── llm-service/main.py
├── api-gateway/
│   ├── main.py
│   ├── routes/                     14 route modules
│   └── services/                   db, audit, access, totp, ratelimit,
│                                   claim_status, notifications
├── frontend/
│   ├── src/pages/                  13 pages
│   ├── src/lib/                    authFetch, auditText
│   └── scripts/smoke-render.mjs    npm run smoke
├── scripts/
└── docs/
```
