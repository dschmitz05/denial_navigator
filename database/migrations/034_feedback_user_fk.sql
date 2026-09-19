-- Keep feedback history when an account is permanently removed without
-- retaining a dangling reference to the deleted identity.
UPDATE feedback_loop fl
SET user_id = NULL
WHERE fl.user_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM users u WHERE u.id = fl.user_id);

CREATE INDEX IF NOT EXISTS idx_feedback_loop_user_id ON feedback_loop(user_id);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'feedback_loop_user_id_fkey'
          AND conrelid = 'feedback_loop'::regclass
    ) THEN
        ALTER TABLE feedback_loop
            ADD CONSTRAINT feedback_loop_user_id_fkey
            FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL;
    END IF;
END $$;
