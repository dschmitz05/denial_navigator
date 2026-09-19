-- FB-09: a claim submitted on an 837 that the payer never answers is lost to
-- timely filing without ever becoming a denial. Track when a claim was
-- submitted and when its first remittance arrived.
ALTER TABLE claims
    ADD COLUMN IF NOT EXISTS submitted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS remittance_received_at TIMESTAMPTZ;

-- Claims that already have denials or payments were answered. Others from
-- before this migration cannot be told apart and are left out of the queue.
UPDATE claims c SET remittance_received_at = COALESCE(c.parsed_at, c.created_at)
WHERE c.remittance_received_at IS NULL
  AND (c.total_paid > 0 OR EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id));

CREATE INDEX IF NOT EXISTS idx_claims_unanswered
    ON claims (organization_id, submitted_at) WHERE remittance_received_at IS NULL;

-- Days a payer normally takes to answer, before a claim needs follow-up.
ALTER TABLE payer_deadline_rules DROP CONSTRAINT IF EXISTS payer_deadline_rules_deadline_type_check;
ALTER TABLE payer_deadline_rules ADD CONSTRAINT payer_deadline_rules_deadline_type_check
    CHECK (deadline_type IN ('timely_filing', 'corrected_claim', 'reconsideration',
                             'appeal_level_2', 'payer_response'));

-- What was done about an unanswered claim.
CREATE TABLE IF NOT EXISTS claim_followups (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    action VARCHAR(30) NOT NULL CHECK (action IN ('status_inquiry', 'resubmitted', 'payer_contact')),
    note TEXT,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_claim_followups_claim ON claim_followups (claim_id, created_at DESC);
