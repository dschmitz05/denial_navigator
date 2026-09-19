-- FB-07: PLB provider-level adjustments from 835s. They change a payment
-- without belonging to a patient claim: recoupment of an earlier overpayment
-- (WO), forward balances (FB), interest (L6) and others. A positive amount
-- reduced the payment; a negative one added to it.
CREATE TABLE IF NOT EXISTS provider_adjustments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    ingestion_id UUID REFERENCES ingestion_log(id) ON DELETE SET NULL,
    payer_name VARCHAR(255),
    payer_identifier VARCHAR(80),
    -- TRN02: the check or EFT trace number of the payment it adjusted.
    trace_number VARCHAR(80),
    payment_date DATE,
    provider_identifier VARCHAR(80),
    fiscal_period_date DATE,
    reason_code VARCHAR(10) NOT NULL,
    reference_number VARCHAR(80),
    amount DECIMAL(12, 2) NOT NULL,
    -- The claim the reference names, when it matches one.
    claim_id UUID REFERENCES claims(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The same payment re-sent in another file is not recorded twice.
CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_adjustments_natural_key
    ON provider_adjustments (organization_id, COALESCE(trace_number, ''), reason_code,
                             COALESCE(reference_number, ''), amount,
                             COALESCE(fiscal_period_date, '1900-01-01'::date));
CREATE INDEX IF NOT EXISTS idx_provider_adjustments_org_date
    ON provider_adjustments (organization_id, payment_date DESC);
CREATE INDEX IF NOT EXISTS idx_provider_adjustments_claim ON provider_adjustments (claim_id);
