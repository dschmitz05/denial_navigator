-- FB-06: a payer that pays after this claim's payer (another subscriber on the
-- 837, or the 835's crossover carrier). A patient-responsibility balance on
-- such a claim goes to that payer before the patient is billed.
ALTER TABLE claims
    ADD COLUMN IF NOT EXISTS next_payer_name VARCHAR(255),
    ADD COLUMN IF NOT EXISTS next_payer_source VARCHAR(30)
        CHECK (next_payer_source IN ('837_other_subscriber', '835_crossover'));
