-- ============================================================
-- Denial Navigator — Sample Data
-- ============================================================

-- Sample payer
INSERT INTO claims (claim_number, patient_id, patient_name, date_of_birth,
                    provider_npi, provider_name, payer_name, payer_id_number,
                    total_charge, total_paid, total_adjustment,
                    status, icd_10_codes)
VALUES
('CLM-2024-001', 'PAT-001', 'John Doe', '1965-03-15',
 '1234567890', 'Metropolitan Medical Group', 'BlueCross BlueShield', 'BCBS-987654',
 2500.00, 0.00, 2500.00,
 'denied',
 ARRAY['J18.9', 'R05.2']),

('CLM-2024-002', 'PAT-002', 'Jane Smith', '1978-07-22',
 '1234567890', 'Metropolitan Medical Group', 'Aetna', 'AET-123456',
 1800.00, 1200.00, 600.00,
 'partially_paid',
 ARRAY['M54.5']),

('CLM-2024-003', 'PAT-003', 'Robert Johnson', '1952-11-08',
 '1234567890', 'Metropolitan Medical Group', 'UnitedHealthcare', 'UHC-654321',
 5200.00, 0.00, 5200.00,
 'denied',
 ARRAY['C34.10', 'R04.0']),

('CLM-2024-004', 'PAT-004', 'Emily Davis', '1990-01-30',
 '1234567890', 'Metropolitan Medical Group', 'Medicare', 'CMS-111111',
 3400.00, 0.00, 3400.00,
 'denied',
 ARRAY['E11.65', 'G47.0']),

('CLM-2024-005', 'PAT-005', 'Michael Brown', '1985-06-12',
 '1234567890', 'Metropolitan Medical Group', 'Cigna', 'CIG-222222',
 950.00, 750.00, 200.00,
 'partially_paid',
 ARRAY['K21.0']);

-- Sample denials for CLM-2024-001 (Denial: Missing prior authorization)
INSERT INTO denials (claim_id, service_line_number, cpt_code, charge_amount, payment_amount,
                     adjustment_amount, cagc, carc_code, rarc_code,
                     denial_reason_code, adjustment_reason,
                     denial_date, appeal_deadline)
VALUES
('CLM-2024-001', 1, '99213', 150.00, 0.00, 150.00, 'CO', '150', 'P487',
 'PRIOR_AUTH', 'Prior authorization not obtained',
 CURRENT_DATE - INTERVAL '45 days', CURRENT_DATE + INTERVAL '20 days'),
('CLM-2024-001', 2, '99214', 200.00, 0.00, 200.00, 'CO', '150', 'P487',
 'PRIOR_AUTH', 'Prior authorization not obtained',
 CURRENT_DATE - INTERVAL '45 days', CURRENT_DATE + INTERVAL '20 days'),
('CLM-2024-001', 3, '71046', 800.00, 0.00, 800.00, 'CO', '150', 'P487',
 'PRIOR_AUTH', 'Prior authorization not obtained for chest X-ray',
 CURRENT_DATE - INTERVAL '45 days', CURRENT_DATE + INTERVAL '20 days'),
('CLM-2024-001', 4, '85025', 550.00, 0.00, 550.00, 'CO', '150', 'P487',
 'PRIOR_AUTH', 'Prior authorization not obtained for CBC panel',
 CURRENT_DATE - INTERVAL '45 days', CURRENT_DATE + INTERVAL '20 days'),
('CLM-2024-001', 5, '80053', 800.00, 0.00, 800.00, 'CO', '150', 'P487',
 'PRIOR_AUTH', 'Prior authorization not obtained for CMP panel',
 CURRENT_DATE - INTERVAL '45 days', CURRENT_DATE + INTERVAL '20 days');

-- Sample denials for CLM-2024-002 (Partial payment, contractual adjustment)
INSERT INTO denials (claim_id, service_line_number, cpt_code, charge_amount, payment_amount,
                     adjustment_amount, cagc, carc_code, rarc_code,
                     denial_reason_code, adjustment_reason,
                     denial_date)
