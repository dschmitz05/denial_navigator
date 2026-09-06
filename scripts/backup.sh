#!/bin/bash
# ============================================================
# Denial Navigator — database backup
#
# PostgreSQL holds everything: claims, denials, analyses, the queue, the
# knowledge base and the audit log. Nothing else in this deployment is state
# worth keeping, and nothing else needs backing up.
#
# Run from cron, e.g. nightly at 02:00:
#   0 2 * * * /path/to/denial-navigator/scripts/backup.sh >> /var/log/dn-backup.log 2>&1
# ============================================================
set -euo pipefail

BACKUP_DIR="${BACKUP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/database/backups}"
CONTAINER="${POSTGRES_CONTAINER:-denial-navigator-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
RETAIN_DAYS="${BACKUP_RETAIN_DAYS:-30}"

mkdir -p "$BACKUP_DIR"
stamp=$(date +%Y%m%d_%H%M%S)
target="$BACKUP_DIR/${DB_NAME}_${stamp}.sql.gz"

echo "[$(date -Is)] backing up $DB_NAME -> $target"
docker exec "$CONTAINER" pg_dump -U "$DB_USER" "$DB_NAME" | gzip > "$target"

# A dump that cannot be read is not a backup. Verify before trusting it, and
# fail loudly rather than leaving an unusable file in place.
if ! gzip -t "$target"; then
    echo "ERROR: $target is corrupt; removing" >&2
    rm -f "$target"
    exit 1
fi
size=$(du -h "$target" | cut -f1)
tables=$(gzip -dc "$target" | grep -c '^CREATE TABLE' || true)
echo "[$(date -Is)] wrote $size, $tables tables"

if [ "$tables" -lt 5 ]; then
    echo "ERROR: only $tables tables in the dump - refusing to treat this as good" >&2
    exit 1
fi

# These files contain PHI.
chmod 600 "$target"

deleted=$(find "$BACKUP_DIR" -name "${DB_NAME}_*.sql.gz" -mtime "+$RETAIN_DAYS" -print -delete | wc -l)
[ "$deleted" -gt 0 ] && echo "[$(date -Is)] pruned $deleted backup(s) older than $RETAIN_DAYS days"

echo "[$(date -Is)] done"
