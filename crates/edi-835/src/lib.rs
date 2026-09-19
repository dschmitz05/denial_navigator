//! X12 835 remittance parser and the ST01 dispatcher shared with 837.

use std::collections::HashSet;
use std::mem;

use chrono::Local;

use denial_edi_837::parse_837;
use denial_edi_core::common::{
    detect_delimiters, one_or, opt, person_name, round2, to_date, to_float, tokenize_segments,
    validate_envelopes, Delimiters, Segment,
};
use denial_edi_core::schema::{
    Parsed835Response, ParsedClaim, ParsedClaimAdjustment, ParsedDenial, ParsedPaymentInfo,
    ParsedProviderAdjustment, ParsedServiceLine, ParsedServiceLineDetail,
    ParsedTransactionMetadata,
};

/// The denials table constrains cagc to this set; emitting anything else
/// would fail the insert, so adjustments outside it are not turned into
/// denial rows (they remain visible on the claim).
const VALID_GROUP_CODES: &[&str] = &["PR", "CO", "OA", "PI", "AB", "AS"];

pub fn parse(content: &str) -> Result<Parsed835Response, String> {
    if content.trim().is_empty() {
        return Err("Empty file".to_string());
    }

    let delims = detect_delimiters(content);
    let segments = tokenize_segments(content, &delims)?;
    if segments.is_empty() {
        return Err("No X12 segments found".to_string());
    }

    validate_envelopes(&segments)?;
    let names: HashSet<&str> = segments.iter().map(|s| s.name.as_str()).collect();
    if !names.contains("ISA") {
        return Err("Missing ISA interchange header - not an X12 file".to_string());
    }

    let st = segments
        .iter()
        .find(|s| s.name == "ST")
        .ok_or("Missing ST transaction set header")?;

    let transaction_type = st.el(1);
    if transaction_type != "835" && transaction_type != "837" {
        return Err(format!(
            "Unsupported transaction set ST01='{}'; expected 835 or 837",
            transaction_type
        ));
    }

    let metadata = parse_metadata(&segments, st, &delims);

    if transaction_type == "837" {
        let ref_value = metadata
            .implementation_convention_reference
            .as_deref()
            .or(metadata.version_identifier.as_deref());
        let (claims, warnings) = parse_837(&segments, ref_value);
        let payment_info = ParsedPaymentInfo::default();
        let denials: Vec<ParsedDenial> = Vec::new();

        Ok(Parsed835Response {
            metadata,
            payment_info,
            total_claims: claims.len(),
            total_denials: 0,
            total_charges: round2(claims.iter().map(|c| c.total_charged).sum()),
            total_payments: round2(claims.iter().map(|c| c.total_paid).sum()),
            total_adjustments: round2(claims.iter().map(|c| c.total_adjustment).sum()),
            warnings,
            denials,
            claims,
        })
    } else {
        let payment_info = parse_payment(&segments);
        let (claims, warnings) = parse_claims(&segments, &payment_info);
        let denials = derive_denials(&claims, &payment_info);

        Ok(Parsed835Response {
            metadata,
            payment_info,
            total_claims: claims.len(),
            total_denials: denials.len(),
            total_charges: round2(claims.iter().map(|c| c.total_charged).sum()),
            total_payments: round2(claims.iter().map(|c| c.total_paid).sum()),
            total_adjustments: round2(claims.iter().map(|c| c.total_adjustment).sum()),
            warnings,
            denials,
            claims,
        })
    }
}