VALUES
('CLM-2024-002', 1, '99214', 250.00, 180.00, 70.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment per agreement',
 CURRENT_DATE - INTERVAL '30 days'),
('CLM-2024-002', 2, '36415', 50.00, 35.00, 15.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment per agreement',
 CURRENT_DATE - INTERVAL '30 days'),
('CLM-2024-002', 3, '36416', 300.00, 210.00, 90.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment per agreement',
 CURRENT_DATE - INTERVAL '30 days'),
('CLM-2024-002', 4, '30040', 1200.00, 775.00, 425.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment per agreement',
 CURRENT_DATE - INTERVAL '30 days');

-- Sample denials for CLM-2024-003 (Medical necessity denial)
INSERT INTO denials (claim_id, service_line_number, cpt_code, hcpcs_code, charge_amount, payment_amount,
                     adjustment_amount, cagc, carc_code, rarc_code,
                     denial_reason_code, adjustment_reason,
                     denial_date, appeal_deadline)
VALUES
('CLM-2024-003', 1, '77427', NULL, 2500.00, 0.00, 2500.00, 'CO', '12', 'P482',
 'MEDICAL_NEC', 'Breast radiation therapy not deemed medically necessary',
 CURRENT_DATE - INTERVAL '60 days', CURRENT_DATE + INTERVAL '5 days'),
('CLM-2024-003', 2, '77449', NULL, 1200.00, 0.00, 1200.00, 'CO', '12', 'P482',
 'MEDICAL_NEC', 'Brachytherapy not medically necessary per policy',
 CURRENT_DATE - INTERVAL '60 days', CURRENT_DATE + INTERVAL '5 days'),
('CLM-2024-003', 3, '77306', NULL, 1500.00, 0.00, 1500.00, 'CO', '12', 'P482',
 'MEDICAL_NEC', 'Treatment planning not covered',
 CURRENT_DATE - INTERVAL '60 days', CURRENT_DATE + INTERVAL '5 days');

-- Sample denials for CLM-2024-004 (Lack of information)
INSERT INTO denials (claim_id, service_line_number, cpt_code, hcpcs_code, charge_amount, payment_amount,
                     adjustment_amount, cagc, carc_code, rarc_code,
                     denial_reason_code, adjustment_reason,
                     denial_date, appeal_deadline)
VALUES
('CLM-2024-004', 1, '99214', NULL, 350.00, 0.00, 350.00, 'CO', '16', 'N401',
 'INFORMATION', 'Claim/service lacks information',
 CURRENT_DATE - INTERVAL '40 days', CURRENT_DATE + INTERVAL '15 days'),
('CLM-2024-004', 2, '25555', NULL, 450.00, 0.00, 450.00, 'CO', '16', 'N401',
 'INFORMATION', 'Missing lab code details',
 CURRENT_DATE - INTERVAL '40 days', CURRENT_DATE + INTERVAL '15 days'),
('CLM-2024-004', 3, '95811', NULL, 1200.00, 0.00, 1200.00, 'CO', '16', 'N401',
 'INFORMATION', 'EMG study lacks documentation',
 CURRENT_DATE - INTERVAL '40 days', CURRENT_DATE + INTERVAL '15 days'),
('CLM-2024-004', 4, '95813', NULL, 1400.00, 0.00, 1400.00, 'CO', '16', 'N401',
 'INFORMATION', 'Nerve conduction study lacks details',
 CURRENT_DATE - INTERVAL '40 days', CURRENT_DATE + INTERVAL '15 days');

-- Sample denials for CLM-2024-005
INSERT INTO denials (claim_id, service_line_number, cpt_code, charge_amount, payment_amount,
                     adjustment_amount, cagc, carc_code, rarc_code,
                     denial_reason_code, adjustment_reason,
                     denial_date)
VALUES
('CLM-2024-005', 1, '99213', 150.00, 120.00, 30.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment',
 CURRENT_DATE - INTERVAL '20 days'),
