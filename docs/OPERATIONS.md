# Operations guide

## Deployment

Use `docker-compose.rust.yml` for the supported self-contained deployment.
Set every secret in `.env` from a password manager, start the stack, then
check `GET /health/ready` and Settings → Service health. The reverse proxy
serves HTTPS on port 3444; provide a real certificate with `TLS_MODE=provided`
before production use.

**First sign-in.** A new database seeds `admin` with `INIT_ADMIN_PASSWORD`
(default `admin123`). That account, any account an administrator creates or
resets, and any existing account still on `admin123` must choose its own
password (12+ characters) before it can do anything else. The test scripts in
`scripts/` sign in as `ADMIN_USER` / `ADMIN_PASSWORD` (default `admin` /
`admin123`); set them once the admin password has been changed.

The EDI parser runs as a non-root user and polls its mounted `dropzone` volume.
Files are marked processed only after a successful parse/store cycle, making
restarts idempotent. Parsed output retention is controlled by
`PARSED_RETENTION_DAYS`.

An optional SFTP source can be enabled with the `SFTP_*` settings in `.env`.
Use a read-only partner account, set `SFTP_HOST_PUBLIC_KEY_SHA256` to the
partner's pinned host-key fingerprint, and prefer a private key mounted
read-only at `SFTP_PRIVATE_KEY_PATH`. The importer accepts only EDI extensions,
downloads each file into a private temporary path, and sends it through the
same tenant-scoped hash/idempotency path as the watched directory. It leaves
remote files untouched; successful re-polls are safely skipped as duplicates.

An optional S3-compatible importer uses paginated `ListObjectsV2` polling.
Enable it with `S3_IMPORT_ENABLED=true` and set the `S3_IMPORT_*` variables.
Use a separate read-only credential restricted to the configured bucket and
prefix. This importer is independent from the object-store configuration used
for knowledge-source artifacts, so its least-privilege policy remains small.

Uploaded knowledge source artifacts are retained beneath
`OBJECT_STORAGE_LOCAL_PATH` (default `./data/objects`). Place that path on an
encrypted, access-controlled volume; permanent document purges remove its
corresponding source artifact.

## Backup and restore

Run `scripts/backup.sh` from an operator host to create a PostgreSQL dump.
Restore only into an isolated target with `scripts/restore.sh <backup-file>`;
run the synthetic 835 upload and `/health/ready` afterward before promoting a
restore. Keep database, certificate, and object-store backups under the
organization's approved retention/encryption policy.

## Upgrade

1. Back up PostgreSQL and record the current image digests.
2. Pull/build the new release and review `database/migrations/`.
3. Start Compose. Before accepting traffic, the API verifies the SQLx migration
   ledger and applies pending numbered migrations transactionally. Do not run
   historical migration files manually against an existing schema.
4. Verify service health, then run the synthetic 835 and 837
   fixtures. Retain the previous image digest until this verification passes.

## Service health

Settings → System health (`GET /api/v1/system/health`) reports each service. The
two model rows test real function, not just reachability:

- **LLM provider** comes from the recommendation service, which lists the LLM
  server's models with its API key and checks that the model it will request
  (`LLM_MODEL`, or the first served model for `auto`) is served. A wrong model
  name, rejected key or unreachable server shows here with the server's reason.
- **Embedding provider** comes from the retrieval service, which embeds a probe
  string and requires a 768-dimension vector, the size of the index. The result
  is cached for 30 seconds. It reads *Turned off* when `VECTOR_SEARCH_ENABLED=false`.

Either row failing makes the overall status *degraded*: analyses fall back to
deterministic rules and new documents cannot be embedded until it is fixed.

**AI analyses (24 h)** reports what actually happened to the organization's
analyses: how many fell back to deterministic rules or ran without policy
evidence. It turns *degraded* when the three most recent analyses all did, or
when their share over 24 hours reaches `AI_DEGRADED_THRESHOLD` (default 0.2);
AI pages then show a banner until analyses succeed again.

## Write-off approval

Settings → Write-off approval (system or security administrators) sets the
amount at or above which a write-off waits for a revenue cycle manager or
system administrator other than the requester. It defaults to 0, meaning every
write-off needs approval; raise it to let small balances be written off
directly. Managers approve or reject pending write-offs at the top of the
Worklist. The setting is per organization and every change is audit-logged.

## Air-gapped hosts

Build/pull images and package the Rust/frontend dependency caches on a
connected build host. Transfer signed image archives, the release checksum,
the Compose file, migrations, and approved model files by the site's approved
media process. Do not transfer production EDI or payer manuals for testing.
