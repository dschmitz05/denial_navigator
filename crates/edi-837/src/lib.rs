//! X12 837 parser — Health Care Claim (professional 837P and institutional 837I).

use std::collections::HashSet;

use denial_edi_core::common::{one_or, opt, person_name, round2, to_date, to_float, Segment};
use denial_edi_core::schema::{ParsedClaim, ParsedServiceLine, ParsedServiceLineDetail};

const DIAGNOSIS_QUALIFIERS: &[&str] = &[
    "ABK", "BK", "ABF", "BF", "ABJ", "BJ", "APR", "PR", "ABN", "BN",
];

#[derive(Debug, Default)]
struct Context {
    billing_provider_name: Option<String>,
    billing_provider_npi: Option<String>,
    subscriber_name: Option<String>,
    subscriber_id: Option<String>,
    subscriber_dob: Option<String>,
    patient_name: Option<String>,
    patient_id: Option<String>,
    patient_dob: Option<String>,
    payer_name: Option<String>,
    payer_id: Option<String>,
    filing_indicator: Option<String>,
    in_dependent_loop: bool,
}

impl Context {
    fn effective_patient(&self) -> (Option<String>, Option<String>, Option<String>) {
        if self.in_dependent_loop && (self.patient_name.is_some() || self.patient_id.is_some()) {
            let id = self
                .patient_id
                .clone()
                .or_else(|| self.subscriber_id.clone());
            let dob = self
                .patient_dob
                .clone()
                .or_else(|| self.subscriber_dob.clone());
            (self.patient_name.clone(), id, dob)
        } else {
            (
                self.subscriber_name.clone(),
                self.subscriber_id.clone(),
                self.subscriber_dob.clone(),
            )
        }
    }
}

fn detect_claim_type(segments: &[Segment], metadata_reference: Option<&str>) -> String {
    let reference = (metadata_reference.unwrap_or("")).to_uppercase();
    if reference.contains("X223") {
        return "institutional".to_string();
    }
    if reference.contains("X222") {
        return "professional".to_string();
    }
    let names: HashSet<&str> = segments.iter().map(|s| s.name.as_str()).collect();
    if names.contains("SV2") {
        "institutional".to_string()
    } else {
        "professional".to_string()
    }
}

fn diagnosis_codes(seg: &Segment) -> Vec<String> {
    let mut codes = Vec::new();
    for position in 1..13 {
        let qualifier = seg.comp(position, 1).to_uppercase();
        let code = seg.comp(position, 2);
        if !code.is_empty() && DIAGNOSIS_QUALIFIERS.contains(&qualifier.as_str()) {
            codes.push(code.to_string());
        }
    }
    codes
}

fn apply_sv1(line: &mut ParsedServiceLine, seg: &Segment) {
    let qualifier = opt(seg.comp(1, 1));
    let code = opt(seg.comp(1, 2));
    let is_cpt = code
        .as_deref()
        .map(|c| c.len() == 5 && c.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);

    line.procedure_qualifier = qualifier;
    line.cpt_code = if is_cpt { code.clone() } else { None };
    line.hcpcs_code = if is_cpt { None } else { code };
    line.modifier_1 = opt(seg.comp(1, 3));
    line.modifier_2 = opt(seg.comp(1, 4));
    line.modifier_3 = opt(seg.comp(1, 5));
    line.modifier_4 = opt(seg.comp(1, 6));
    line.charge_amount = to_float(seg.el(2));
    line.units = one_or(to_float(seg.el(4)));
}

fn apply_sv2(line: &mut ParsedServiceLine, seg: &Segment) {
    line.revenue_code = opt(seg.el(1));
    let qualifier = opt(seg.comp(2, 1));
    let code = opt(seg.comp(2, 2));
    let is_cpt = code
        .as_deref()
        .map(|c| c.len() == 5 && c.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);

    line.procedure_qualifier = qualifier;
    line.cpt_code = if is_cpt { code.clone() } else { None };
    line.hcpcs_code = if is_cpt { None } else { code };
    line.modifier_1 = opt(seg.comp(2, 3));
    line.modifier_2 = opt(seg.comp(2, 4));
    line.charge_amount = to_float(seg.el(3));
    line.units = one_or(to_float(seg.el(5)));
}

