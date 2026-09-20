-- FB-16: structured payer-interaction log.
--
-- `payer_contact` has always been a resolution type on appeals_queue and a
-- follow-up action on claim_followups, but the call itself — who was
-- reached, what reference number they gave, what they promised and by
-- when — was never recorded anywhere but a free-text note. An appeal or an
-- escalation later depends on exactly that detail.
CREATE TABLE payer_interactions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    channel VARCHAR(20) NOT NULL CHECK (channel IN ('phone', 'portal', 'fax', 'mail', 'email', 'other')),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reference_number VARCHAR(100),
    representative VARCHAR(255),
    summary TEXT NOT NULL,
    -- A date to check back by, if the payer promised one. NULL means no
    -- promise was made, not "check back never" - the digest only surfaces
    -- rows that have one.
    follow_up_on DATE,
    follow_up_completed_at TIMESTAMPTZ,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_payer_interactions_denial ON payer_interactions (denial_id, occurred_at DESC);
-- Backs the "due follow-ups" digest query: due, not yet completed, per org.
CREATE INDEX idx_payer_interactions_follow_up_due
    ON payer_interactions (organization_id, follow_up_on)
    WHERE follow_up_on IS NOT NULL AND follow_up_completed_at IS NULL;