fn parse_metadata(
    segments: &[Segment],
    st: &Segment,
    delims: &Delimiters,
) -> ParsedTransactionMetadata {
    let isa = segments.iter().find(|s| s.name == "ISA");
    let gs = segments.iter().find(|s| s.name == "GS");

    ParsedTransactionMetadata {
        transaction_set_identifier: opt(st.el(1)),
        transaction_set_control_number: opt(st.el(2)),
        implementation_convention_reference: opt(st.el(3)),
        interchange_sender_id: isa.and_then(|s| opt(s.el(6))),
        interchange_receiver_id: isa.and_then(|s| opt(s.el(8))),
        functional_group_code: gs.and_then(|s| opt(s.el(1))),
        version_identifier: gs.and_then(|s| opt(s.el(8))),
        element_separator: Some(delims.element.to_string()),
        component_separator: Some(delims.component.to_string()),
        segment_terminator: if delims.segment == '\n' {
            Some("\\n".to_string())
        } else {
            Some(delims.segment.to_string())
        },
    }
}

/// BPR/TRN/DTM*405 plus the header N1 loops (payer and payee).
fn parse_payment(segments: &[Segment]) -> ParsedPaymentInfo {
    let mut info = ParsedPaymentInfo::default();

    if let Some(bpr) = segments.iter().find(|s| s.name == "BPR") {
        info.transaction_handling_code = opt(bpr.el(1));
        info.payment_amount = to_float(bpr.el(2));
        info.credit_debit_flag = opt(bpr.el(3));
        info.payment_method = opt(bpr.el(4));
        info.payment_format = opt(bpr.el(5));
        info.payment_date = to_date(bpr.el(16));
    }

    if let Some(trn) = segments.iter().find(|s| s.name == "TRN") {
        info.trace_number = opt(trn.el(2));
        info.payer_identifier = opt(trn.el(3));
    }

    for seg in segments {
        if seg.name == "CLP" {
            break;
        }
        if seg.name == "DTM" && seg.el(1) == "405" {
            info.production_date = to_date(seg.el(2));
        } else if seg.name == "N1" {
            let entity = seg.el(1);
            if entity == "PR" {
                info.payer_name = opt(seg.el(2));
                info.payer_id_number = opt(seg.el(4));
            } else if entity == "PE" {
                info.payee_name = opt(seg.el(2));
                if seg.el(3) == "XX" {
                    info.payee_npi = opt(seg.el(4));
                }
            }
        }
    }

    // PLB is provider-level balancing, not a claim denial. Parse it here so
    // it remains visible to reconciliation without contaminating the queue.
    for seg in segments.iter().filter(|seg| seg.name == "PLB") {
        let provider_identifier = seg.el(1).to_string();
        let fiscal_period_date = to_date(seg.el(2));
        for position in (3..seg.elements.len()).step_by(2) {
            let composite = seg.el(position);
            let amount = to_float(seg.el(position + 1));
            if composite.is_empty() || amount == 0.0 {
                continue;
            }
            let mut parts = composite.split(seg.component_separator());
            let adjustment_reason_code = parts.next().unwrap_or_default().to_string();
            let reference_number = parts.next().filter(|v| !v.is_empty()).map(str::to_string);
            info.provider_adjustments.push(ParsedProviderAdjustment {
                provider_identifier: provider_identifier.clone(),
                fiscal_period_date: fiscal_period_date.clone(),
                adjustment_reason_code,
                reference_number,
                amount,
            });
        }
    }

    info
}

