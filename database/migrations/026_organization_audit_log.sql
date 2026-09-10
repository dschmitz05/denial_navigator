ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS organization_id UUID;

UPDATE audit_log al
SET organization_id = (
    SELECT organization_id
    FROM organization_memberships
    WHERE user_id = al.user_id
    ORDER BY created_at ASC
    LIMIT 1
)
WHERE al.organization_id IS NULL;

ALTER TABLE audit_log DROP CONSTRAINT IF EXISTS audit_log_organization_id_fkey;
ALTER TABLE audit_log ADD CONSTRAINT audit_log_organization_id_fkey
    FOREIGN KEY (organization_id) REFERENCES organizations(id);
CREATE INDEX IF NOT EXISTS idx_audit_log_organization_created
    ON audit_log(organization_id, created_at DESC);
