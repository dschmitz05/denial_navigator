-- ============================================================
-- Denial Navigator — Database Initialization
-- PostgreSQL 17 + pgvector
-- ============================================================

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ============================================================
-- 0. Organizations / tenant boundary
-- ============================================================
CREATE TABLE organizations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    slug VARCHAR(100) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    -- Write-offs at or above this amount need a second person's approval
    -- (write_off_requests); 0 means every write-off does.
    write_off_approval_threshold DECIMAL(12, 2) NOT NULL DEFAULT 0
        CHECK (write_off_approval_threshold >= 0),
    -- Days from identifying an overpayment to its refund deadline (FB-08).
    overpayment_refund_days INTEGER NOT NULL DEFAULT 60
        CHECK (overpayment_refund_days BETWEEN 1 AND 3650),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO organizations (id, slug, name)
VALUES ('00000000-0000-0000-0000-000000000001', 'development', 'Development Organization')
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- 1. Claims
-- ============================================================
CREATE TABLE claims (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
        REFERENCES organizations(id),
    claim_number VARCHAR(100) NOT NULL,
    patient_id VARCHAR(100) NOT NULL,
    patient_name VARCHAR(255),
    date_of_birth DATE,
    provider_npi VARCHAR(50),
    provider_name VARCHAR(255),
    payer_id UUID,
    payer_name VARCHAR(255),
    payer_id_number VARCHAR(100),
    total_charge DECIMAL(12, 2) NOT NULL DEFAULT 0,
    total_paid DECIMAL(12, 2) NOT NULL DEFAULT 0,
    total_adjustment DECIMAL(12, 2) NOT NULL DEFAULT 0,
    status VARCHAR(50) NOT NULL DEFAULT 'ingested'
        CHECK (status IN ('ingested', 'parsed', 'analyzed', 'denied', 'partially_paid', 'appealed', 'resolved', 'resubmitted')),
    claim_type VARCHAR(20) NOT NULL DEFAULT 'professional'
        CHECK (claim_type IN ('professional', 'institutional', 'pharmacy')),
    facility_type_code VARCHAR(20),
    frequency_code VARCHAR(20),
    admission_date DATE,
    discharge_date DATE,
    service_from DATE,
    service_to DATE,
    icd_10_codes TEXT[],
    diagnosis_pointer TEXT[],
    raw_835_data JSONB,
    correlation_status VARCHAR(20) NOT NULL DEFAULT 'unmatched'
        CHECK (correlation_status IN ('unmatched', 'matched', 'ambiguous')),
    correlation_confidence DECIMAL(3, 2),
    -- A payer that pays after this one (FB-06): PR balances go there first.
    next_payer_name VARCHAR(255),
    next_payer_source VARCHAR(30)
        CHECK (next_payer_source IN ('837_other_subscriber', '835_crossover')),
    -- Set when the payer reverses the claim (835 CLP02 22).
    reversed_at TIMESTAMPTZ,
    -- When an 837 submitted it and when its first remittance arrived (FB-09).
    submitted_at TIMESTAMPTZ,
    remittance_received_at TIMESTAMPTZ,
    parsed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_claims_status ON claims(status);
CREATE INDEX idx_claims_payer_id ON claims(payer_id);
CREATE INDEX idx_claims_created_at ON claims(created_at);
CREATE INDEX idx_claims_total_charge ON claims(total_charge DESC);
CREATE UNIQUE INDEX idx_claims_organization_claim_number ON claims(organization_id, claim_number);

-- ============================================================
-- 2. Denials (Claim Adjustments)
-- ============================================================
CREATE TABLE denials (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    service_line_number INTEGER,
    cpt_code VARCHAR(20),
    hcpcs_code VARCHAR(20),
    modifier_1 VARCHAR(10),
    modifier_2 VARCHAR(10),
    charge_amount DECIMAL(12, 2) NOT NULL DEFAULT 0,
    payment_amount DECIMAL(12, 2) NOT NULL DEFAULT 0,
    adjustment_amount DECIMAL(12, 2) NOT NULL DEFAULT 0,
    cagc VARCHAR(20) NOT NULL
        CHECK (cagc IN ('PR', 'CO', 'OA', 'AB', 'AS', 'PI')),
    carc_code VARCHAR(20),
    rarc_code VARCHAR(20),
    denial_reason_code VARCHAR(50),
    adjustment_reason VARCHAR(500),
    denial_date DATE NOT NULL DEFAULT CURRENT_DATE,
    -- 'in_progress' is denial work that is NOT an appeal: a corrected claim,
    -- a records request, a payer call. Keeping it distinct from 'in_appeal'
    -- is what keeps the Appeals tab to actual appeals.
    status VARCHAR(50) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'analyzed', 'in_progress', 'in_appeal',
                          'appealed', 'overruled', 'resolved', 'written_off')),
    appeal_deadline DATE,
    -- 'remittance' when a later 835 paid the denied line and closed it.
    resolution_source VARCHAR(20) CHECK (resolution_source IN ('user', 'remittance')),
    recovered_amount DECIMAL(12, 2),
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_denials_claim_id ON denials(claim_id);
CREATE INDEX idx_denials_status ON denials(status);
CREATE INDEX idx_denials_cagc ON denials(cagc);
CREATE INDEX idx_denials_carc_code ON denials(carc_code);
CREATE INDEX idx_denials_rarc_code ON denials(rarc_code);
CREATE INDEX idx_denials_cpt_code ON denials(cpt_code);
CREATE INDEX idx_denials_denial_date ON denials(denial_date);
CREATE INDEX idx_denials_appeal_deadline ON denials(appeal_deadline);
CREATE INDEX idx_denials_charge_amount ON denials(charge_amount DESC);
-- One row per adjustment occurrence (migration 007); re-ingesting a file must
-- not duplicate denials.
CREATE UNIQUE INDEX idx_denials_natural_key ON denials (
    claim_id,
    COALESCE(service_line_number, -1),
    COALESCE(cpt_code, ''),
    cagc,
    COALESCE(carc_code, ''),
    charge_amount,
    adjustment_amount,
    COALESCE(denial_date, '1900-01-01'::date)
);

