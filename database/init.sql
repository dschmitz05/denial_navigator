-- ============================================================
-- Denial Navigator — Database Initialization
-- PostgreSQL 17 + pgvector
-- ============================================================

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ============================================================
-- 1. Claims
-- ============================================================
CREATE TABLE claims (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    claim_number VARCHAR(100) UNIQUE NOT NULL,
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
    frequency_code VARCHAR(20),
    admission_date DATE,
    discharge_date DATE,
    service_from DATE,
    service_to DATE,
    icd_10_codes TEXT[],
    diagnosis_pointer TEXT[],
    raw_835_data JSONB,
    parsed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_claims_status ON claims(status);
CREATE INDEX idx_claims_payer_id ON claims(payer_id);
CREATE INDEX idx_claims_created_at ON claims(created_at);
CREATE INDEX idx_claims_total_charge ON claims(total_charge DESC);
CREATE INDEX idx_claims_claim_number ON claims(claim_number);

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

-- ============================================================
-- 3. AI Analyses
-- ============================================================
CREATE TABLE ai_analyses (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    denial_id UUID NOT NULL REFERENCES denials(id) ON DELETE CASCADE,
    claim_id UUID NOT NULL,
    model_name VARCHAR(100),
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    system_prompt_template VARCHAR(255),
    raw_prompt TEXT,
    raw_response TEXT,
    explanation TEXT,
    denial_category VARCHAR(100)
        CHECK (denial_category IN ('coding_error', 'missing_info', 'lack_of_preauth', 'medical_necessity',
                                   'bundled_service', 'duplicate_claim', 'timely_filing',
                                   'non_covered_service', 'patient_responsibility', 'other')),
    root_cause_summary TEXT,
    required_action TEXT,
    action_plan JSONB,
    steps JSONB,
    needs_appeal BOOLEAN DEFAULT FALSE,
    draft_appeal_letter TEXT,
    confidence_score DECIMAL(3, 2),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_ai_analyses_denial_id ON ai_analyses(denial_id);
CREATE INDEX idx_ai_analyses_claim_id ON ai_analyses(claim_id);
CREATE INDEX idx_ai_analyses_denial_category ON ai_analyses(denial_category);
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
    -- 'corrected_claim', 'clinical_docs', 'payer_contact', 'bill_patient', 'write_off'
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
CREATE INDEX idx_feedback_loop_accepted ON feedback_loop(accepted);
CREATE INDEX idx_feedback_loop_was_paid ON feedback_loop(was_paid_on_resubmit);

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

-- ============================================================
-- 11. Users / RBAC
-- ============================================================
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    username VARCHAR(100) UNIQUE NOT NULL,
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255),
    full_name VARCHAR(255),
    role VARCHAR(50) NOT NULL DEFAULT 'billing_specialist'
        CHECK (role IN ('billing_specialist', 'billing_manager', 'rcm_director', 'admin')),
    is_active BOOLEAN DEFAULT TRUE,
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

CREATE INDEX idx_users_role ON users(role);

-- ============================================================
-- 12. File Uploads / Ingestion Log
-- ============================================================
CREATE TABLE ingestion_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
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
    user_id UUID NOT NULL,
    kind VARCHAR(50) NOT NULL,
    for_date DATE NOT NULL DEFAULT CURRENT_DATE,
    title VARCHAR(255) NOT NULL,
    body TEXT,
    payload JSONB,
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_notifications_once_per_day ON notifications (user_id, kind, for_date);
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
