-- ============================================================
-- 011 — reference_imports: a log of CARC/RARC list refreshes
--
-- carc_codes and rarc_codes are seeded once from database/seed/,
-- but X12 revises the published CARC list several times a year and
-- the in-house RARC list changes as payers are added. This table
-- records who imported which file, what it changed, and when, so
-- the Settings page can show how current the reference data is and
-- a reviewer can trace why a code's description changed.
--
-- Idempotent: safe to re-run against an existing database.
-- ============================================================

CREATE TABLE IF NOT EXISTS reference_imports (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    kind VARCHAR(10) NOT NULL CHECK (kind IN ('carc', 'rarc')),
    filename TEXT,
    rows_parsed INTEGER NOT NULL DEFAULT 0,
    rows_added INTEGER NOT NULL DEFAULT 0,
    rows_updated INTEGER NOT NULL DEFAULT 0,
    rows_deactivated INTEGER NOT NULL DEFAULT 0,
    codes_not_in_file INTEGER NOT NULL DEFAULT 0,
    row_errors JSONB NOT NULL DEFAULT '[]',
    imported_by VARCHAR(100),
    imported_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_reference_imports_kind_time
    ON reference_imports (kind, imported_at DESC);

-- The two lists predate the import feature and carry only created_at.
-- updated_at is how a reviewer sees which codes a refresh touched.
ALTER TABLE carc_codes ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;
ALTER TABLE rarc_codes  ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;
