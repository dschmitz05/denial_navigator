-- Playbooks contain organization-specific operational knowledge and must never
-- be visible to or applied for another organization.
ALTER TABLE institutional_playbooks
    ADD COLUMN IF NOT EXISTS organization_id UUID;

-- Existing installations pre-date playbook tenant ownership. Attribute rows to
-- their creator when possible; legacy/system-created rows remain in the
-- development organization rather than becoming globally visible.
UPDATE institutional_playbooks p
SET organization_id = COALESCE(
    (
        SELECT om.organization_id
        FROM organization_memberships om
        WHERE om.user_id = p.created_by
        ORDER BY om.created_at ASC
        LIMIT 1
    ),
    '00000000-0000-0000-0000-000000000001'::uuid
)
WHERE organization_id IS NULL;

ALTER TABLE institutional_playbooks
    ALTER COLUMN organization_id SET NOT NULL;
ALTER TABLE institutional_playbooks
    DROP CONSTRAINT IF EXISTS institutional_playbooks_organization_id_fkey;
ALTER TABLE institutional_playbooks
    ADD CONSTRAINT institutional_playbooks_organization_id_fkey
    FOREIGN KEY (organization_id) REFERENCES organizations(id);
CREATE INDEX IF NOT EXISTS idx_institutional_playbooks_organization_status
    ON institutional_playbooks (organization_id, status, updated_at DESC);
