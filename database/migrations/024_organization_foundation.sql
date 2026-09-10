CREATE TABLE IF NOT EXISTS organizations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    slug VARCHAR(100) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO organizations (id, slug, name)
VALUES ('00000000-0000-0000-0000-000000000001', 'development', 'Development Organization')
ON CONFLICT (id) DO NOTHING;

ALTER TABLE claims ADD COLUMN IF NOT EXISTS organization_id UUID;
UPDATE claims
SET organization_id = '00000000-0000-0000-0000-000000000001'
WHERE organization_id IS NULL;
ALTER TABLE claims ALTER COLUMN organization_id SET NOT NULL;
ALTER TABLE claims ALTER COLUMN organization_id
    SET DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE claims DROP CONSTRAINT IF EXISTS claims_organization_id_fkey;
ALTER TABLE claims ADD CONSTRAINT claims_organization_id_fkey
    FOREIGN KEY (organization_id) REFERENCES organizations(id);
CREATE INDEX IF NOT EXISTS idx_claims_organization_id ON claims(organization_id);

CREATE TABLE IF NOT EXISTS organization_memberships (
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(50) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (organization_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_organization_memberships_user
    ON organization_memberships(user_id);

INSERT INTO organization_memberships (organization_id, user_id, role)
SELECT '00000000-0000-0000-0000-000000000001', id, role FROM users
ON CONFLICT (organization_id, user_id) DO NOTHING;
