-- ============================================================
-- 003 — users.sessions_valid_from
--
-- A JWT was accepted on signature and expiry alone, so a token stayed usable
-- for its full 8 hours after the account was deactivated, its password reset,
-- or the account deleted outright. Firing someone did not end their access to
-- patient data, and neither did responding to a stolen password.
--
-- Tokens now carry an issued-at claim, and any token issued before this
-- timestamp is refused. Bumped on password change and password reset; the
-- deactivation and deletion cases are handled by checking the row itself.
--
-- Idempotent.
-- ============================================================

ALTER TABLE users ADD COLUMN IF NOT EXISTS sessions_valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Whole seconds, to match the JWT's iat claim.
--
-- iat is an integer count of seconds, truncated DOWN, so a token minted at
-- 10:00:00.9 carries 10:00:00. Compared against a microsecond-precision
-- timestamp of 10:00:00.7 it looks older than a value recorded before it, and
-- the session is revoked the instant it is created. Storing this truncated
-- puts both sides on the same granularity. The cost is that a token minted
-- earlier in the same second as a revocation survives it - a sub-second
-- window, against sessions that otherwise lasted eight hours.
ALTER TABLE users ALTER COLUMN sessions_valid_from SET DEFAULT date_trunc('second', NOW());
UPDATE users SET sessions_valid_from = date_trunc('second', sessions_valid_from)
 WHERE sessions_valid_from <> date_trunc('second', sessions_valid_from);

COMMENT ON COLUMN users.sessions_valid_from IS
    'Tokens issued before this moment are rejected. Bump to sign a user out everywhere.';
