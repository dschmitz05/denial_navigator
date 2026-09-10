#!/bin/sh
# Fresh-container database bootstrap, mounted as
# /docker-entrypoint-initdb.d/00-run-all.sh. The stock postgres entrypoint
# cannot run a mounted directory, which is why this exists.
#
# Order: init.sql (the base schema) -> numbered migrations -> reference seed.
# init.sql already contains the effect of migrations 001-010, so those replay
# as harmless "already exists" errors; 011-013 add the reference-code tables
# init.sql still lacks and DO apply. Every migration therefore runs
# best-effort - a failure is logged and skipped, never fatal, because the
# postgres entrypoint aborts the whole init if this script exits non-zero.
#
# A default admin is created so the API is usable immediately.
set -e

DB="${POSTGRES_DB:-denial_navigator}"
DBUSER="${POSTGRES_USER:-denial_nav}"
SRC=/opt/dn-sql
run() { psql -v ON_ERROR_STOP=1 -U "$DBUSER" -d "$DB" "$@"; }
soft() { psql -U "$DBUSER" -d "$DB" "$@" 2>&1 || true; }

echo "[initdb] schema (init.sql)"
run -f "$SRC/init.sql"

echo "[initdb] migrations (best-effort)"
for f in "$SRC"/migrations/*.sql; do
    [ -f "$f" ] || continue
    echo "[initdb]   $(basename "$f")"
    soft -f "$f" | sed 's/^/[initdb]     /'
done

echo "[initdb] reference seed"
if [ -f "$SRC/seed/carc_codes.sql" ]; then run -f "$SRC/seed/carc_codes.sql"; fi
if [ -f "$SRC/seed/rarc_codes.sql" ]; then run -f "$SRC/seed/rarc_codes.sql"; fi
if [ -f "$SRC/seed/sample_data.sql" ]; then
    echo "[initdb] sample data (best-effort; stale against current schema)"
    soft -f "$SRC/seed/sample_data.sql" >/dev/null
fi

echo "[initdb] default admin  (admin / ${INIT_ADMIN_PASSWORD:-admin123})"
run <<SQL
CREATE EXTENSION IF NOT EXISTS pgcrypto;
INSERT INTO users (username, email, password_hash, full_name, role, is_active)
VALUES ('admin', 'admin@denialnavigator.local',
        crypt('${INIT_ADMIN_PASSWORD:-admin123}', gen_salt('bf', 12)),
        'System Administrator', 'admin', TRUE)
ON CONFLICT (username) DO NOTHING;
SQL

echo "[initdb] done"
