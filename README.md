# Denial Navigator

A self-hosted healthcare denial management system. It parses EDI 835 remittance
and 837 claim files, explains each denial with a local LLM grounded in your own
payer policies, and gives billing teams a queue to work the result.

No PHI leaves the deployment. The models run on your hardware, and the
application refuses unauthenticated requests rather than serving them.

**Using it day to day?** See the [wiki](https://git.blacklionit.us/dschmitz/denial_navigator/wiki).
This file covers running and operating it.

## How it fits together

```
 835 / 837 file
       │
       ▼
 ┌──────────────┐     ┌──────────────────────────┐
 │  EDI Parser  │     │  Knowledge base (policy  │
 │  X12 → JSON  │     │  docs → pgvector chunks) │
 └──────┬───────┘     └────────────┬─────────────┘
        │                          │
        ▼                          ▼
 ┌─────────────────────────────────────────────┐
 │        API Gateway — the only writer        │
 │  auth · RBAC · audit · claims · denials     │
 └──────┬──────────────────────────┬───────────┘
        │                          │
        ▼                          ▼
 ┌──────────────┐          ┌───────────────┐
 │  PostgreSQL  │          │  RAG + LLM    │
 │  + pgvector  │          │  reasoning    │
 └──────────────┘          └───────────────┘
        │
        ▼
 ┌─────────────────────────────────────────────┐
 │  React UI — Denials → Appeals / Worklist    │
 └─────────────────────────────────────────────┘
```

Only the API gateway writes to the database. The parser parses, the RAG engine
embeds and retrieves, the LLM service reasons; all three go through the gateway
to persist anything, which is what keeps ingestion idempotent and the audit
trail complete.

## Services

| Service | Port | Purpose |
|---------|------|---------|
| **PostgreSQL** | 5432 | Relational store + pgvector embeddings |
| **API Gateway** | 8000 | REST API, authentication, RBAC, audit |
| **EDI Parser** | 8001 | X12 835/837 parsing, dropzone watcher |
| **RAG Engine** | 8002 | Chunking, embeddings, vector search |
| **LLM Service** | 8003 | Denial reasoning and appeal drafting |
| **Frontend** | 3081 | React billing dashboard |

Two model servers run outside Compose and are configured by URL:

| Backend | Default | Used for |
|---------|---------|----------|
| llama.cpp | `http://10.10.10.98:8080` | Reasoning (`LLM_MODEL`) |
| Ollama | `http://10.10.10.98:11434` | Embeddings (`EMBEDDING_MODEL`) |

They are deliberately separate: the llama.cpp chat server answers
`/v1/embeddings` with `501 does not support embeddings`, so embeddings come
from Ollama, which produces the 768-dimension vectors the schema expects.

## Quick start

```bash
cp .env.example .env
```

Then edit `.env` — at minimum:

```bash
POSTGRES_PASSWORD=...                                    # not the default
JWT_SECRET=$(python3 -c "import secrets; print(secrets.token_urlsafe(48))")
SERVICE_API_KEY=$(python3 -c "import secrets; print(secrets.token_urlsafe(48))")
LLAMA_BASE_URL=http://<your-llama-host>:8080
EMBED_BASE_URL=http://<your-ollama-host>:11434
LLM_MODEL=<whatever your llama.cpp server has loaded>
```

`JWT_SECRET` and `SERVICE_API_KEY` are not optional. Without the first, tokens
are signed with a default string that is in the source; without the second, the
parser and LLM service cannot write their results back through the gateway.

```bash
docker compose up -d --build

# Create the admin user. Idempotent - it skips anything already present.
docker compose run --rm --no-deps -v "$PWD/scripts:/seed" api python /seed/seed_admin.py
```

Open <http://localhost:3081> and sign in as `admin` / `admin123`.
**Change that password immediately** on the My Profile page.

Then drop a file in: Upload tab, or

```bash
curl -X POST http://localhost:8000/api/v1/ingestion/ingest \
  -H "Authorization: Bearer <token>" -F "file=@scripts/sample_835.txt;filename=sample.835"
```

Sample files for a first run: `scripts/sample_835.txt`, `sample_837p.txt`,
`sample_837i.txt`.

## Security

Enforced today:

- **Every `/api` route requires credentials.** Unauthenticated callers get 401.
  Sibling services authenticate with `SERVICE_API_KEY` and are audited under
  their own name, so a machine write is never filed as a person's.
- **Role-based access** by resource and action. An unknown resource denies
  rather than allows, so a route added without a permission rule fails in
  testing instead of leaking.
- **Row-level queue scoping.** A billing specialist sees work assigned to them
  plus the unassigned pool, enforced on lists, single-record reads and writes.
- **Audit logging on every request** — who, which record, from where, and the
  outcome, including refused ones. Written as middleware so a new route cannot
  be missed.
- **bcrypt password hashes**; self-service change requires the current password.

Still your responsibility before production:

- Change the default `admin` and PostgreSQL passwords.
- **Terminate TLS in front of the app.** Tokens and PHI cross the network in
  the clear over plain HTTP.
- Automate PostgreSQL backups, and test a restore.
- Decide audit-log retention — nothing prunes it today.

## Roles

| | Specialist | Manager | Director | Admin |
|---|---|---|---|---|
| Work denials, appeals, worklist | ✅ | ✅ | ✅ | ✅ |
| See other people's queue items | — | ✅ | ✅ | ✅ |
| Assign work | — | ✅ | ✅ | ✅ |
| Upload remittance files | — | ✅ | ✅ | ✅ |
| Manage policy documents | — | ✅ | ✅ | ✅ |
| Read the audit log | — | ✅ | ✅ | ✅ |
| Manage users | — | — | — | ✅ |

Everyone can read claims, denials and policy documents; the differences are in
what they can change. Defined in `api-gateway/services/access.py`.

## Database migrations

`database/migrations/*.sql` runs automatically **only against a fresh volume**,
via the Compose initdb mount. An existing database needs them applied by hand:

```bash
docker exec -i denial-navigator-postgres psql -U denial_nav -d denial_navigator \
  -v ON_ERROR_STOP=1 < database/migrations/001_denial_status_in_progress.sql
```

All migrations are written to be idempotent, so re-running one is safe.

## Air-gapped deployment

The API docs at `/docs` and `/redoc` are served from assets vendored into the
gateway image (`api-gateway/static/docs/`), so they work with no outbound
network. Verify with:

```bash
./scripts/check_docs_offline.sh http://localhost:8000
```

**Image builds still need a network** — `apt-get`, `pip install` and
`npm install`. For a genuinely air-gapped site, build on a connected machine and
transfer the images with `docker save` / `docker load`, or point the package
managers at an internal mirror.

## Development

```bash
cd frontend && npm run smoke      # server-renders every page
./scripts/check_docs_offline.sh   # asserts the docs reference nothing off-host
docker compose logs -f api        # follow a service
```

`npm run smoke` catches what a build cannot: a page that compiles cleanly and
throws the moment it renders. It exists because a temporal-dead-zone read once
shipped the audit page as a blank white screen.

## Layout

```
api-gateway/     REST API — routes/, services/ (auth, access, audit, db)
                 static/docs/  vendored Swagger + ReDoc, for air-gapped use
ediparser/       X12 835/837 parser + dropzone watcher
rag-engine/      chunking, embeddings, pgvector search, prompt building
llm-service/     llama.cpp client, JSON parsing, analysis storage
frontend/        React + Vite; scripts/smoke-render.mjs
database/        init.sql, migrations/, seed/
scripts/         setup, sample EDI files, offline-docs check
docs/            ARCHITECTURE.md
```

## Troubleshooting

| Symptom | Cause |
|---------|-------|
| AI button fails | Check Settings → Service Status. A model server being down shows as *degraded*. |
| Vector search returns nothing | Embeddings need Ollama, not the llama.cpp chat server. Check `EMBED_BASE_URL`. |
| Everything returns 401 | `JWT_SECRET` changed, invalidating existing tokens. Sign in again. |
| Parser results never appear | `SERVICE_API_KEY` mismatch between the gateway and ediparser/llm-service. |
| Docs pages blank | A CDN reference crept back in. Run `scripts/check_docs_offline.sh`. |

## License

Proprietary — internal use only.
