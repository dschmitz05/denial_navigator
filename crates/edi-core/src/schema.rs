//! Parsed EDI data structures. Field names are the contract the api-gateway
//! reads in `_clean_claim` / `_clean_denial`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ParsedPaymentInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_handling_code: Option<String>,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub payment_amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_debit_flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_id_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payee_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payee_npi: Option<String>,
    /// PLB adjustments apply to the provider/payment, never a patient claim.
    pub provider_adjustments: Vec<ParsedProviderAdjustment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ParsedProviderAdjustment {
    pub provider_identifier: String,
    pub fiscal_period_date: Option<String>,
    pub adjustment_reason_code: String,
    pub reference_number: Option<String>,
    pub amount: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedClaimAdjustment {
    #[serde(default = "default_level")]
    pub adjustment_group_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remark_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "is_default_level")]
    pub level: String,
}

fn default_level() -> String {
    "claim".to_string()
}

fn is_default_level(v: &str) -> bool {
    v == "claim"
}

impl Default for ParsedClaimAdjustment {
    fn default() -> Self {
        Self {
            adjustment_group_code: String::new(),
            reason_code: None,
            amount: 0.0,
            quantity: None,
            remark_code: None,
            description: None,
            level: "claim".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedServiceLine {
    pub line_number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub procedure_qualifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpt_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hcpcs_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_3: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_4: Option<String>,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub charge_amount: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub paid_amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revenue_code: Option<String>,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub units: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_amount: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_number: Option<String>,
    pub remark_codes: Vec<String>,
}

impl Default for ParsedServiceLine {
    fn default() -> Self {
        Self {
            line_number: 0,
            procedure_qualifier: None,
            cpt_code: None,
            hcpcs_code: None,
            modifier_1: None,
            modifier_2: None,
            modifier_3: None,
            modifier_4: None,
            charge_amount: 0.0,
            paid_amount: 0.0,
            revenue_code: None,
            units: 1.0,
            allowed_amount: None,
            service_date: None,
            control_number: None,
            remark_codes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ParsedServiceLineDetail {
    pub service_line: ParsedServiceLine,
    pub adjustments: Vec<ParsedClaimAdjustment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedClaim {
    pub claim_id: String,
    pub claim_status_code: String,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_charged: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_paid: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub patient_responsibility: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_adjustment: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_filing_indicator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_claim_control_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility_type_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_code: Option<String>,
    #[serde(default = "default_claim_type")]
    pub claim_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patient_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patient_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_of_birth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_npi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer_id_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_to: Option<String>,
    pub diagnosis_codes: Vec<String>,
    pub service_lines: Vec<ParsedServiceLineDetail>,
    pub claim_level_adjustments: Vec<ParsedClaimAdjustment>,
    pub remark_codes: Vec<String>,
    /// A payer that pays after this one: another subscriber on the 837 with a
    /// later responsibility sequence, or the 835's crossover carrier (NM1*TT).
    /// A patient-responsibility balance goes there before the patient.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_payer_name: Option<String>,
    /// `837_other_subscriber` or `835_crossover`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_payer_source: Option<String>,
}

fn default_claim_type() -> String {
    "professional".to_string()
}

impl Default for ParsedClaim {
    fn default() -> Self {
        Self {
            claim_id: String::new(),
            claim_status_code: String::new(),
            total_charged: 0.0,
            total_paid: 0.0,
            patient_responsibility: 0.0,
            total_adjustment: 0.0,
            claim_filing_indicator: None,
            payer_claim_control_number: None,
            facility_type_code: None,
            frequency_code: None,
            claim_type: "professional".to_string(),
            patient_id: None,
            patient_name: None,
            date_of_birth: None,
            provider_npi: None,
            provider_name: None,
            payer_name: None,
            payer_id_number: None,
            service_from: None,
            service_to: None,
            diagnosis_codes: Vec::new(),
            service_lines: Vec::new(),
            claim_level_adjustments: Vec::new(),
            remark_codes: Vec::new(),
            next_payer_name: None,
            next_payer_source: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedDenial {
    pub claim_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_line_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpt_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hcpcs_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_2: Option<String>,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub charge_amount: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub payment_amount: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub adjustment_amount: f64,
    pub cagc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carc_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rarc_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_date: Option<String>,
}

impl Default for ParsedDenial {
    fn default() -> Self {
        Self {
            claim_id: String::new(),
            service_line_number: None,
            cpt_code: None,
            hcpcs_code: None,
            modifier_1: None,
            modifier_2: None,
            charge_amount: 0.0,
            payment_amount: 0.0,
            adjustment_amount: 0.0,
            cagc: String::new(),
            carc_code: None,
            rarc_code: None,
            denial_reason: None,
            denial_date: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ParsedTransactionMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_set_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_set_control_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation_convention_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interchange_sender_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interchange_receiver_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functional_group_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_separator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_separator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_terminator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Parsed835Response {
    pub metadata: ParsedTransactionMetadata,
    pub payment_info: ParsedPaymentInfo,
    pub claims: Vec<ParsedClaim>,
    pub denials: Vec<ParsedDenial>,
    pub total_claims: usize,
    pub total_denials: usize,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_charges: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_payments: f64,
    #[serde(skip_serializing_if = "is_default_f64")]
    pub total_adjustments: f64,
    pub warnings: Vec<String>,
}

fn is_default_f64(v: &f64) -> bool {
    *v == 0.0
}
