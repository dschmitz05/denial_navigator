-- ============================================================
-- 010 — fix the denials.modfier_1 column-name typo
--
-- It is spelled "modfier_1" in init.sql and therefore in the live schema.
-- Every reader has to reproduce the typo to work, which is the kind of thing
-- that stays wrong for years and produces a silent NULL the first time
-- someone writes it correctly from memory.
--
-- Renamed to modifier_1 to match modifier_2 beside it. Idempotent: safe to
-- run against a database that has already been renamed, and against one
-- created fresh from a corrected init.sql.
-- ============================================================

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_name = 'denials' AND column_name = 'modfier_1')
       AND NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_name = 'denials' AND column_name = 'modifier_1')
    THEN
        ALTER TABLE denials RENAME COLUMN modfier_1 TO modifier_1;
    END IF;
END $$;
