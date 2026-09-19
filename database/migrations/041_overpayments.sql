-- FB-08: overpayments found in remittances, tracked to a refund deadline.
-- Returning an identified overpayment is time-limited for many payers (60
-- days from identification for Medicare); the window is set per organization
-- and should be reviewed by compliance staff.

ALTER TABLE organizations
    ADD COLUMN IF NOT EXISTS overpayment_refund_days INTEGER NOT NULL DEFAULT 60
        CHECK (overpayment_refund_days BETWEEN 1 AND 3650);

CREATE TABLE IF NOT EXISTS overpayments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    ingestion_id UUID REFERENCES ingestion_log(id) ON DELETE SET NULL,
    -- paid_above_allowed: a line paid more than its allowed amount (AMT*B6)
    -- duplicate_payment: the claim paid again under a different payer claim
    --                    control number, with no reversal of the first payment
    kind VARCHAR(30) NOT NULL CHECK (kind IN ('paid_above_allowed', 'duplicate_payment')),
    service_line_number INTEGER,
    amount DECIMAL(12, 2) NOT NULL CHECK (amount > 0),
    payer_name VARCHAR(255),
    detail TEXT,
    identified_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    due_date DATE NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'identified'
        CHECK (status IN ('identified', 'refunded', 'recouped', 'disputed')),
    resolved_at TIMESTAMPTZ,
    resolved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    resolution_note TEXT
);

-- Re-ingesting the same remittance does not identify the same overpayment twice.
CREATE UNIQUE INDEX IF NOT EXISTS idx_overpayments_natural_key
    ON overpayments (organization_id, claim_id, kind, COALESCE(service_line_number, -1), amount);
CREATE INDEX IF NOT EXISTS idx_overpayments_org_status_due
    ON overpayments (organization_id, status, due_date);
