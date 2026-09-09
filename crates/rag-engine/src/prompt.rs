//! Denial analysis prompt builder.
//! Ported from `rag-engine/prompts/denial_analysis.py`.

/// The fields needed to build a denial analysis prompt. Grouped into a struct
/// so the builder stays under the argument-count lint.
pub struct DenialPromptInput {
    pub claim_id: String,
    pub payer_name: String,
    pub cpt_code: String,
    pub icd10_code: String,
    pub cagc: String,
    pub carc_code: String,
    pub carc_definition: String,
    pub rarc_code: String,
    pub rarc_definition: String,
    pub retrieved_policies: Vec<String>,
}

fn cagc_description(cagc: &str) -> &'static str {
    match cagc {
        "PR" => "Patient Responsibility",
        "CO" => "Contractual Obligation",
        "OA" => "Other Adjustment",
        "AB" => "Absence of Documentation",
        "AS" => "Assignment of Benefits",
        "PI" => "Internal/Payer Investigation",
        _ => "Unknown Adjustment Group",
    }
}

/// Build the `(system, user)` prompt pair for denial analysis.
pub fn build_denial_prompt(i: &DenialPromptInput) -> (String, String) {
    let system_prompt = "You are an expert Revenue Cycle Management (RCM) Denial Analyst with deep knowledge of:\n\
- ANSI X12 EDI 835/837 transactions\n\
- CPT, HCPCS, and ICD-10 coding\n\
- CMS Local Coverage Determinations (LCDs) and National Coverage Determinations (NCDs)\n\
- Payer-specific policies and medical necessity criteria\n\
- WPC Claim Adjustment Reason Codes (CARC) and Remittance Advice Remark Codes (RARC)\n\
- Revenue cycle denial management and appeals best practices\n\
\n\
Your task is to analyze claim denial data and produce actionable resolution steps.\n\
\n\
CRITICAL: Your response MUST be valid JSON with these exact fields:\n\
- explanation: Plain-language WHY the claim was denied, written for a biller, not a coder\n\
- denial_category: One of [coding_error, missing_info, lack_of_preauth, medical_necessity, bundled_service, duplicate_claim, timely_filing, non_covered_service, patient_responsibility, other]\n\
- required_action: EXACTLY one of [coding_correction, clinical_documentation, appeal, bill_patient, no_action_required]\n\
  - coding_correction: fixable by re-coding and resubmitting (modifier, CPT/ICD mismatch, bundling)\n\
  - clinical_documentation: needs records, notes or medical justification before resubmission\n\
  - appeal: the payer's decision must be formally challenged\n\
  - bill_patient: correctly adjudicated, and the balance is the PATIENT's under their\n\
    benefit design - deductible not yet met, coinsurance, copay, non-covered service the\n\
    patient is liable for. The money is still collectible; it moves to patient billing.\n\
  - no_action_required: correctly adjudicated and NOT collectible from anyone - a\n\
    contractual write-off the provider absorbs under the payer agreement.\n\
\n\
The category and the action must agree. If you have identified something the\n\
provider can act on, the action cannot be \"nothing to do\":\n\
  - coding_error, missing_info, bundled_service -> coding_correction\n\
  - lack_of_preauth, medical_necessity          -> clinical_documentation or appeal\n\
  - patient_responsibility                      -> bill_patient\n\
Reserve no_action_required for a denial where nothing is recoverable by anyone:\n\
a contractual write-off, or a confirmed duplicate. A denial that says\n\
information is missing is telling you what to supply - recommending a write-off\n\
there tells a billing team to abandon a claim the payer has just explained how\n\
to fix.\n\
\n\
CRITICAL distinction: a PR (Patient Responsibility) adjustment is NOT a write-off. PR\n\
means the payer has assigned that balance to the patient, so required_action MUST be\n\
bill_patient. Reserve no_action_required for CO (Contractual Obligation) adjustments,\n\
where the provider is contractually barred from billing anyone for the difference.\n\
Calling a deductible a write-off tells the billing team to abandon money they are\n\
entitled to collect.\n\
- root_cause_summary: 1-2 sentence root cause\n\
- action_plan: Object with 'type' and 'requires' keys\n\
- steps: Array of numbered step objects with 'step' and 'action' keys. Be explicit and\n\
  concrete - each action must be something a billing team member can carry out without\n\
  further interpretation.\n\
- needs_appeal: Boolean, true only when required_action is \"appeal\"\n\
- draft_appeal_letter: A complete appeal letter when needs_appeal is true, otherwise an\n\
  empty string. Cite the CARC/RARC codes and any payer policy text supplied above.\n\
- confidence_score: Float 0.0-1.0\n\
\n\
Ground your answer in the supplied CARC/RARC definitions and payer policy text. If no\n\
payer policy was retrieved, say so in the explanation rather than inventing one.\n\
\n\
Return ONLY the JSON object. No markdown formatting, no explanatory text.";

    let retrieved_context = if i.retrieved_policies.is_empty() {
        "No specific payer policy retrieved for this denial.".to_string()
    } else {
        i.retrieved_policies.join("\n\n---\n\n")
    };

    let user_prompt = format!(
        "## Claim Parameters\n\
 - **Claim ID**: {}\n\
 - **Payer**: {}\n\
 - **CPT Code**: {}\n\
 - **ICD-10 Code**: {}\n\
 \n\
 ## Denial Codes\n\
 - **CAGC**: {} — {}\n\
 - **CARC**: {} — {}\n\
 - **RARC**: {} — {}\n\
 \n\
 ## Retrieved Payer Policies\n\
 {}\n\
 \n\
 ## Requirements\n\
 1. Explain in simple terms WHY the claim was denied.\n\
 2. Identify whether resolution requires a coding correction, clinical documentation, an\n\
    appeal, billing the patient, or nothing at all. If the CAGC above is PR, the balance\n\
    belongs to the patient: required_action is bill_patient, never no_action_required.\n\
 3. List explicit step-by-step instructions for the billing team to resolve the issue.\n\
 4. Draft an initial appeal letter if an appeal is warranted.\n\
 \n\
 Produce the structured JSON response described above.",
        i.claim_id,
        i.payer_name,
        i.cpt_code,
        i.icd10_code,
        i.cagc,
        cagc_description(&i.cagc),
        i.carc_code,
        i.carc_definition,
        i.rarc_code,
        i.rarc_definition,
        retrieved_context,
    );

    (system_prompt.to_string(), user_prompt)
}
