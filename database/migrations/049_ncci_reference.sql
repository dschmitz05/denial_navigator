-- FB-20: CMS NCCI reference edits, used only for post-denial explanation.
CREATE TABLE ncci_ptp_edits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    column_1_code VARCHAR(20) NOT NULL, column_2_code VARCHAR(20) NOT NULL,
    modifier_indicator SMALLINT NOT NULL CHECK (modifier_indicator IN (0, 1, 9)),
    effective_date DATE, termination_date DATE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (column_1_code, column_2_code, effective_date)
);
CREATE INDEX idx_ncci_ptp_pair ON ncci_ptp_edits(column_1_code, column_2_code);
CREATE TABLE ncci_mue_edits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(), code VARCHAR(20) NOT NULL,
    mue_value INTEGER NOT NULL CHECK (mue_value > 0),
    adjudication_indicator SMALLINT CHECK (adjudication_indicator IN (1, 2, 3)),
    effective_date DATE, termination_date DATE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (code, effective_date)
);
CREATE INDEX idx_ncci_mue_code ON ncci_mue_edits(code);