('CLM-2024-005', 2, '80047', 500.00, 400.00, 100.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment',
 CURRENT_DATE - INTERVAL '20 days'),
('CLM-2024-005', 3, '82340', 300.00, 230.00, 70.00, 'CO', '48', NULL,
 'CONTRACTUAL', 'Contractual adjustment',
 CURRENT_DATE - INTERVAL '20 days');

-- Sample AI Analyses
INSERT INTO ai_analyses (denial_id, claim_id, model_name, prompt_tokens, completion_tokens,
                         total_tokens, explanation, denial_category,
                         root_cause_summary, action_plan, steps,
                         needs_appeal, draft_appeal_letter, confidence_score)
VALUES
(
    (SELECT id FROM denials WHERE claim_id = 'CLM-2024-001' AND carc_code = '150' LIMIT 1),
    'CLM-2024-001', 'qwen2.5:7b', 450, 320, 770,
    'This claim was denied because prior authorization was not obtained for the services rendered. BlueCross BlueShield requires pre-authorization for office visits (99213, 99214) and diagnostic procedures (71046, 85025, 80053) when billed together.',
    'administrative',
    'Prior authorization was not obtained before rendering services. This is a documentation/administrative error that can be corrected.',
    '{"type": "corrected_claim", "requires": "retroactive_prior_auth"}',
    '[{"step": 1, "action": "Contact BlueCross BlueShield prior authorization department"},
      {"step": 2, "action": "Request retroactive authorization citing clinical necessity"},
      {"step": 3, "action": "Submit supporting clinical documentation (notes, labs, imaging)"},
      {"step": 4, "action": "Resubmit claim with new authorization number"},
      {"step": 5, "action": "Monitor for adjudication within 30 days"}]',
    TRUE,
    'RE: Request for Retroactive Prior Authorization\n\nClaim ID: CLM-2024-001\nPatient: John Doe (DOB: 1965-03-15)\nPayer: BlueCross BlueShield (ID: BCBS-987654)\n\nDear BlueCross BlueShield Claims Department,\n\nI am writing to request retroactive prior authorization for the above-referenced claim.\n\nThe patient presented on [date of service] with [clinical presentation]. The services rendered (CPT codes: 99213, 99214, 71046, 85025, 80053) were medically necessary and consistent with standard of care.\n\nAttached clinical documentation supports the medical necessity of these services.\n\nWe respectfully request reconsideration and payment of this claim.\n\nSincerely,\n[Provider Name]\n[Contact Information]',
    0.85
),

(
    (SELECT id FROM denials WHERE claim_id = 'CLM-2024-003' LIMIT 1),
    'CLM-2024-003', 'qwen2.5:7b', 520, 480, 1000,
    'This claim was denied on medical necessity grounds. UnitedHealthcare denied coverage for breast radiation therapy (77427), brachytherapy (77449), and treatment planning (77306), stating these services were not deemed medically necessary per their medical policy MP-2024-0156.',
    'medical_necessity',
    'The payer determined that radiation therapy for this patient''s condition does not meet their medical necessity criteria. This requires clinical documentation and potentially an appeal with peer-to-peer review.',
    '{"type": "appeal_with_clinical_docs", "requires": "peer_to_peer_review"}',
    '[{"step": 1, "action": "Obtain the full UnitedHealthcare medical policy MP-2024-0156"},
      {"step": 2, "action": "Gather pathology reports, imaging, and oncology notes"},
      {"step": 3, "action": "Document medical necessity per UHC criteria"},
      {"step": 4, "action": "Request peer-to-peer review with UHC medical director"},
      {"step": 5, "action": "Submit formal appeal with all clinical documentation"},
      {"step": 6, "action": "If denied, escalate to external review per state regulations"}]',
    TRUE,
    'RE: Formal Appeal — Medical Necessity Review\n\nClaim ID: CLM-2024-003\nPatient: Robert Johnson (DOB: 1952-11-08)\nPayer: UnitedHealthcare (ID: UHC-654321)\nDiagnosis: C34.10 (Malignant neoplasm of upper lobe, right lung)\n\nDear UnitedHealthcare Medical Review Department,\n\nI am writing to formally appeal the denial of radiation therapy services for the above-referenced patient.\n\nThe patient has been diagnosed with Stage [X] non-small cell lung cancer of the right upper lobe. Following multidisciplinary tumor board review, radiation therapy was recommended as the standard of care per NCCN Clinical Practice Guidelines.\n\nAttached clinical documentation includes:\n- Pathology report confirming diagnosis\n- Imaging studies (CT/PET scan)\n- Tumor board discussion notes\n- NCCN guideline citations supporting treatment\n- Oncology progress notes\n\nThe treatment plan follows evidence-based medicine and meets all established medical necessity criteria.\n\nWe request an expedited peer-to-peer review and approval of these medically necessary services.\n\nSincerely,\n[Provider Name], MD\n[Credentials]\n[Contact Information]',
    0.78
);

