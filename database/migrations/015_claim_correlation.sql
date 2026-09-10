ALTER TABLE claims
    ADD COLUMN IF NOT EXISTS correlation_status VARCHAR(20) NOT NULL DEFAULT 'unmatched'
        CHECK (correlation_status IN ('unmatched', 'matched', 'ambiguous')),
    ADD COLUMN IF NOT EXISTS correlation_confidence DECIMAL(3, 2);
