-- IF NOT EXISTS throughout: the app's snapshot-tail startup path re-runs
-- every migration file's raw SQL once, unconditionally, after loading
-- init.sql on a fresh database (see crates/db/src/migrations.rs), and
-- init.sql already carries these tables. Every migration must tolerate that.
-- FB-20: CMS NCCI reference edits, used only for post-denial explanation.
CREATE TABLE IF NOT EXISTS ncci_ptp_edits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    column_1_code VARCHAR(20) NOT NULL, column_2_code VARCHAR(20) NOT NULL,
    modifier_indicator SMALLINT NOT NULL CHECK (modifier_indicator IN (0, 1, 9)),
    effective_date DATE, termination_date DATE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (column_1_code, column_2_code, effective_date)
);
CREATE INDEX IF NOT EXISTS idx_ncci_ptp_pair ON ncci_ptp_edits(column_1_code, column_2_code);
CREATE TABLE IF NOT EXISTS ncci_mue_edits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(), code VARCHAR(20) NOT NULL,
    mue_value INTEGER NOT NULL CHECK (mue_value > 0),
    adjudication_indicator SMALLINT CHECK (adjudication_indicator IN (1, 2, 3)),
    effective_date DATE, termination_date DATE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (code, effective_date)
);
CREATE INDEX IF NOT EXISTS idx_ncci_mue_code ON ncci_mue_edits(code);
