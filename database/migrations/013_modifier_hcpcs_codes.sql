-- 013: Modifier and HCPCS Level II reference tables, importable like the rest.
--
-- Modifier codes (two characters, e.g. 25, LT) and HCPCS Level II codes
-- (alphanumeric, e.g. S1000, J9090) complete the reference set alongside
-- CARC, RARC, ICD-10 and CPT. Like icd10_codes and cpt_codes, the tables
-- start EMPTY: the published lists are maintained by CMS/AMA and are
-- populated through the reference import endpoint (Settings → Reference
-- codes). The reference_imports kind check gains 'modifier' and 'hcpcs'.

CREATE TABLE IF NOT EXISTS modifier_codes (
    code           VARCHAR(20) PRIMARY KEY,
    description    TEXT NOT NULL,
    is_active      BOOLEAN NOT NULL DEFAULT TRUE,
    effective_date DATE,
    expiration_date DATE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS hcpcs_codes (
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
    CHECK (kind IN ('carc', 'rarc', 'icd10', 'cpt', 'modifier', 'hcpcs'));