fn parse_claims(
    segments: &[Segment],
    payment_info: &ParsedPaymentInfo,
) -> (Vec<ParsedClaim>, Vec<String>) {
    let mut claims: Vec<ParsedClaim> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut claim: Option<ParsedClaim> = None;
    let mut line: Option<ParsedServiceLine> = None;
    let mut line_adjustments: Vec<ParsedClaimAdjustment> = Vec::new();
    let mut line_counter: i64 = 0;

    for seg in segments {
        let name = seg.name.as_str();

        if name == "CLP" {
            close_line_into(&mut claim, &mut line, &mut line_adjustments);
            if let Some(mut c) = claim.take() {
                finalise_claim(&mut c, &mut warnings);
                claims.push(c);
            }
            line_counter = 0;
            claim = Some(parse_claim_header(seg, payment_info));
            continue;
        }

        if claim.is_none() {
            continue;
        }

        match name {
            "SVC" => {
                close_line_into(&mut claim, &mut line, &mut line_adjustments);
                line_counter += 1;
                line = Some(parse_service_line(seg, line_counter));
            }
            "CAS" => {
                let level = if line.is_some() { "line" } else { "claim" };
                let adjustments = parse_cas(seg, level);
                if line.is_some() {
                    line_adjustments.extend(adjustments);
                } else if let Some(c) = claim.as_mut() {
                    c.claim_level_adjustments.extend(adjustments);
                }
            }
            "NM1" => {
                if let Some(c) = claim.as_mut() {
                    let qualifier = seg.el(1);
                    if qualifier == "QC" {
                        c.patient_name = person_name(seg);
                        c.patient_id = opt(seg.el(9)).or(c.patient_id.take());
                    } else if qualifier == "82" || qualifier == "85" {
                        c.provider_name = person_name(seg);
                        if seg.el(8) == "XX" {
                            c.provider_npi = opt(seg.el(9));
                        }
                    }
                }
            }
            "DMG" => {
                if let Some(c) = claim.as_mut() {
                    c.date_of_birth = to_date(seg.el(2)).or(c.date_of_birth.take());
                }
            }
            "DTM" => {
                let value = to_date(seg.el(2));
                if seg.el(1) == "472" && line.is_some() {
                    if let Some(l) = line.as_mut() {
                        l.service_date = value;
                    }
                } else if let Some(c) = claim.as_mut() {
                    if seg.el(1) == "232" {
                        c.service_from = value;
                    } else if seg.el(1) == "233" {
                        c.service_to = value;
                    } else if seg.el(1) == "472" {
                        c.service_from = c.service_from.take().or(value.clone());
                        c.service_to = c.service_to.take().or(value);
                    }
                }
            }
            "AMT" => {
                if seg.el(1) == "B6" {
                    if let Some(l) = line.as_mut() {
                        l.allowed_amount = Some(to_float(seg.el(2)));
                    }
                }
            }
            "REF" => {
                if seg.el(1) == "6R" {
                    if let Some(l) = line.as_mut() {
                        l.control_number = opt(seg.el(2));
                    }
                }
            }
            "LQ" => {
                let code = seg.el(2);
                if !code.is_empty() {
                    if line.is_some() {
                        if let Some(l) = line.as_mut() {
                            l.remark_codes.push(code.to_string());
                        }
                    } else if let Some(c) = claim.as_mut() {
                        c.remark_codes.push(code.to_string());
                    }
                }
            }
            "MOA" => {
                if let Some(c) = claim.as_mut() {
                    for pos in 3..8 {
                        let code = seg.el(pos);
                        if !code.is_empty() {
                            c.remark_codes.push(code.to_string());
                        }
                    }
                }
            }
            "MIA" => {
                if let Some(c) = claim.as_mut() {
                    for pos in [5, 20, 21, 22, 23] {
                        let code = seg.el(pos);
                        if !code.is_empty() {
                            c.remark_codes.push(code.to_string());
                        }
                    }
                }
            }
            "SE" | "GE" | "IEA" => {
                close_line_into(&mut claim, &mut line, &mut line_adjustments);
                if let Some(mut c) = claim.take() {
                    finalise_claim(&mut c, &mut warnings);
                    claims.push(c);
                }
                line_counter = 0;
            }
            _ => {}
        }
    }

    close_line_into(&mut claim, &mut line, &mut line_adjustments);
    if let Some(mut c) = claim.take() {
        finalise_claim(&mut c, &mut warnings);
        claims.push(c);
    }

    if claims.is_empty() {
        warnings.push("No CLP claim loops found in this 835".to_string());
    }

    (claims, warnings)
}

