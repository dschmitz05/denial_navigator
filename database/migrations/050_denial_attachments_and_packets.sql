-- FB-15: denial attachments and appeal packet assembly.
--
-- Appeals need supporting documents (visit notes, authorizations, a
-- remittance excerpt), but there was no way to attach a file to a denial, and
-- no reviewable packet assembling the draft letter, the claim summary and an
-- attachment index. Attachments are PHI: org-scoped like everything else,
-- audited on every read by the existing request-audit middleware, and
-- covered by the same backup/retention posture as the rest of the database
-- (the file itself lives in object storage, keyed by this table's id, same
-- pattern as knowledge_documents' source files).
--
-- IF NOT EXISTS throughout: the app's snapshot-tail startup path re-runs
-- every migration file's raw SQL once, unconditionally, after loading
-- init.sql on a fresh database (see crates/db/src/migrations.rs), and
-- init.sql already carries these tables. Every migration must tolerate that.
CREATE TABLE IF NOT EXISTS denial_attachments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    filename VARCHAR(255) NOT NULL,
    content_type VARCHAR(100) NOT NULL,
    size_bytes BIGINT NOT NULL,
    storage_key TEXT NOT NULL,
    uploaded_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_denial_attachments_denial ON denial_attachments (denial_id, created_at DESC);

-- One packet per appeal, replaced (not versioned) on regeneration: a stale
-- draft naming the wrong attachments is worse than losing the old one.
-- 'draft' until a user reviews and approves it; approving does not submit it
-- to the payer, it only marks the content as reviewed.
CREATE TABLE IF NOT EXISTS appeal_packets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    appeal_id UUID NOT NULL UNIQUE REFERENCES appeals_queue(id) ON DELETE CASCADE,
    storage_key TEXT NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'approved')),
    generated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    generated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    approved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    approved_at TIMESTAMPTZ
);

-- How and when the packet actually went to the payer, and the payer's own
-- confirmation of receipt - distinct from `submitted_at`/`payer_response`,
-- which track the appeal's outcome, not how it was filed.
ALTER TABLE appeals_queue
    ADD COLUMN IF NOT EXISTS submission_method VARCHAR(20)
        CHECK (submission_method IN ('portal', 'fax', 'mail', 'email')),
    ADD COLUMN IF NOT EXISTS payer_confirmation_number VARCHAR(100);