-- Sample appeals queue
INSERT INTO appeals_queue (denial_id, claim_id, resolution_type, outcome_status, notes)
VALUES
((SELECT id FROM denials WHERE claim_id = 'CLM-2024-001' AND carc_code = '150' LIMIT 1),
 'CLM-2024-001', 'appeal_letter', 'queued',
 'High priority — appeal deadline in 20 days. Retroactive prior auth request needed.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-003' LIMIT 1),
 'CLM-2024-003', 'clinical_docs', 'queued',
 'Critical — appeal deadline in 5 days. Requires oncology documentation and peer-to-peer review request.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-004' LIMIT 1),
 'CLM-2024-004', 'corrected_claim', 'queued',
 'Missing information — update claim with complete lab codes and EMG/NCS details.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-001'),
 'CLM-2024-001', 'corrected_claim', 'in_progress',
 'Remaining service line denials for prior auth issue — batch processing.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-004'),
 'CLM-2024-004', 'corrected_claim', 'in_progress',
 'Remaining service line denials — need complete documentation.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-003' ORDER BY id DESC LIMIT 1),
 'CLM-2024-003', 'appeal_letter', 'queued',
 'Second service line — brachytherapy medical necessity appeal.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-003' ORDER BY id DESC LIMIT 1),
 'CLM-2024-003', 'appeal_letter', 'queued',
 'Third service line — treatment planning appeal.'),

((SELECT id FROM denials WHERE claim_id = 'CLM-2024-004' ORDER BY id DESC LIMIT 1),
 'CLM-2024-004', 'corrected_claim', 'queued',
 'Fourth service line — nerve conduction study documentation needed.');

-- Knowledge document samples
INSERT INTO knowledge_documents (title, source_type, effective_date, status, metadata)
VALUES
('BlueCross BlueShield Prior Authorization Requirements 2024', 'payer_policy', '2024-01-01', 'indexed',
 '{"payer": "BlueCross BlueShield", "version": "2024.1", "sections": ["office_visits", "imaging", "labs"]}'),

('UnitedHealthcare Medical Policy MP-2024-0156: Radiation Oncology', 'payer_policy', '2024-03-01', 'indexed',
 '{"payer": "UnitedHealthcare", "policy_number": "MP-2024-0156", "category": "radiation_therapy"}'),

('CMS Local Coverage Determination — Radiation Therapy (L33944)', 'cms_lcd', '2023-10-01', 'indexed',
 {"cms_number": "L33944", "topic": "radiation_therapy", "region": "all"}),

('Aetna Clinical Policy Bulletin — Office Visit Level of Service', 'payer_policy', '2024-02-15', 'indexed',
 '{"payer": "Aetna", "category": "evaluation_management"}'),

('Cigna Prior Authorization Guideline — Diagnostic Imaging', 'payer_policy', '2024-01-20', 'indexed',
 '{"payer": "Cigna", "category": "imaging"}');
