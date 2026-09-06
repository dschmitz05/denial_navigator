-- ============================================================
-- 005 — TOTP two-factor authentication
--
-- Three pieces of state, deliberately separate:
--
--   totp_required     an administrator has decided this account needs 2FA.
--   totp_secret       the shared secret, encrypted. NULL until enrolment.
--   totp_confirmed_at when the user proved they hold the device.
--
-- Required-but-not-confirmed is the enrolment state: the account must set up
-- an authenticator before it can sign in again. Resetting a lost device is
-- clearing the secret, which drops the account back into that state without
-- an administrator ever seeing the secret itself.
--
-- totp_last_used_step blocks replay: a code is valid for a 30-second window,
-- so without recording the step it just satisfied, the same code works twice.
--
-- Idempotent.
-- ============================================================

ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_required BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_secret TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_confirmed_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_last_used_step BIGINT;

COMMENT ON COLUMN users.totp_secret IS
    'Fernet-encrypted TOTP secret. Never returned by the API after enrolment.';
COMMENT ON COLUMN users.totp_last_used_step IS
    'Last accepted 30-second step, so a code cannot be replayed within its window.';

CREATE INDEX IF NOT EXISTS idx_users_totp_required ON users(totp_required) WHERE totp_required;
