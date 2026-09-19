-- FB-02: payer reversals and outcomes read from later remittances.

-- A claim the payer reversed (CLP02 22). The reversal loop's negated totals are
-- never written over the claim; this records that it happened.
ALTER TABLE claims ADD COLUMN IF NOT EXISTS reversed_at TIMESTAMPTZ;

-- How and when a denial was closed, and what the payer paid for it. A later
-- remittance that pays a denied line resolves the denial itself
-- (resolution_source 'remittance'), so recovery analytics no longer depend on
-- someone recording the outcome by hand.
ALTER TABLE denials
    ADD COLUMN IF NOT EXISTS resolution_source VARCHAR(20)
        CHECK (resolution_source IN ('user', 'remittance')),
    ADD COLUMN IF NOT EXISTS recovered_amount DECIMAL(12, 2),
    ADD COLUMN IF NOT EXISTS resolved_at TIMESTAMPTZ;

-- idx_denials_natural_key (migration 007) was never added to init.sql, so
-- databases baselined from it have no duplicate guard at all. It is not created
-- here because such a database may already hold duplicates that would make the
-- index fail; ingestion now refuses to insert a copy of an active denial, which
-- covers re-sent files on every database.
