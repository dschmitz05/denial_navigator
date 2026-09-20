# Denial Navigator

A self-hosted healthcare denial management system. It parses EDI 835 remittance
and 837 claim files, explains each denial with a local LLM grounded in your own
payer policies, and gives billing teams a queue to work the result.

No PHI leaves the deployment. The models run on your hardware, and the
application refuses unauthenticated requests rather than serving them.

**Using it day to day?** See the [wiki](https://git.blacklionit.us/dschmitz/denial_navigator/wiki).
This file covers running and operating it. Operational detail beyond a first
run — upgrades, air-gapped hosts, importer configuration — lives in
[docs/OPERATIONS.md](docs/OPERATIONS.md); how the pieces fit together and why
lives in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

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
trail complete. All four are Rust services built from one workspace
(`crates/`) into one image, `Dockerfile.rust`, each run as a separate target.

## Services

`docker-compose.rust.yml` is the only supported deployment file.

| Service | Port | Purpose |
|---------|------|---------|
| **PostgreSQL** | *internal* | Relational store + pgvector embeddings |
| **API Gateway** | 127.0.0.1:18000 | REST API, authentication, RBAC, audit |
| **EDI Parser** | *internal* | X12 835/837 parsing, dropzone watcher |
| **RAG Engine** | *internal* | Chunking, embeddings, vector search |
| **LLM Service** | *internal* | Denial reasoning and appeal drafting |
| **Frontend** | 3444 (HTTPS) | React billing dashboard — 3082 redirects here |

The API gateway's port is loopback-only and meant for scripting against the
running host; the browser always goes through the frontend. Two optional
services are gated behind Compose profiles and are not started by default:
`minio` (`--profile object-storage`, an S3-compatible store for
`OBJECT_STORAGE_BACKEND=s3` in development) and `keycloak`
(`--profile oidc`, a development-only OIDC issuer — production must use your
organization's managed identity provider).

Two model servers run outside Compose and are configured by URL:

| Backend | Default | Used for |
|---------|---------|----------|
| llama.cpp | `LLAMA_BASE_URL` | Reasoning (`LLM_MODEL`) |
| llama.cpp (embeddings) | `EMBED_BASE_URL` | Embeddings (`EMBEDDING_MODEL`) |

They are deliberately separate servers: the chat server answers `/v1/embeddings`
with `501 does not support embeddings`, so a second llama.cpp instance running
an embedding model (`EMBED_BASE_URL`) produces the 768-dimension vectors the
schema expects.

## Quick start

```bash
cp .env.example .env
```

Then edit `.env` — at minimum:

```bash
POSTGRES_PASSWORD=...                                       # not the default
JWT_SECRET=$(openssl rand -base64 48)
TOTP_FERNET_KEY=$(openssl rand -base64 32)
EDIPARSER_SERVICE_API_KEY=$(openssl rand -base64 32)
LLM_SERVICE_API_KEY=$(openssl rand -base64 32)
EDIPARSER_INTERNAL_API_KEY=$(openssl rand -base64 32)
RAG_INTERNAL_API_KEY=$(openssl rand -base64 32)
LLM_INTERNAL_API_KEY=$(openssl rand -base64 32)
LLAMA_BASE_URL=http://<your-llama.cpp-host>:8080
EMBED_BASE_URL=http://<your-embedding-host>:8081
LLM_MODEL=<whatever your llama.cpp server has loaded>
```

None of the seven secrets above are optional — each service checks its own at
startup and **refuses to start** rather than run with the placeholder value
`.env.example` ships, so a deployment can't go live on a default key by
accident.

```bash
docker compose -f docker-compose.rust.yml up -d --build
```

The database bootstraps itself on first start (`database/docker-initdb.sh`):
schema, CARC/RARC reference data, and a default admin,
`admin` / `${INIT_ADMIN_PASSWORD:-admin123}`.

Open <https://localhost:3444> and sign in as `admin` / `admin123`. That account
is flagged to require a new password before it can do anything else — you'll
be prompted immediately. Choose one at least 12 characters.

The first start generates a **self-signed** certificate, so your browser will
warn. That is expected and correct — the connection is encrypted but not
authenticated. See [TLS](#tls) to install a trusted one.

Then drop a file in: the Upload tab, or

```bash
TOKEN=$(curl -fsS -H 'Content-Type: application/json' \
  --data '{"username":"admin","password":"<your new password>"}' \
  http://127.0.0.1:18000/api/v1/auth/login | jq -r .access_token)

curl -X POST http://127.0.0.1:18000/api/v1/ingestion/ingest \
  -H "Authorization: Bearer $TOKEN" \
  -F "file=@scripts/sample_835.txt;filename=sample.835"
```

Sample files for a first run: `scripts/sample_835.txt`,
`scripts/sample_837p.txt`, `scripts/sample_837i.txt`. A larger synthetic set —
denials, remittances and matching knowledge-base policies to explain them —
comes from `scripts/generate_synthetic_835.py` and
`scripts/seed_test_knowledge.sh`; `scripts/test_synthetic_e2e.sh` runs the
whole API path end to end and checks what came out. With that synthetic stack
running, `cd apps/web && E2E_PASSWORD=... npm run e2e` also verifies login,
the browser denial queue, and the PAT001 denial detail.

## Security

Enforced today:

- **Every `/api` route requires credentials.** Unauthenticated callers get 401.
  Sibling services authenticate with their own `*_SERVICE_API_KEY` and are
  audited under their own name, so a machine write is never filed as a
  person's.
- **Role-based access** by resource and action, defined in
  `crates/auth/src/rbac.rs`. An unknown resource denies rather than allows, so
  a route added without a permission rule fails in testing instead of leaking.
- **Row-level queue scoping.** A billing specialist sees work assigned to them
  plus the unassigned pool, enforced on lists, single-record reads and writes.
- **Audit logging on every request** — who, which record, from where, and the
  outcome, including refused ones. Written as middleware so a new route cannot
  be missed.
- **bcrypt password hashes** (cost 12, unique per-password salt); self-service
  change requires the current password. A password set by someone else — a
  new account, an administrator's reset, or the seeded default — must be
  replaced before the account can do anything else.
- **Recommendation consistency checks.** A denial the payer explained how to
  fix is never offered as a write-off, and a PR (patient responsibility)
  balance is never offered as one either — both are collectible. The analysis
  is stored as written; the disagreement is shown, not applied silently.
- **Optional TOTP two-factor**, per account, controlled by an administrator.
  Secrets are encrypted at rest, codes are single-use, and an admin can reset a
  lost device without ever seeing the secret.
- **Sessions are revocable.** Deactivating an account, resetting its password or
  deleting it ends its existing sessions immediately, rather than leaving them
  valid for the rest of the token's life.
- **TLS by default.** The application is served over HTTPS on first start, with
  HTTP redirecting to it. See below.
- **Only the web listener is published.** PostgreSQL and the internal services
  are reachable on the Docker network only; the API gateway is bound to
  loopback for scripting.

Still your responsibility before production:

- Change the default `admin` and PostgreSQL passwords, and set every secret
  listed under [Quick start](#quick-start) to a real value — the services
  won't start on the placeholders, but nothing stops you from generating weak
  ones.
- **Install a trusted certificate.** The default is self-signed: encrypted, not
  authenticated.
- **Encrypt the database volume.** PHI is stored unencrypted at rest —
  `ai_analyses` holds raw prompts containing patient data. Use LUKS, an
  encrypted ZFS dataset, or an encrypted EBS volume; the application cannot do
  this for you.
- Schedule `scripts/backup.sh` and verify a restore periodically.

## Backups

```bash
./scripts/backup.sh                                   # nightly, from cron
./scripts/restore.sh <backup.sql.gz> --verify-only    # prove it restores
```

The backup script refuses to keep a dump it cannot read back, or one with
implausibly few tables — a corrupt file that looks like a backup is worse than
no file. `--verify-only` restores into a scratch database and prints row
counts, so you can test a restore without touching live data. Dumps are written
`chmod 600` because they contain PHI.

Suggested cron entry:

```
0 2 * * * cd /path/to/denial-navigator && ./scripts/backup.sh >> /var/log/dn-backup.log 2>&1
```

## Deadline notifications

Filing deadlines only helped whoever opened the dashboard. `scripts/send_deadline_digests.sh`
builds them into alerts that arrive:

- **Per owner** — what of *their* queue is overdue or due within
  `DEADLINE_DIGEST_DAYS` (default 14, matching the dashboard's window so the
  two cannot say different things).
- **To managers** — anything overdue with nobody assigned, because an unowned
  overdue denial has no one to chase it.

Delivered in-app (a bell in the sidebar) rather than by email: an air-gapped
deployment may have no mail path, and a notification that silently fails to
send is worse than one waiting to be read.

```
0 7 * * 1-5 cd /path/to/denial-navigator && ./scripts/send_deadline_digests.sh >> /var/log/dn-digests.log 2>&1
```

Generation is idempotent per user per day — enforced by a unique index, not by
assuming the script runs once — so a retry after a failure sends no duplicates.

## AI Insights

**AI Insights** answers whether the model is earning its place: success rate by
payer, by recommended action and by denial reason, month over month, and the
money actually recovered.

Every rate divides by outcomes that are *known* — work still in flight is
excluded rather than counted as failure — and the page says plainly when there
are too few outcomes to conclude anything, rather than showing a confident
percentage over four data points.

Retrieval — whether the AI is finding the *right* policy, not just answering
confidently — is measured separately with `scripts/eval_retrieval.py` against
a labelled query set (`scripts/fixtures/retrieval_eval.json`); see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for what it measures and why the
evidence cut is calibrated the way it is.

## Audit retention

The audit log grows without limit. `AUDIT_RETENTION_DAYS` defaults to **2190
(six years)**, matching HIPAA's documentation retention expectation, and
nothing is ever deleted automatically.

```
GET  /api/v1/retention/audit         # size, age, how much is beyond the window
POST /api/v1/retention/audit/prune   # admin only, confirm=true, minimum 1 year
```

A prune records itself as an audit entry naming the range it removed — that
entry is the only remaining evidence the history existed.

## TLS

Set `TLS_MODE` in `.env`:

| Mode | Behaviour |
|------|-----------|
| `self-signed` *(default)* | Generates a certificate on first start if `certs-rust/` is empty. Encrypted but **not authenticated** — browsers warn. |
| `provided` | Uses `certs-rust/tls.crt` + `certs-rust/tls.key`. **Refuses to start** if they are missing, rather than silently falling back. |
| `off` | Plain HTTP only. Valid **only** when something else terminates TLS over a trusted hop — a proxy on the same host or Docker network. Across a VLAN this puts PHI on the wire in clear. |

To install a real certificate:

```bash
cp your-cert.pem certs-rust/tls.crt     # server cert, then any intermediates
cp your-key.pem  certs-rust/tls.key
sed -i 's/^TLS_MODE=.*/TLS_MODE=provided/' .env
docker compose -f docker-compose.rust.yml restart frontend
```

Set `TLS_HOSTNAME` to the name staff actually type — a certificate issued for
the wrong name makes the browser warning worse, not better. `PUBLIC_HTTPS_PORT`
only shapes the HTTP→HTTPS redirect; compose sets it to `3444` to match the
published port, or set it to `443` if you publish there instead.

HSTS is deliberately **not** enabled. With a self-signed certificate it would
pin browsers to a certificate they do not trust, and undoing that means
clearing state on every client machine. Turn it on once a trusted certificate
is in place.

### Behind an existing reverse proxy

Point your proxy at the frontend's HTTPS listener (3444) and let it verify or
skip verification as your policy requires. If the proxy runs on the same host
or Docker network, `TLS_MODE=off` with plain HTTP on 3082 is a reasonable
simplification — but not across a network segment.

## Roles

| | Specialist | Coding | Manager | Auditor | Security admin | System admin |
|---|---|---|---|---|---|---|
| Work denials, appeals, worklist | ✅ | ✅ | ✅ | — | — | ✅ |
| See other people's queue items | — | — | ✅ | ✅ | ✅ | ✅ |
| Assign work | — | — | ✅ | — | — | ✅ |
| Upload remittance files, manage policy documents | — | — | ✅ | — | — | ✅ |
| Read the audit log | — | — | ✅ | ✅ | ✅ | ✅ |
| Manage users, security settings | — | — | — | — | ✅ | ✅ |

Everyone (plus `read_only`) can read claims, denials and policy documents; the
differences are in what they can change. The seven roles are
`billing_specialist`, `coding_specialist`, `revenue_cycle_manager`, `auditor`,
`read_only`, `security_admin` and `system_admin`, defined in
`crates/auth/src/rbac.rs`.

## Appeal filing windows

The dashboard's Priority Denials panel warns about claims approaching their
filing deadline. An 835 does not carry that deadline — a remittance states the
payer's adjudication, not your window to contest it — so the window is
configuration.

`payer_appeal_policies` holds one row per payer plus a `*` default of **90
days**, and `appeal_deadline_for()` applies it to the remittance date the
parser derives. Ingestion, the backfill and the recompute all call that one
function, so they cannot drift apart.

Edit them under **Settings → Appeal filing windows** (managers and above).
Saving re-dates the *open* denials that payer governs and reports how many
moved; closed denials are never re-dated.

Payers vary from 60 to 365 days, so any payer showing *"using the default"* is
being governed by a number nobody chose for it. Payers also run other clocks —
timely filing, corrected claims, reconsiderations, second-level appeals — set
separately under **Settings → Payer deadlines**; see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Database migrations

The API applies `database/migrations/*.sql` with SQLx before it serves traffic.
An empty database starts from `database/init.sql`; the first API startup records
the checked-in historical migrations as a verified baseline. After that, SQLx
applies only new numbered files and verifies their checksums. Do not manually
replay historical migration files over an existing schema.

## Air-gapped deployment

The API docs at `/docs` and `/redoc` are served from assets vendored into the
gateway image (`crates/api-gateway/static/docs/`), so they work with no outbound
network. Verify with:

```bash
./scripts/check_docs_offline.sh http://localhost:18000
```

**Image builds still need a network** — `cargo build` and `npm install` both
fetch dependencies. For a genuinely air-gapped site, build on a connected
machine and transfer the images with `docker save` / `docker load`, or point
the package managers at an internal mirror. See
[docs/OPERATIONS.md](docs/OPERATIONS.md#air-gapped-hosts) for the full
transfer process.

## Development

```bash
cd apps/web && npm run smoke      # server-renders every page
cd apps/web && npm run typecheck  # checks generated and converted TypeScript
./scripts/check_docs_offline.sh   # asserts the docs reference nothing off-host
./scripts/check_docs_paths.py     # asserts README.md references nothing that doesn't exist
docker compose -f docker-compose.rust.yml logs -f api   # follow a service
```

`npm run smoke` catches what a build cannot: a page that compiles cleanly and
throws the moment it renders. It exists because a temporal-dead-zone read once
shipped the audit page as a blank white screen.

Each backlog feature has an end-to-end script under `scripts/test_*.sh` that
runs against the live stack and cleans up after itself —
`scripts/test_deadlines.sh`, `scripts/test_payer_retrieval.sh`,
`scripts/test_organization_isolation.sh`, and so on. Run one directly, or all
of them, once the stack and its test data are up:

```bash
ADMIN_USER=admin ADMIN_PASSWORD=<your admin password> ./scripts/test_deadlines.sh
```

## Layout

```
crates/          Rust workspace — api-gateway, ediparser, rag-engine,
                 llm-service, auth, common; api-gateway/static/docs/ holds
                 vendored Swagger + ReDoc for air-gapped use
apps/web/        React + Vite TypeScript app; scripts/smoke-render.mjs
database/        init.sql, migrations/, seed/, docker-initdb.sh
Dockerfile.rust  multi-target build for all four Rust services
scripts/         backups, deadline digests, sample EDI files, test_*.sh
                 end-to-end checks, retrieval evaluation, offline-docs check
docs/            ARCHITECTURE.md, OPERATIONS.md, and the rest
```

## Troubleshooting

| Symptom | Cause |
|---------|-------|
| AI button fails | Check Settings → Service health. A model server being down shows here as *degraded*. |
| Vector search returns nothing | Check `EMBED_BASE_URL` — it must point at an embedding-capable llama.cpp server, not the chat one. |
| Everything returns 401 | `JWT_SECRET` changed, invalidating existing tokens. Sign in again. |
| Parser or LLM results never appear | A `*_SERVICE_API_KEY` / `*_INTERNAL_API_KEY` mismatch between the gateway and the service; check its container logs for a 401 on the callback. |
| Docs pages blank | A CDN reference crept back in. Run `scripts/check_docs_offline.sh`. |
| A service won't start at all | Check its logs for `FATAL: ... is not set to a real value` — one of the required secrets is still the `.env.example` placeholder. |

## License

Proprietary — internal use only.