/// Push the current service line into the current claim, if both exist.
fn close_line_into(
    claim: &mut Option<ParsedClaim>,
    line: &mut Option<ParsedServiceLine>,
    line_adjustments: &mut Vec<ParsedClaimAdjustment>,
) {
    if let Some(l) = line.take() {
        if let Some(c) = claim.as_mut() {
            c.service_lines.push(ParsedServiceLineDetail {
                service_line: l,
                adjustments: mem::take(line_adjustments),
            });
        }
    }
}

/// CLP01..CLP09 — note el(N) is CLP-N, which is what the old code got wrong.
fn parse_claim_header(seg: &Segment, payment_info: &ParsedPaymentInfo) -> ParsedClaim {
    ParsedClaim {
        claim_id: if seg.el(1).is_empty() {
            "UNKNOWN".to_string()
        } else {
            seg.el(1).to_string()
        },
        claim_status_code: seg.el(2).to_string(),
        total_charged: to_float(seg.el(3)),
        total_paid: to_float(seg.el(4)),
        patient_responsibility: to_float(seg.el(5)),
        total_adjustment: 0.0,
        claim_filing_indicator: opt(seg.el(6)),
        payer_claim_control_number: opt(seg.el(7)),
        facility_type_code: opt(seg.el(8)),
        frequency_code: opt(seg.el(9)),
        claim_type: "professional".to_string(),
        patient_id: None,
        patient_name: None,
        date_of_birth: None,
        provider_npi: payment_info.payee_npi.clone(),
        provider_name: payment_info.payee_name.clone(),
        payer_name: payment_info.payer_name.clone(),
        payer_id_number: payment_info.payer_id_number.clone(),
        service_from: None,
        service_to: None,
        diagnosis_codes: Vec::new(),
        service_lines: Vec::new(),
        claim_level_adjustments: Vec::new(),
        remark_codes: Vec::new(),
    }
}

/// SVC01 is a composite: qualifier:code:mod1:mod2:mod3:mod4.
fn parse_service_line(seg: &Segment, line_number: i64) -> ParsedServiceLine {
    let qualifier = opt(seg.comp(1, 1));
    let code = opt(seg.comp(1, 2));
    let is_cpt = code
        .as_deref()
        .map(|c| c.len() == 5 && c.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);

    ParsedServiceLine {
        line_number,
        procedure_qualifier: qualifier,
        cpt_code: if is_cpt { code.clone() } else { None },
        hcpcs_code: if is_cpt { None } else { code },
        modifier_1: opt(seg.comp(1, 3)),
        modifier_2: opt(seg.comp(1, 4)),
        modifier_3: opt(seg.comp(1, 5)),
        modifier_4: opt(seg.comp(1, 6)),
        charge_amount: to_float(seg.el(2)),
        paid_amount: to_float(seg.el(3)),
        revenue_code: opt(seg.el(4)),
        units: one_or(to_float(seg.el(5))),
        allowed_amount: None,
        service_date: None,
        control_number: None,
        remark_codes: Vec::new(),
    }
}

/// A CAS carries up to six (reason, amount, quantity) triplets:
/// CAS01 group, then 02-04, 05-07, 08-10, 11-13, 14-16, 17-19.
fn parse_cas(seg: &Segment, level: &str) -> Vec<ParsedClaimAdjustment> {
    let group = seg.el(1).to_string();
    let mut adjustments = Vec::new();

    for start in [2, 5, 8, 11, 14, 17] {
        let reason = seg.el(start);
        let amount_raw = seg.el(start + 1);
        let quantity_raw = seg.el(start + 2);

        if reason.is_empty() && amount_raw.is_empty() {
            continue;
        }

        adjustments.push(ParsedClaimAdjustment {
            adjustment_group_code: group.clone(),
            reason_code: opt(reason),
            amount: to_float(amount_raw),
            quantity: if quantity_raw.is_empty() {
                None
            } else {
                Some(to_float(quantity_raw))
            },
            remark_code: None,
            description: None,
            level: level.to_string(),
        });
    }

    adjustments
}

