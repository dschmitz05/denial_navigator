-- Approved institutional rules are explicit, versioned records rather than
-- opaque learning from production data.
CREATE TABLE IF NOT EXISTS institutional_playbooks (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    triggers JSONB NOT NULL DEFAULT '{}'::jsonb,
    recommendation JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'approved', 'archived')),
    version INTEGER NOT NULL DEFAULT 1,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    approved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_institutional_playbooks_status ON institutional_playbooks(status);
CREATE INDEX IF NOT EXISTS idx_institutional_playbooks_triggers ON institutional_playbooks USING GIN(triggers);

ALTER TABLE users DROP CONSTRAINT IF EXISTS users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN
 ('billing_specialist', 'billing_manager', 'rcm_director', 'admin', 'auditor'));
