-- Trigram indexes for the substring (ILIKE '%term%') searches on the claims,
-- denials and reference-code list endpoints. A btree cannot serve a leading
-- wildcard, so these were sequential scans.
--
-- IF NOT EXISTS throughout: the snapshot-tail startup path re-runs every
-- migration's raw SQL on a fresh database (see crates/db/src/migrations.rs).
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_claims_claim_number_trgm ON claims USING gin (claim_number gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_claims_patient_name_trgm ON claims USING gin (patient_name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_claims_patient_id_trgm ON claims USING gin (patient_id gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_denials_cpt_code_trgm ON denials USING gin (cpt_code gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_denials_carc_code_trgm ON denials USING gin (carc_code gin_trgm_ops);
