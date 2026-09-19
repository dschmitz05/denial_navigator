ALTER TABLE notifications
    ADD COLUMN IF NOT EXISTS organization_id UUID;

DO $$
DECLARE
    ambiguous_count BIGINT;
BEGIN
    SELECT COUNT(*) INTO ambiguous_count
    FROM notifications n
    WHERE n.organization_id IS NULL
      AND (SELECT COUNT(*) FROM organization_memberships om WHERE om.user_id = n.user_id) <> 1;
    IF ambiguous_count > 0 THEN
        RAISE EXCEPTION 'cannot safely assign organization to % legacy notification(s)', ambiguous_count;
    END IF;

    UPDATE notifications n
    SET organization_id = (
        SELECT om.organization_id FROM organization_memberships om WHERE om.user_id = n.user_id
    )
    WHERE n.organization_id IS NULL;
END $$;

ALTER TABLE notifications
    ALTER COLUMN organization_id SET NOT NULL;

DROP INDEX IF EXISTS idx_notifications_once_per_day;
CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_once_per_day
    ON notifications (organization_id, user_id, kind, for_date);
