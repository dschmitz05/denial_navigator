-- ============================================================
-- 004 — index supporting login throttling
--
-- Failed sign-ins are counted out of audit_log so the limit is shared by every
-- uvicorn worker; an in-process counter gave each of the four workers its own
-- budget, so the real allowance was four times the stated one.
--
-- This query runs on every sign-in attempt and audit_log grows without bound,
-- so it needs an index rather than a scan.
--
-- Idempotent.
-- ============================================================

CREATE INDEX IF NOT EXISTS idx_audit_log_login_attempts
    ON audit_log (action, created_at DESC)
    WHERE action IN ('login', 'login_failed', 'login_blocked');