/// Derive totals and dates that are not carried on the CLP itself.
fn finalise_claim(claim: &mut ParsedClaim, warnings: &mut Vec<String>) {
    let total_adj: f64 = claim
        .claim_level_adjustments
        .iter()
        .map(|a| a.amount)
        .sum::<f64>()
        + claim
            .service_lines
            .iter()
            .flat_map(|d| d.adjustments.iter())
            .map(|a| a.amount)
            .sum::<f64>();
    claim.total_adjustment = round2(total_adj);

    // Attach claim-level remark codes to claim-level adjustments that have none.
    if !claim.remark_codes.is_empty() {
        let first_remark = claim.remark_codes[0].clone();
        for adjustment in &mut claim.claim_level_adjustments {
            if adjustment.remark_code.is_none() {
                adjustment.remark_code = Some(first_remark.clone());
            }
        }
    }

    for detail in &mut claim.service_lines {
        let remark = detail.service_line.remark_codes.first().cloned();
        for adjustment in &mut detail.adjustments {
            if adjustment.remark_code.is_none() {
                adjustment.remark_code = remark.clone();
            }
        }
    }

    // Fall back to the span of the service lines when DTM*232/233 are absent.
    let line_dates: Vec<&str> = claim
        .service_lines
        .iter()
        .filter_map(|d| d.service_line.service_date.as_deref())
        .collect();
    if !line_dates.is_empty() {
        if claim.service_from.is_none() {
            claim.service_from = line_dates.iter().min().map(|s| (*s).to_string());
        }
        if claim.service_to.is_none() {
            claim.service_to = line_dates.iter().max().map(|s| (*s).to_string());
        }
    }

    if claim
        .patient_id
        .as_deref()
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        claim.patient_id = Some(claim.claim_id.clone());
    }

    check_balance(claim, warnings);
}

/// X12 835 requires each claim to balance:
/// CLP03 (charged) == CLP04 (paid) + every CAS amount on the claim
/// and each service line:
/// SVC02 (charge) == SVC03 (paid) + that line's CAS amounts
fn check_balance(claim: &ParsedClaim, warnings: &mut Vec<String>) {
    let expected = round2(claim.total_paid + claim.total_adjustment);
    if (claim.total_charged - expected).abs() > 0.01 {
        warnings.push(format!(
            "Claim {} does not balance: charged {:.2} != paid {:.2} + adjustments {:.2}",
            claim.claim_id, claim.total_charged, claim.total_paid, claim.total_adjustment
        ));
    }

    for detail in &claim.service_lines {
        let sl = &detail.service_line;
        let line_adjustment: f64 = detail.adjustments.iter().map(|a| a.amount).sum::<f64>();
        let line_adjustment = round2(line_adjustment);
        let line_expected = round2(sl.paid_amount + line_adjustment);
        if (sl.charge_amount - line_expected).abs() > 0.01 {
            warnings.push(format!(
                "Claim {} line {} does not balance: charge {:.2} != paid {:.2} + adjustments {:.2}",
                claim.claim_id, sl.line_number, sl.charge_amount, sl.paid_amount, line_adjustment
            ));
        }
    }
}

/// CLP02 value for a payer's reversal of a previously paid claim.
pub const REVERSAL_STATUS: &str = "22";

