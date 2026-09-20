-- IF NOT EXISTS throughout: the app's snapshot-tail startup path re-runs
-- every migration file's raw SQL once, unconditionally, after loading
-- init.sql on a fresh database (see crates/db/src/migrations.rs), and
-- init.sql already carries this. Every migration must tolerate that.
--
-- FB-20 follow-up: the 835 QTY segment quantity (distinct from SVC05 billed
-- units) was parsed into ParsedServiceLine but never reached ParsedDenial,
-- the database, or an API response - it was computed and then dropped.
-- Recorded per denial line so an MUE evidence check has something concrete
-- to compare against a code's per-date-of-service unit limit.
ALTER TABLE denials
    ADD COLUMN IF NOT EXISTS reported_quantity NUMERIC(10, 2),
    ADD COLUMN IF NOT EXISTS quantity_qualifier VARCHAR(10);