fn date_or_range(fmt: &str, value: &str) -> (Option<String>, Option<String>) {
    if value.is_empty() {
        return (None, None);
    }
    if fmt.to_uppercase() == "RD8" && value.contains('-') {
        let (start_raw, end_raw) = match value.split_once('-') {
            Some((a, b)) => (a, b),
            None => return (to_date(value), None),
        };
        (to_date(start_raw), to_date(end_raw))
    } else {
        (to_date(value), None)
    }
}

fn finalise(claim: &mut ParsedClaim, warnings: &mut Vec<String>) {
    let line_dates: Vec<String> = claim
        .service_lines
        .iter()
        .filter_map(|d| d.service_line.service_date.clone())
        .collect();
    if !line_dates.is_empty() {
        if claim.service_from.is_none() {
            claim.service_from = line_dates.iter().min().cloned();
        }
        if claim.service_to.is_none() {
            claim.service_to = line_dates.iter().max().cloned();
        }
    }

    if claim.patient_id.is_none() {
        claim.patient_id = Some(claim.claim_id.clone());
    }

    let mut seen = HashSet::new();
    let ordered: Vec<String> = claim
        .diagnosis_codes
        .iter()
        .filter(|c| seen.insert((*c).clone()))
        .cloned()
        .collect();
    claim.diagnosis_codes = ordered;

    let line_total = round2(
        claim
            .service_lines
            .iter()
            .map(|d| d.service_line.charge_amount)
            .sum::<f64>(),
    );
    if !claim.service_lines.is_empty() && (claim.total_charged - line_total).abs() > 0.01 {
        warnings.push(format!(
            "Claim {} does not balance: CLM02 {:.2} != sum of service lines {:.2}",
            claim.claim_id, claim.total_charged, line_total
        ));
    }
}

