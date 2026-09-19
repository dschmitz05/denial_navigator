-- FB-11: one payer, many spellings. Claims and documents name payers however
-- the source spelled them ("BlueCross BlueShield", "BLUECROSS BLUESHIELD OF
-- ILLINOIS") or by payer ID, and retrieval matched names exactly, so a claim
-- never found the policies filed under another spelling. A payer's aliases
-- (names and IDs) all resolve to it.

CREATE OR REPLACE FUNCTION normalize_payer_name(p TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
AS $fn$
    SELECT trim(regexp_replace(lower(COALESCE(p, '')), '[^a-z0-9]+', ' ', 'g'));
$fn$;

CREATE TABLE IF NOT EXISTS payers (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_payers_org_name
    ON payers (organization_id, normalize_payer_name(name));

CREATE TABLE IF NOT EXISTS payer_aliases (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    payer_id UUID NOT NULL REFERENCES payers(id) ON DELETE CASCADE,
    alias VARCHAR(255) NOT NULL,
    alias_normalized VARCHAR(255) NOT NULL,
    kind VARCHAR(10) NOT NULL DEFAULT 'name' CHECK (kind IN ('name', 'payer_id')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- An alias belongs to one payer per organization.
CREATE UNIQUE INDEX IF NOT EXISTS idx_payer_aliases_org_alias
    ON payer_aliases (organization_id, alias_normalized);
