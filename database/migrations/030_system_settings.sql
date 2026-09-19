-- Key/value store for admin-configurable system settings. Idempotent so it can
-- run on both the empty-database snapshot-tail path and as a forward migration.
CREATE TABLE IF NOT EXISTS system_settings (
    key        VARCHAR(100) PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by UUID
);

-- Seed the PHI disclosure level at the plan §12.2 default (withhold the claim
-- reference). ON CONFLICT keeps an operator's existing choice intact.
INSERT INTO system_settings (key, value)
VALUES ('phi_disclosure_level', 'deidentified')
ON CONFLICT (key) DO NOTHING;
