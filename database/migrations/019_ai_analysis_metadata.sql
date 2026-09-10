ALTER TABLE ai_analyses
    ADD COLUMN IF NOT EXISTS provider_name VARCHAR(100),
    ADD COLUMN IF NOT EXISTS provider_version VARCHAR(100),
    ADD COLUMN IF NOT EXISTS prompt_template_version VARCHAR(100);
