ALTER TABLE claims ADD COLUMN IF NOT EXISTS facility_type_code VARCHAR(20);
CREATE INDEX IF NOT EXISTS idx_claims_facility_type_code ON claims(facility_type_code);
