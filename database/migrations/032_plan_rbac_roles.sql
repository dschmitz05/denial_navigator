-- Normalize the legacy role names to the seven roles in plan §2.2.
-- The old constraint only allows the legacy names, so drop it before renaming.
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_role_check;

UPDATE users SET role = CASE role
    WHEN 'admin' THEN 'system_admin'
    WHEN 'rcm_director' THEN 'revenue_cycle_manager'
    WHEN 'billing_manager' THEN 'revenue_cycle_manager'
    ELSE role
END;

UPDATE organization_memberships SET role = CASE role
    WHEN 'admin' THEN 'system_admin'
    WHEN 'rcm_director' THEN 'revenue_cycle_manager'
    WHEN 'billing_manager' THEN 'revenue_cycle_manager'
    ELSE role
END;

-- Memberships historically had no role constraint. Preserve access safely for
-- unknown custom values until organization-defined roles are supported.
UPDATE organization_memberships
SET role = 'read_only'
WHERE role NOT IN (
    'system_admin', 'security_admin', 'revenue_cycle_manager',
    'billing_specialist', 'coding_specialist', 'auditor', 'read_only'
);

ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN (
    'system_admin', 'security_admin', 'revenue_cycle_manager',
    'billing_specialist', 'coding_specialist', 'auditor', 'read_only'
));

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'organization_memberships'::regclass
          AND conname = 'organization_memberships_role_check'
    ) THEN
        ALTER TABLE organization_memberships
            ADD CONSTRAINT organization_memberships_role_check CHECK (role IN (
                'system_admin', 'security_admin', 'revenue_cycle_manager',
                'billing_specialist', 'coding_specialist', 'auditor', 'read_only'
            ));
    END IF;
END $$;
