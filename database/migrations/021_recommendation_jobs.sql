CREATE TABLE IF NOT EXISTS recommendation_jobs (
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

CREATE INDEX IF NOT EXISTS idx_recommendation_jobs_status_created
    ON recommendation_jobs(status, created_at);
