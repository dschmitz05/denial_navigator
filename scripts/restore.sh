#!/bin/bash
# ============================================================
# Denial Navigator — restore from a backup
#
# An untested backup is a hypothesis. Run this against a scratch database
# periodically to prove the dumps are usable:
#
#   ./scripts/restore.sh database/backups/denial_navigator_20260906.sql.gz --verify-only
# ============================================================
set -euo pipefail

FILE="${1:-}"
MODE="${2:-}"
CONTAINER="${POSTGRES_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"

[ -f "$FILE" ] || { echo "usage: $0 <backup.sql.gz> [--verify-only]" >&2; exit 1; }

if [ "$MODE" = "--verify-only" ]; then
    # Restore into a throwaway database. Proves the dump loads without
    # touching anything real - which is the only test that means something.
    TMP="verify_$(date +%s)"
    echo "Restoring into scratch database $TMP…"
    docker exec "$CONTAINER" createdb -U "$DB_USER" "$TMP"
    if gzip -dc "$FILE" | docker exec -i "$CONTAINER" psql -q -U "$DB_USER" -d "$TMP" >/dev/null 2>&1; then
        echo "Restored. Row counts:"
        docker exec "$CONTAINER" psql -U "$DB_USER" -d "$TMP" -t -c \
          "SELECT '  ' || relname || ': ' || n_live_tup FROM pg_stat_user_tables WHERE n_live_tup > 0 ORDER BY relname"
        docker exec "$CONTAINER" dropdb -U "$DB_USER" "$TMP"
        echo "Verified; scratch database removed."
    else
        docker exec "$CONTAINER" dropdb --if-exists -U "$DB_USER" "$TMP"
        echo "ERROR: the dump failed to restore" >&2
        exit 1
    fi
    exit 0
fi

echo "This REPLACES the contents of $DB_NAME. Type the database name to confirm:"
read -r confirm
[ "$confirm" = "$DB_NAME" ] || { echo "Aborted."; exit 1; }

gzip -dc "$FILE" | docker exec -i "$CONTAINER" psql -U "$DB_USER" -d "$DB_NAME"
echo "Restored from $FILE"
