-- 012: ICD-10 and CPT reference tables, importable like CARC/RARC.
--
-- Diagnosis codes arrive on claims as a bare list (claims.icd_10_codes) and
-- procedure codes on denials as bare text (denials.cpt_code) - nothing in the
-- app can tell you what E11.9 or 99213 means. These two tables give those
-- codes a home so the same CSV import used for CARC/RARC can keep them
-- current. They start empty and are populated by the first import; the
-- published lists are large (ICD-10-CM is ~70,000 codes), so they are not
-- seeded here.

CREATE TABLE IF NOT EXISTS icd10_codes (
    code           VARCHAR(20) PRIMARY KEY,
    description    TEXT NOT NULL,
    is_active      BOOLEAN NOT NULL DEFAULT TRUE,
    effective_date DATE,
    expiration_date DATE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS cpt_codes (
    code           VARCHAR(20) PRIMARY KEY,
    description    TEXT NOT NULL,
    is_active      BOOLEAN NOT NULL DEFAULT TRUE,
    effective_date DATE,
    expiration_date DATE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ
);

-- The import log's kind list is a CHECK, not an open string: extend it.
ALTER TABLE reference_imports
    DROP CONSTRAINT IF EXISTS reference_imports_kind_check;
ALTER TABLE reference_imports
    ADD CONSTRAINT reference_imports_kind_check
    CHECK (kind IN ('carc', 'rarc', 'icd10', 'cpt'));
