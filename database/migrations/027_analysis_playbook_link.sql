ALTER TABLE ai_analyses ADD COLUMN IF NOT EXISTS playbook_id UUID;
ALTER TABLE ai_analyses DROP CONSTRAINT IF EXISTS ai_analyses_playbook_id_fkey;
ALTER TABLE ai_analyses ADD CONSTRAINT ai_analyses_playbook_id_fkey
    FOREIGN KEY (playbook_id) REFERENCES institutional_playbooks(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_ai_analyses_playbook_id ON ai_analyses(playbook_id);
