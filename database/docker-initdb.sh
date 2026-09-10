#!/bin/sh
# Fresh-container database bootstrap, mounted as
# /docker-entrypoint-initdb.d/00-run-all.sh. The stock postgres entrypoint
# cannot run a mounted directory, which is why this exists.
#
# `init.sql` is the current complete schema. The API baselines this snapshot
# into SQLx's migration ledger on its first start, then applies future numbered
# migrations with checksums. Do not replay historical migration files here:
# they describe intermediate schema states and are not all safe to rerun.
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

INSERT INTO organization_memberships (organization_id, user_id, role)
SELECT '00000000-0000-0000-0000-000000000001', id, role
FROM users
WHERE username = 'admin'
ON CONFLICT (organization_id, user_id) DO NOTHING;
SQL

echo "[initdb] done"
