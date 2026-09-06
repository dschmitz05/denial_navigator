-- ============================================================
-- 008 — notifications
--
-- Filing deadlines existed but only surfaced to whoever opened the dashboard.
-- A warning nobody sees does not prevent the thing it warns about.
--
-- Delivered in-app rather than by email, because an air-gapped deployment may
-- have no mail path at all and a notification that silently fails to send is
-- worse than one that waits to be read. Email can be layered on top later; the
-- record of what was raised lives here either way.
--
-- The unique index is what makes generation idempotent: the digest runs from
-- cron and the API runs four workers, so "once per user per kind per day" has
-- to be enforced by the database rather than by hoping it is called once.
--
-- Idempotent.
-- ============================================================

CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL,
    kind VARCHAR(50) NOT NULL,           -- 'deadline_digest', 'overdue_escalation'
    -- The day the notification is FOR, not when it was written, so a retry an
    -- hour later does not produce a second copy.
    for_date DATE NOT NULL DEFAULT CURRENT_DATE,
    title VARCHAR(255) NOT NULL,
    body TEXT,
    -- What it is about, so the interface can link straight to the work.
    payload JSONB,
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_once_per_day
    ON notifications (user_id, kind, for_date);

CREATE INDEX IF NOT EXISTS idx_notifications_unread
    ON notifications (user_id, created_at DESC) WHERE read_at IS NULL;
