-- FB-03: write-offs at or above an organization's threshold need a second
-- person's approval before the denial is written off.

-- 0 means every write-off needs approval.
ALTER TABLE organizations
    ADD COLUMN IF NOT EXISTS write_off_approval_threshold DECIMAL(12, 2) NOT NULL DEFAULT 0
        CHECK (write_off_approval_threshold >= 0);

CREATE TABLE IF NOT EXISTS write_off_requests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    -- The worklist item that asked for it, when it came from one.
    appeal_id UUID REFERENCES appeals_queue(id) ON DELETE SET NULL,
    amount DECIMAL(12, 2) NOT NULL,
    reason TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    decided_by UUID REFERENCES users(id) ON DELETE SET NULL,
    decided_at TIMESTAMPTZ,
    decision_note TEXT,
    -- Segregation of duties: the requester can never be the approver.
    CHECK (decided_by IS NULL OR requested_by IS NULL OR decided_by <> requested_by)
);

-- At most one open request per denial.
CREATE UNIQUE INDEX IF NOT EXISTS idx_write_off_requests_one_pending
    ON write_off_requests (denial_id) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_write_off_requests_org_status
    ON write_off_requests (organization_id, status, requested_at DESC);
