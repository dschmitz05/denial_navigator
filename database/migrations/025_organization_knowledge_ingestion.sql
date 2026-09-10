-- Tenant ownership for records not inherently linked to a claim.
ALTER TABLE knowledge_documents ADD COLUMN IF NOT EXISTS organization_id UUID;
UPDATE knowledge_documents
SET organization_id = '00000000-0000-0000-0000-000000000001'
WHERE organization_id IS NULL;
ALTER TABLE knowledge_documents ALTER COLUMN organization_id SET NOT NULL;
ALTER TABLE knowledge_documents ALTER COLUMN organization_id
    SET DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE knowledge_documents DROP CONSTRAINT IF EXISTS knowledge_documents_organization_id_fkey;
ALTER TABLE knowledge_documents ADD CONSTRAINT knowledge_documents_organization_id_fkey
    FOREIGN KEY (organization_id) REFERENCES organizations(id);
CREATE INDEX IF NOT EXISTS idx_knowledge_documents_organization_id
    ON knowledge_documents(organization_id);

ALTER TABLE ingestion_log ADD COLUMN IF NOT EXISTS organization_id UUID;
UPDATE ingestion_log
SET organization_id = '00000000-0000-0000-0000-000000000001'
WHERE organization_id IS NULL;
ALTER TABLE ingestion_log ALTER COLUMN organization_id SET NOT NULL;
ALTER TABLE ingestion_log ALTER COLUMN organization_id
    SET DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE ingestion_log DROP CONSTRAINT IF EXISTS ingestion_log_organization_id_fkey;
ALTER TABLE ingestion_log ADD CONSTRAINT ingestion_log_organization_id_fkey
    FOREIGN KEY (organization_id) REFERENCES organizations(id);
CREATE INDEX IF NOT EXISTS idx_ingestion_log_organization_created
    ON ingestion_log(organization_id, created_at DESC);

ALTER TABLE claims DROP CONSTRAINT IF EXISTS claims_claim_number_key;
DROP INDEX IF EXISTS idx_claims_claim_number;
CREATE UNIQUE INDEX IF NOT EXISTS idx_claims_organization_claim_number
    ON claims(organization_id, claim_number);