-- ============================================================
-- 3. AI Analyses
-- ============================================================
CREATE TABLE ai_analyses (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    playbook_id UUID,
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    claim_id UUID NOT NULL,
    model_name VARCHAR(100),
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    system_prompt_template VARCHAR(255),
    provider_name VARCHAR(100),
    provider_version VARCHAR(100),
    prompt_template_version VARCHAR(100),
    raw_prompt TEXT,
    raw_response TEXT,
    explanation TEXT,
    denial_category VARCHAR(100)
        CHECK (denial_category IN ('coding_error', 'missing_info', 'lack_of_preauth', 'medical_necessity',
                                   'bundled_service', 'duplicate_claim', 'timely_filing',
                                   'non_covered_service', 'patient_responsibility', 'other')),
    root_cause_summary TEXT,
    -- Why the analysis is degraded, if it is (llm_error, retrieval_error, no_evidence).
    fallback_reason VARCHAR(20)
        CHECK (fallback_reason IN ('llm_error', 'retrieval_error', 'no_evidence')),
    required_action TEXT,
    action_plan JSONB,
    steps JSONB,
    citations JSONB NOT NULL DEFAULT '[]'::jsonb,
    needs_appeal BOOLEAN DEFAULT FALSE,
    draft_appeal_letter TEXT,
    confidence_score DECIMAL(3, 2),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_ai_analyses_denial_id ON ai_analyses(denial_id);
CREATE INDEX idx_ai_analyses_claim_id ON ai_analyses(claim_id);
CREATE INDEX idx_ai_analyses_denial_category ON ai_analyses(denial_category);
CREATE INDEX idx_ai_analyses_created_fallback ON ai_analyses (created_at DESC, fallback_reason);
CREATE INDEX idx_ai_analyses_created_at ON ai_analyses(created_at);

-- ============================================================
-- 4. Appeals Queue
-- ============================================================
CREATE TABLE appeals_queue (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    claim_id UUID NOT NULL,
    ai_analysis_id UUID REFERENCES ai_analyses(id) ON DELETE SET NULL,
    assigned_user_id UUID,
    resolution_type VARCHAR(100),
    -- 'appeal_letter'  -> Appeals tab; everything below -> Worklist tab.
    -- 'corrected_claim', 'clinical_docs', 'payer_contact', 'bill_patient',
    -- 'bill_secondary', 'write_off'
    -- 'bill_patient' and 'write_off' are NOT interchangeable: a PR balance is
    -- billed to the patient and collected; a CO write-off is absorbed.
    outcome_status VARCHAR(50)
        CHECK (outcome_status IN ('queued', 'in_progress', 'submitted', 'approved',
                                  'denied_again', 'overruled', 'resolved', 'cancelled')),
    submitted_at TIMESTAMPTZ,
    payer_response DATE,
    payer_response_text TEXT,
    final_outcome TEXT,
    notes TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_appeals_queue_denial_id ON appeals_queue(denial_id);
CREATE INDEX idx_appeals_queue_assigned_user_id ON appeals_queue(assigned_user_id);
CREATE INDEX idx_appeals_queue_outcome_status ON appeals_queue(outcome_status);
CREATE INDEX idx_appeals_queue_resolution_type ON appeals_queue(resolution_type);
CREATE INDEX idx_appeals_queue_created_at ON appeals_queue(created_at);

-- ============================================================
-- 5. Feedback Loop
-- ============================================================
CREATE TABLE feedback_loop (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    ai_analysis_id UUID NOT NULL REFERENCES ai_analyses(id) ON DELETE CASCADE,
    user_id UUID,
    rating SMALLINT CHECK (rating BETWEEN 1 AND 5),
    accepted BOOLEAN,
    user_edits JSONB,
    action_taken VARCHAR(100),
    -- 'accepted_as_is', 'modified_then_accepted', 'rejected', 'corrected_and_resubmitted'
    was_paid_on_resubmit BOOLEAN,
    resubmit_result TEXT,
    feedback_text TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_feedback_loop_ai_analysis_id ON feedback_loop(ai_analysis_id);
CREATE INDEX idx_feedback_loop_user_id ON feedback_loop(user_id);
CREATE INDEX idx_feedback_loop_accepted ON feedback_loop(accepted);
CREATE INDEX idx_feedback_loop_was_paid ON feedback_loop(was_paid_on_resubmit);
CREATE INDEX idx_feedback_loop_paid_created ON feedback_loop(created_at DESC)
    WHERE was_paid_on_resubmit IS TRUE;

CREATE TABLE institutional_playbooks (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    triggers JSONB NOT NULL DEFAULT '{}'::jsonb,
    recommendation JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(20) NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'approved', 'archived')),
    version INTEGER NOT NULL DEFAULT 1,
    created_by UUID,
    approved_by UUID,
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_institutional_playbooks_status ON institutional_playbooks(status);
CREATE INDEX idx_institutional_playbooks_organization_status ON institutional_playbooks(organization_id, status, updated_at DESC);
CREATE INDEX idx_institutional_playbooks_triggers ON institutional_playbooks USING GIN(triggers);
ALTER TABLE ai_analyses ADD CONSTRAINT ai_analyses_playbook_id_fkey
    FOREIGN KEY (playbook_id) REFERENCES institutional_playbooks(id) ON DELETE SET NULL;
CREATE INDEX idx_ai_analyses_playbook_id ON ai_analyses(playbook_id);

-- ============================================================
-- 6. CARC Codes (Claim Adjustment Reason Codes - WPC)
-- ============================================================
CREATE TABLE carc_codes (
    code VARCHAR(20) PRIMARY KEY,
    description TEXT NOT NULL,
    category VARCHAR(100),
    -- 'patient_responsibility', 'contractual_obligation', 'payer_policy', 'coding', 'administrative'
    effective_date DATE NOT NULL DEFAULT CURRENT_DATE,
    expiration_date DATE,
    is_active BOOLEAN DEFAULT TRUE,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_carc_codes_is_active ON carc_codes(is_active);

-- ============================================================
-- 7. RARC Codes (Remittance Advice Remark Codes - WPC)
-- ============================================================
CREATE TABLE rarc_codes (
    code VARCHAR(20) PRIMARY KEY,
    description TEXT NOT NULL,
    applicable_cagc VARCHAR(20),
    effective_date DATE NOT NULL DEFAULT CURRENT_DATE,
    expiration_date DATE,
    is_active BOOLEAN DEFAULT TRUE,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_rarc_codes_is_active ON rarc_codes(is_active);

-- ============================================================
-- 8. Knowledge Documents (RAG Source Material)
-- ============================================================
CREATE TABLE knowledge_documents (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
        REFERENCES organizations(id),
    title VARCHAR(500) NOT NULL,
    source_type VARCHAR(50) NOT NULL
        CHECK (source_type IN ('cms_lcd', 'payer_policy', 'fee_schedule',
                               'contract', 'prior_auth_policy', 'medical_necessity_criteria')),
    payer_id UUID,
    effective_date DATE,
    expiration_date DATE,
    file_path TEXT,
    mime_type VARCHAR(100),
    file_size_bytes BIGINT,
    status VARCHAR(50) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'processing', 'indexed', 'error', 'archived')),
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_knowledge_documents_source_type ON knowledge_documents(source_type);
CREATE INDEX idx_knowledge_documents_status ON knowledge_documents(status);
CREATE INDEX idx_knowledge_documents_payer_id ON knowledge_documents(payer_id);
CREATE INDEX idx_knowledge_documents_organization_id ON knowledge_documents(organization_id);

-- ============================================================
-- 9. Knowledge Chunks (Vector Embeddings)
-- ============================================================
CREATE TABLE knowledge_chunks (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    knowledge_document_id UUID NOT NULL REFERENCES knowledge_documents(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    content TEXT NOT NULL,
    embedding vector(768),  -- nomic-embed-text produces 768-dim vectors
    metadata JSONB,
    token_count INTEGER,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_knowledge_chunks_doc_id ON knowledge_chunks(knowledge_document_id);
CREATE INDEX idx_knowledge_chunks_content_fts ON knowledge_chunks USING GIN (to_tsvector('english', content));
CREATE INDEX idx_knowledge_documents_effective_dates ON knowledge_documents (effective_date, expiration_date) WHERE status <> 'archived';
CREATE INDEX idx_knowledge_documents_jurisdiction ON knowledge_documents (lower(COALESCE(metadata->>'jurisdiction', ''))) WHERE metadata ? 'jurisdiction';
-- HNSW, not IVFFlat. IVFFlat computes its centroids at CREATE INDEX time, and
-- this file runs against an EMPTY database - so the centroids were meaningless
-- and the default ivfflat.probes = 1 scanned one arbitrary list. Measured on a
-- 3-chunk table: the index returned 0 rows where an exact scan returned 3.
-- A vector index that silently returns nothing is worse than no index at all.
-- HNSW builds incrementally, needs no training data, and has high recall.
CREATE INDEX idx_knowledge_chunks_embedding ON knowledge_chunks USING hnsw (embedding vector_cosine_ops);

-- ============================================================
-- 10. Audit Log (HIPAA Compliance)
-- ============================================================
CREATE TABLE audit_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID REFERENCES organizations(id),
    user_id UUID,
    action VARCHAR(100) NOT NULL,
    -- 'login', 'view_claim', 'edit_claim', 'generate_analysis', 'submit_appeal',
    -- 'ingest_file', 'update_knowledge', 'modify_analysis', 'export_data'
    resource_type VARCHAR(50) NOT NULL,
    -- 'claim', 'denial', 'analysis', 'appeal', 'knowledge_doc', 'user'
    resource_id UUID,
    details JSONB,
    ip_address INET,
    user_agent TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_audit_log_action ON audit_log(action);
CREATE INDEX idx_audit_log_resource ON audit_log(resource_type, resource_id);
CREATE INDEX idx_audit_log_user_id ON audit_log(user_id);
CREATE INDEX idx_audit_log_created_at ON audit_log(created_at);
CREATE INDEX idx_audit_log_organization_created ON audit_log(organization_id, created_at DESC);

-- ============================================================
-- 11. Users / RBAC
-- ============================================================
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    username VARCHAR(100) UNIQUE NOT NULL,
    email VARCHAR(255) UNIQUE NOT NULL,
    oidc_subject VARCHAR(255) UNIQUE,
    password_hash VARCHAR(255),
    full_name VARCHAR(255),
    role VARCHAR(50) NOT NULL DEFAULT 'billing_specialist'
        CHECK (role IN ('system_admin', 'security_admin', 'revenue_cycle_manager',
                        'billing_specialist', 'coding_specialist', 'auditor', 'read_only')),
    is_active BOOLEAN DEFAULT TRUE,
    -- Set when someone else chose the password (seeded default, admin
    -- registration or reset); cleared when the user sets their own.
    must_change_password BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    last_login TIMESTAMPTZ,
    -- Tokens issued before this moment are refused. Bumped on password change
    -- so a credential reset actually ends the sessions using the old one.
    sessions_valid_from TIMESTAMPTZ NOT NULL DEFAULT date_trunc('second', NOW()),
    -- Two-factor authentication. An administrator sets totp_required; the user
    -- enrols by scanning the secret and proving they hold the device, which
    -- sets totp_confirmed_at. Clearing the secret re-enrols a lost device.
    totp_required BOOLEAN NOT NULL DEFAULT FALSE,
    totp_secret TEXT,
    totp_confirmed_at TIMESTAMPTZ,
    totp_last_used_step BIGINT
);

-- Write-offs at or above organizations.write_off_approval_threshold wait here
-- for a second person's approval (FB-03).
CREATE TABLE write_off_requests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
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
    CHECK (decided_by IS NULL OR requested_by IS NULL OR decided_by <> requested_by)
);
CREATE UNIQUE INDEX idx_write_off_requests_one_pending
    ON write_off_requests (denial_id) WHERE status = 'pending';
CREATE INDEX idx_write_off_requests_org_status
    ON write_off_requests (organization_id, status, requested_at DESC);


ALTER TABLE feedback_loop
    ADD CONSTRAINT feedback_loop_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE institutional_playbooks
    ADD CONSTRAINT institutional_playbooks_created_by_fkey
    FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL,
    ADD CONSTRAINT institutional_playbooks_approved_by_fkey
    FOREIGN KEY (approved_by) REFERENCES users(id) ON DELETE SET NULL;

CREATE INDEX idx_users_role ON users(role);

CREATE TABLE organization_memberships (
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(50) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (organization_id, user_id)
);
CREATE INDEX idx_organization_memberships_user ON organization_memberships(user_id);

-- Durable, asynchronous recommendation generation jobs. The API's existing
-- synchronous endpoint remains available for interactive use.
CREATE TABLE recommendation_jobs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    temperature REAL NOT NULL DEFAULT 0.3,
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    result JSONB,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
CREATE INDEX idx_recommendation_jobs_status_created
    ON recommendation_jobs(status, created_at);

-- ============================================================
-- 12. File Uploads / Ingestion Log
-- ============================================================
CREATE TABLE ingestion_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
        REFERENCES organizations(id),
    file_name VARCHAR(500) NOT NULL,
    file_path TEXT,
    file_size_bytes BIGINT,
    file_hash VARCHAR(64),
    status VARCHAR(50) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'parsing', 'parsed', 'error', 'completed')),
    claims_count INTEGER,
    denials_count INTEGER,
    errors JSONB,
    raw_response JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX idx_ingestion_log_status ON ingestion_log(status);
CREATE INDEX idx_ingestion_log_created_at ON ingestion_log(created_at);
CREATE INDEX idx_ingestion_log_organization_created ON ingestion_log(organization_id, created_at DESC);


-- FB-07: PLB provider-level adjustments from 835s. They change a payment
-- without belonging to a patient claim: recoupment of an earlier overpayment
-- (WO), forward balances (FB), interest (L6) and others. A positive amount
-- reduced the payment; a negative one added to it.
CREATE TABLE provider_adjustments (
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
CREATE UNIQUE INDEX idx_provider_adjustments_natural_key
    ON provider_adjustments (organization_id, COALESCE(trace_number, ''), reason_code,
                             COALESCE(reference_number, ''), amount,
                             COALESCE(fiscal_period_date, '1900-01-01'::date));
CREATE INDEX idx_provider_adjustments_org_date
    ON provider_adjustments (organization_id, payment_date DESC);
CREATE INDEX idx_provider_adjustments_claim ON provider_adjustments (claim_id);

-- FB-08: overpayments found in remittances, tracked to a refund deadline.
CREATE TABLE overpayments (
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
CREATE UNIQUE INDEX idx_overpayments_natural_key
    ON overpayments (organization_id, claim_id, kind, COALESCE(service_line_number, -1), amount);
CREATE INDEX idx_overpayments_org_status_due
    ON overpayments (organization_id, status, due_date);

-- FB-09: follow-up on claims the payer has not answered.
CREATE INDEX idx_claims_unanswered ON claims (organization_id, submitted_at) WHERE remittance_received_at IS NULL;
CREATE TABLE claim_followups (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    action VARCHAR(30) NOT NULL CHECK (action IN ('status_inquiry', 'resubmitted', 'payer_contact')),
    note TEXT,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_claim_followups_claim ON claim_followups (claim_id, created_at DESC);

-- ============================================================
-- 13. Payer appeal filing windows
-- ============================================================
-- An 835 states the payer's adjudication, not your window to contest it, so
-- the filing deadline is configuration rather than parsed data. '*' is the
-- default for payers without an entry.
CREATE TABLE payer_appeal_policies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    payer_name VARCHAR(255) NOT NULL,
    appeal_window_days INTEGER NOT NULL CHECK (appeal_window_days BETWEEN 1 AND 3650),
    notes TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_payer_appeal_policies_name
    ON payer_appeal_policies (lower(payer_name));

INSERT INTO payer_appeal_policies (payer_name, appeal_window_days, notes)
VALUES ('*', 90, 'Default filing window for payers without a specific policy.');

-- FB-11: payers and the spellings and IDs that resolve to them.
CREATE OR REPLACE FUNCTION normalize_payer_name(p TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
AS $fn$
    SELECT trim(regexp_replace(lower(COALESCE(p, '')), '[^a-z0-9]+', ' ', 'g'));
$fn$;

CREATE TABLE payers (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX idx_payers_org_name
    ON payers (organization_id, normalize_payer_name(name));

CREATE TABLE payer_aliases (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    payer_id UUID NOT NULL REFERENCES payers(id) ON DELETE CASCADE,
    alias VARCHAR(255) NOT NULL,
    alias_normalized VARCHAR(255) NOT NULL,
    kind VARCHAR(10) NOT NULL DEFAULT 'name' CHECK (kind IN ('name', 'payer_id')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- An alias belongs to one payer per organization.
CREATE UNIQUE INDEX idx_payer_aliases_org_alias
    ON payer_aliases (organization_id, alias_normalized);

-- FB-10: other payer clocks (timely filing, corrected claim, reconsideration,
-- second-level appeal), per organization; payer_name '*' is the default.
CREATE TABLE payer_deadline_rules (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id),
    payer_name VARCHAR(255) NOT NULL,
    deadline_type VARCHAR(30) NOT NULL
        CHECK (deadline_type IN ('timely_filing', 'corrected_claim', 'reconsideration',
                                 'appeal_level_2', 'payer_response')),
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 3650),
    notes TEXT,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX idx_payer_deadline_rules_unique
    ON payer_deadline_rules (organization_id, lower(payer_name), deadline_type);

CREATE OR REPLACE FUNCTION appeal_deadline_for(p_payer TEXT, p_base DATE)
RETURNS DATE
LANGUAGE sql
STABLE
AS $fn$
    SELECT COALESCE(p_base, CURRENT_DATE) + (
        COALESCE(
            (SELECT appeal_window_days FROM payer_appeal_policies
              WHERE lower(payer_name) = lower(COALESCE(p_payer, '')) LIMIT 1),
            (SELECT appeal_window_days FROM payer_appeal_policies
              WHERE payer_name = '*' LIMIT 1),
            90
        ) || ' days'
    )::INTERVAL;
$fn$;

-- ============================================================
-- 14. Notifications
-- ============================================================
-- Deadline digests and escalations. Delivered in-app because an air-gapped
-- deployment may have no mail path; the unique index makes generation
-- idempotent, since the digest runs from cron against four API workers.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL,
    user_id UUID NOT NULL,
    kind VARCHAR(50) NOT NULL,
    for_date DATE NOT NULL DEFAULT CURRENT_DATE,
    title VARCHAR(255) NOT NULL,
    body TEXT,
    payload JSONB,
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_notifications_once_per_day ON notifications (organization_id, user_id, kind, for_date);
CREATE INDEX idx_notifications_unread ON notifications (user_id, created_at DESC) WHERE read_at IS NULL;

-- ============================================================
-- Triggers: updated_at auto-update
-- ============================================================
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_claims_updated_at BEFORE UPDATE ON claims
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_denials_updated_at BEFORE UPDATE ON denials
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_ai_analyses_updated_at BEFORE UPDATE ON ai_analyses
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_appeals_queue_updated_at BEFORE UPDATE ON appeals_queue
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_knowledge_documents_updated_at BEFORE UPDATE ON knowledge_documents
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_payer_appeal_policies_updated_at BEFORE UPDATE ON payer_appeal_policies
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- Views for Dashboard Queries
-- ============================================================

-- Denial summary view
CREATE OR REPLACE VIEW vw_denial_summary AS
SELECT
    c.id AS claim_id,
    c.claim_number,
    c.patient_name,
    c.payer_name,
    c.total_charge,
    d.id AS denial_id,
    d.cpt_code,
    d.cagc,
    d.carc_code,
    d.rarc_code,
    d.charge_amount AS denial_amount,
    d.status AS denial_status,
    d.denial_date,
    d.appeal_deadline,
    aa.explanation,
    aa.denial_category,
    aa.needs_appeal,
    aq.outcome_status AS appeal_status
FROM claims c
JOIN denials d ON d.claim_id = c.id
LEFT JOIN ai_analyses aa ON aa.denial_id = d.id
LEFT JOIN appeals_queue aq ON aq.denial_id = d.id;

-- High-priority denials (appeal deadline approaching)
CREATE OR REPLACE VIEW vw_priority_denials AS
SELECT ds.*
FROM vw_denial_summary ds
JOIN claims c ON c.id = ds.claim_id
WHERE c.status = 'denied'
  AND ds.appeal_deadline IS NOT NULL
  AND ds.appeal_deadline <= CURRENT_DATE + INTERVAL '14 days'
ORDER BY ds.appeal_deadline ASC;

-- Denial by CARC code aggregation
CREATE OR REPLACE VIEW vw_denial_by_carc AS
SELECT
    d.carc_code,
    COALESCE(cc.description, 'Unknown') AS carc_description,
    COUNT(*) AS denial_count,
    SUM(d.charge_amount) AS total_denied_amount,
    AVG(d.charge_amount) AS avg_denial_amount
FROM denials d
LEFT JOIN carc_codes cc ON cc.code = d.carc_code
GROUP BY d.carc_code, cc.description
ORDER BY total_denied_amount DESC;
