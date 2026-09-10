ALTER TABLE users
    ADD COLUMN IF NOT EXISTS oidc_subject VARCHAR(255);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_oidc_subject
    ON users(oidc_subject)
    WHERE oidc_subject IS NOT NULL;