/// One denial row per adjustment that carries a CARC.
///
/// Deliberately NOT limited to claims whose CLP02 is 4. A partially
/// denied claim — paid in part, with a CO/PI adjustment on one line —
/// is the common case this application exists to work on.
fn derive_denials(claims: &[ParsedClaim], payment_info: &ParsedPaymentInfo) -> Vec<ParsedDenial> {
    let denial_date = payment_info
        .production_date
        .clone()
        .or(payment_info.payment_date.clone())
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());

    let mut denials: Vec<ParsedDenial> = Vec::new();

    for claim in claims {
        // A reversal (CLP02 22) takes back an earlier payment and repeats
        // its adjustments with the signs flipped. Those are not new denials;
        // the original ones already exist from the first remittance.
        if claim.claim_status_code == REVERSAL_STATUS {
            continue;
        }
        for adjustment in &claim.claim_level_adjustments {
            if !is_denial(adjustment) {
                continue;
            }
            denials.push(ParsedDenial {
                claim_id: claim.claim_id.clone(),
                service_line_number: None,
                cpt_code: None,
                hcpcs_code: None,
                modifier_1: None,
                modifier_2: None,
                charge_amount: claim.total_charged,
                payment_amount: claim.total_paid,
                adjustment_amount: adjustment.amount,
                cagc: adjustment.adjustment_group_code.clone(),
                carc_code: adjustment.reason_code.clone(),
                rarc_code: adjustment.remark_code.clone(),
                denial_reason: adjustment.description.clone(),
                denial_date: Some(denial_date.clone()),
            });
        }

        for detail in &claim.service_lines {
            let sl = &detail.service_line;
            for adjustment in &detail.adjustments {
                if !is_denial(adjustment) {
                    continue;
                }
                denials.push(ParsedDenial {
                    claim_id: claim.claim_id.clone(),
                    service_line_number: Some(sl.line_number),
                    cpt_code: sl.cpt_code.clone(),
                    hcpcs_code: sl.hcpcs_code.clone(),
                    modifier_1: sl.modifier_1.clone(),
                    modifier_2: sl.modifier_2.clone(),
                    charge_amount: sl.charge_amount,
                    payment_amount: sl.paid_amount,
                    adjustment_amount: adjustment.amount,
                    cagc: adjustment.adjustment_group_code.clone(),
                    carc_code: adjustment.reason_code.clone(),
                    rarc_code: adjustment.remark_code.clone(),
                    denial_reason: adjustment.description.clone(),
                    denial_date: Some(denial_date.clone()),
                });
            }
        }
    }

    denials
}

fn is_denial(adjustment: &ParsedClaimAdjustment) -> bool {
    VALID_GROUP_CODES.contains(&adjustment.adjustment_group_code.as_str())
        && adjustment
            .reason_code
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        && adjustment.amount != 0.0
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn synthetic_835_produces_claims_and_denials() {
        let result = parse(denial_test_support::SYNTHETIC_835)
            .expect("the checked-in synthetic 835 must remain parseable");
        assert!(!result.claims.is_empty());
        assert!(!result.denials.is_empty());
        assert_eq!(
            result.metadata.transaction_set_identifier.as_deref(),
            Some("835")
        );
    }

    #[test]
    fn a_reversal_loop_creates_no_denials() {
        let edi = "ISA*00*          *00*          *ZZ*PAYER          *ZZ*PROVIDER       *240201*0800*^*00501*000000009*0*P*:~\
GS*HP*PAYER*PROVIDER*20240201*0800*9*X*005010X221A1~ST*835*0009~\
BPR*I*800.00*C*ACH*CCP*01*011000015*DA*1*1**01*021000021*DA*2*20240205~\
TRN*1*EFT9*1~N1*PR*SYNTHETIC PAYER~N1*PE*SYNTHETIC CLINIC*XX*1999999999~LX*1~\
CLP*PAT009*22*-800.00*0.00*0.00*MC*PCN9*11*1~SVC*HC:71046*-800.00*0.00**1~CAS*CO*197*-800.00~\
CLP*PAT009*1*800.00*800.00*0.00*MC*PCN9*11*1~SVC*HC:71046*800.00*800.00**1~\
SE*12*0009~GE*1*9~IEA*1*000000009~";
        let result = parse(edi).expect("synthetic reversal 835 parses");
        assert_eq!(result.claims.len(), 2);
        assert_eq!(result.claims[0].claim_status_code, super::REVERSAL_STATUS);
        assert!(
            result.denials.is_empty(),
            "reversal CAS lines must not become denials: {:?}",
            result.denials
        );
    }
}
