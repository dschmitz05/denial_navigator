"""
Denial Analysis Prompt Builder
Constructs context-rich prompts for the LLM reasoning engine
"""


async def build_denial_prompt(
    claim_id: str,
    payer_name: str,
    cpt_code: str,
    icd10_code: str,
    cagc: str,
    carc_code: str,
    carc_definition: str,
    rarc_code: str,
    rarc_definition: str,
    retrieved_policies: list[str],
) -> dict:
    """
    Build system and user prompts for denial analysis.

    Returns dict with 'system' and 'user' keys.
    """

    system_prompt = """You are an expert Revenue Cycle Management (RCM) Denial Analyst with deep knowledge of:
- ANSI X12 EDI 835/837 transactions
- CPT, HCPCS, and ICD-10 coding
- CMS Local Coverage Determinations (LCDs) and National Coverage Determinations (NCDs)
- Payer-specific policies and medical necessity criteria
- WPC Claim Adjustment Reason Codes (CARC) and Remittance Advice Remark Codes (RARC)
- Revenue cycle denial management and appeals best practices

Your task is to analyze claim denial data and produce actionable resolution steps.

CRITICAL: Your response MUST be valid JSON with these exact fields:
- explanation: Plain-language WHY the claim was denied, written for a biller, not a coder
- denial_category: One of [coding_error, missing_info, lack_of_preauth, medical_necessity, bundled_service, duplicate_claim, timely_filing, non_covered_service, patient_responsibility, other]
- required_action: EXACTLY one of [coding_correction, clinical_documentation, appeal, bill_patient, no_action_required]
  - coding_correction: fixable by re-coding and resubmitting (modifier, CPT/ICD mismatch, bundling)
  - clinical_documentation: needs records, notes or medical justification before resubmission
  - appeal: the payer's decision must be formally challenged
  - bill_patient: correctly adjudicated, and the balance is the PATIENT's under their
    benefit design - deductible not yet met, coinsurance, copay, non-covered service the
    patient is liable for. The money is still collectible; it moves to patient billing.
  - no_action_required: correctly adjudicated and NOT collectible from anyone - a
    contractual write-off the provider absorbs under the payer agreement.

CRITICAL distinction: a PR (Patient Responsibility) adjustment is NOT a write-off. PR
means the payer has assigned that balance to the patient, so required_action MUST be
bill_patient. Reserve no_action_required for CO (Contractual Obligation) adjustments,
where the provider is contractually barred from billing anyone for the difference.
Calling a deductible a write-off tells the billing team to abandon money they are
entitled to collect.
- root_cause_summary: 1-2 sentence root cause
- action_plan: Object with 'type' and 'requires' keys
- steps: Array of numbered step objects with 'step' and 'action' keys. Be explicit and
  concrete - each action must be something a billing team member can carry out without
  further interpretation.
- needs_appeal: Boolean, true only when required_action is "appeal"
- draft_appeal_letter: A complete appeal letter when needs_appeal is true, otherwise an
  empty string. Cite the CARC/RARC codes and any payer policy text supplied above.
- confidence_score: Float 0.0-1.0

Ground your answer in the supplied CARC/RARC definitions and payer policy text. If no
payer policy was retrieved, say so in the explanation rather than inventing one.

Return ONLY the JSON object. No markdown formatting, no explanatory text."""

    cagc_descriptions = {
        "PR": "Patient Responsibility",
        "CO": "Contractual Obligation",
        "OA": "Other Adjustment",
        "AB": "Absence of Documentation",
        "AS": "Assignment of Benefits",
        "PI": "Internal/Payer Investigation",
    }
    cagc_desc = cagc_descriptions.get(cagc, "Unknown Adjustment Group")

    retrieved_context = "\n\n---\n\n".join(retrieved_policies) if retrieved_policies else "No specific payer policy retrieved for this denial."

    user_prompt = f"""## Claim Parameters
- **Claim ID**: {claim_id}
- **Payer**: {payer_name}
- **CPT Code**: {cpt_code}
- **ICD-10 Code**: {icd10_code}

## Denial Codes
- **CAGC**: {cagc} — {cagc_desc}
- **CARC**: {carc_code} — {carc_definition}
- **RARC**: {rarc_code} — {rarc_definition}

## Retrieved Payer Policies
{retrieved_context}

## Requirements
1. Explain in simple terms WHY the claim was denied.
2. Identify whether resolution requires a coding correction, clinical documentation, an
   appeal, billing the patient, or nothing at all. If the CAGC above is PR, the balance
   belongs to the patient: required_action is bill_patient, never no_action_required.
3. List explicit step-by-step instructions for the billing team to resolve the issue.
4. Draft an initial appeal letter if an appeal is warranted.

Produce the structured JSON response described above."""

    return {"system": system_prompt, "user": user_prompt}
