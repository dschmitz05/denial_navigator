-- Earlier releases allowed an appeal to be assigned to any active user. Clear
-- legacy assignments whose user is not a member of the claim's organization;
-- the item returns to that organization's unassigned work queue.
UPDATE appeals_queue aq
SET assigned_user_id = NULL,
    updated_at = NOW()
FROM denials d
JOIN claims c ON c.id = d.claim_id
WHERE aq.denial_id = d.id
  AND aq.assigned_user_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM organization_memberships om
      WHERE om.user_id = aq.assigned_user_id
        AND om.organization_id = c.organization_id
  );
