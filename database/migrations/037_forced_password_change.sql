-- FB-04: an account whose password someone else set (the seeded default, an
-- admin registration or an admin reset) must choose its own at next sign-in.
ALTER TABLE users ADD COLUMN IF NOT EXISTS must_change_password BOOLEAN NOT NULL DEFAULT FALSE;

-- Accounts still on the documented default password. The seed hashed it with
-- pgcrypto ($2a$); other hashes are skipped rather than risk crypt() failing
-- on a format it does not read.
CREATE EXTENSION IF NOT EXISTS pgcrypto;
UPDATE users
SET must_change_password = TRUE
WHERE password_hash LIKE '$2a$%'
  AND password_hash = crypt('admin123', password_hash);