pub fn parse_837(
    segments: &[Segment],
    implementation_reference: Option<&str>,
) -> (Vec<ParsedClaim>, Vec<String>) {
    let claim_type = detect_claim_type(segments, implementation_reference);

    let mut claims: Vec<ParsedClaim> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut ctx = Context::default();

    let mut claim: Option<ParsedClaim> = None;
    let mut line: Option<ParsedServiceLine> = None;
    let mut line_counter: i64 = 0;

    for seg in segments {
        let name = seg.name.as_str();

        if name == "HL" {
            let level = seg.el(3);
            if level == "20" || level == "22" || level == "23" {
                if let Some(c) = claim.take() {
                    let mut c = c;
                    if let Some(l) = line.take() {
                        c.service_lines.push(ParsedServiceLineDetail {
                            service_line: l,
                            adjustments: Vec::new(),
                        });
                    }
                    finalise(&mut c, &mut warnings);
                    claims.push(c);
                }
                line = None;
                line_counter = 0;
                if level == "20" || level == "22" {
                    ctx.in_dependent_loop = false;
                }
                if level == "22" || level == "23" {
                    ctx.patient_name = None;
                    ctx.patient_id = None;
                    ctx.patient_dob = None;
                }
                if level == "23" {
                    ctx.in_dependent_loop = true;
                }
            }
            continue;
        }

        if name == "SBR" {
            if let Some(v) = opt(seg.el(9)) {
                ctx.filing_indicator = Some(v);
            }
            continue;
        }

        if name == "NM1" {
            let qualifier = seg.el(1);
            let identifier = opt(seg.el(9));
            match qualifier {
                "85" => {
                    ctx.billing_provider_name = person_name(seg);
                    if seg.el(8) == "XX" {
                        ctx.billing_provider_npi = identifier;
                    }
                }
                "IL" => {
                    ctx.subscriber_name = person_name(seg);
                    ctx.subscriber_id = identifier;
                }
                "QC" => {
                    ctx.patient_name = person_name(seg);
                    ctx.patient_id = identifier;
                }
                "PR" => {
                    ctx.payer_name = person_name(seg);
                    ctx.payer_id = identifier;
                }
                "82" => {
                    if let Some(c) = claim.as_mut() {
                        c.provider_name = person_name(seg).or_else(|| c.provider_name.clone());
                        if seg.el(8) == "XX" && identifier.is_some() {
                            c.provider_npi = identifier;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if name == "DMG" {
            let dob = to_date(seg.el(2));
            if let Some(c) = claim.as_mut() {
                if c.date_of_birth.is_none() {
                    c.date_of_birth = dob;
                }
            } else if ctx.in_dependent_loop {
                ctx.patient_dob = dob;
            } else {
                ctx.subscriber_dob = dob;
            }
            continue;
        }

        if name == "CLM" {
            if let Some(c) = claim.take() {
                let mut c = c;
                if let Some(l) = line.take() {
                    c.service_lines.push(ParsedServiceLineDetail {
                        service_line: l,
                        adjustments: Vec::new(),
                    });
                }
                finalise(&mut c, &mut warnings);
                claims.push(c);
            }
            line = None;

            let (patient_name, patient_id, patient_dob) = ctx.effective_patient();
            claim = Some(ParsedClaim {
                claim_id: seg.el(1).to_string(),
                claim_status_code: String::new(),
                total_charged: to_float(seg.el(2)),
                total_paid: 0.0,
                patient_responsibility: 0.0,
                total_adjustment: 0.0,
                claim_filing_indicator: ctx.filing_indicator.clone(),
                payer_claim_control_number: None,
                facility_type_code: opt(seg.comp(5, 1)),
                frequency_code: opt(seg.comp(5, 3)),
                claim_type: claim_type.clone(),
                patient_id,
                patient_name,
                date_of_birth: patient_dob,
                provider_npi: ctx.billing_provider_npi.clone(),
                provider_name: ctx.billing_provider_name.clone(),
                payer_name: ctx.payer_name.clone(),
                payer_id_number: ctx.payer_id.clone(),
                service_from: None,
                service_to: None,
                diagnosis_codes: Vec::new(),
                service_lines: Vec::new(),
                claim_level_adjustments: Vec::new(),
                remark_codes: Vec::new(),
            });
            line_counter = 0;
            continue;
        }

        if claim.is_none() {
            continue;
        }

        match name {
            "HI" => {
                if let Some(c) = claim.as_mut() {
                    c.diagnosis_codes.extend(diagnosis_codes(seg));
                }
            }
            "LX" => {
                if let (Some(c), Some(l)) = (claim.as_mut(), line.take()) {
                    c.service_lines.push(ParsedServiceLineDetail {
                        service_line: l,
                        adjustments: Vec::new(),
                    });
                }
                line_counter += 1;
                line = Some(ParsedServiceLine {
                    line_number: line_counter,
                    ..Default::default()
                });
            }
            "SV1" => {
                if line.is_none() {
                    line_counter += 1;
                    line = Some(ParsedServiceLine {
                        line_number: line_counter,
                        ..Default::default()
                    });
                }
                if let (Some(l), Some(c)) = (line.as_mut(), claim.as_ref()) {
                    let _ = c;
                    apply_sv1(l, seg);
                }
            }
            "SV2" => {
                if line.is_none() {
                    line_counter += 1;
                    line = Some(ParsedServiceLine {
                        line_number: line_counter,
                        ..Default::default()
                    });
                }
                if let Some(l) = line.as_mut() {
                    apply_sv2(l, seg);
                }
            }
            "DTP" => {
                let qualifier = seg.el(1);
                let fmt = seg.el(2);
                let value = seg.el(3);
                if qualifier == "472" {
                    let (start, end) = date_or_range(fmt, value);
                    if let Some(l) = line.as_mut() {
                        l.service_date = start.clone();
                    }
                    if let Some(c) = claim.as_mut() {
                        if c.service_from.is_none() {
                            c.service_from = start.clone();
                        }
                        if c.service_to.is_none() {
                            c.service_to = end.or(start);
                        }
                    }
                } else if qualifier == "434" {
                    let (start, end) = date_or_range(fmt, value);
                    if let Some(c) = claim.as_mut() {
                        c.service_from = start.clone().or_else(|| c.service_from.take());
                        c.service_to = end.or(start).or_else(|| c.service_to.take());
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
            "SE" | "GE" | "IEA" => {
                if let Some(c) = claim.take() {
                    let mut c = c;
                    if let Some(l) = line.take() {
                        c.service_lines.push(ParsedServiceLineDetail {
                            service_line: l,
                            adjustments: Vec::new(),
                        });
                    }
                    finalise(&mut c, &mut warnings);
                    claims.push(c);
                }
                line = None;
                line_counter = 0;
            }
            _ => {}
        }
    }

    if let Some(c) = claim.take() {
        let mut c = c;
        if let Some(l) = line.take() {
            c.service_lines.push(ParsedServiceLineDetail {
                service_line: l,
                adjustments: Vec::new(),
            });
        }
        finalise(&mut c, &mut warnings);
        claims.push(c);
    }

    if claims.is_empty() {
        warnings.push("No CLM claim loops found in this 837".to_string());
    }

    (claims, warnings)
}
