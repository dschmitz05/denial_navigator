-- FB-10: payers run several clocks, not one appeal window. Each rule is the
-- number of days a payer allows for one kind of action, per organization;
-- payer_name '*' is the organization's default. The first-level appeal window
-- stays in payer_appeal_policies (denials.appeal_deadline).
--   timely_filing    from the date of service
--   corrected_claim  from the remittance date
--   reconsideration  from the remittance date
--   appeal_level_2   from the payer's first-level appeal decision
CREATE TABLE IF NOT EXISTS payer_deadline_rules (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    payer_name VARCHAR(255) NOT NULL,
    deadline_type VARCHAR(30) NOT NULL
        CHECK (deadline_type IN ('timely_filing', 'corrected_claim', 'reconsideration', 'appeal_level_2')),
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 3650),
    notes TEXT,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_payer_deadline_rules_unique
    ON payer_deadline_rules (organization_id, lower(payer_name), deadline_type);
