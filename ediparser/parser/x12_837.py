"""
X12 837 parser - Health Care Claim (professional 837P and institutional 837I)

Added 2026-09-05. The service previously advertised 837 support in its route
docstrings but had none; 837 files parsed to zero claims and were reported as
successful. This module is the actual implementation.

An 837 is a claim SUBMISSION, so unlike an 835 it carries no payments and no
adjustments. There is nothing to derive denials from - `denials` is always
empty and `total_paid` is always zero. That is correct, not a parse failure.

Structure, in the order the state machine walks it:

    BHT                       transaction purpose
    NM1*41 / NM1*40           submitter / receiver
    HL*..*20  (2000A)  NM1*85 billing provider
    HL*..*22  (2000B)  SBR, NM1*IL subscriber, NM1*PR payer, DMG
    HL*..*23  (2000C)  PAT, NM1*QC patient        <- only when patient != subscriber
      CLM     (2300)   the claim itself
      HI               diagnosis codes
      DTP*434/*472     statement / service dates
      LX      (2400)   service line
      SV1 / SV2        professional / institutional service detail

The hierarchy is expressed by HL parent pointers, but claims always follow
their own subscriber/patient loop, so tracking the current context linearly
is sufficient and far less fragile than rebuilding the HL tree.
"""

from typing import Optional

from parser.schema import (
    ParsedClaim,
    ParsedServiceLine,
    ParsedServiceLineDetail,
)
from parser.x12_common import Segment, person_name, to_date, to_float

# ICD qualifiers that carry a diagnosis code in an HI composite.
# A* qualifiers are ICD-10; the bare forms are legacy ICD-9.
DIAGNOSIS_QUALIFIERS = {
    "ABK", "BK",    # principal diagnosis
    "ABF", "BF",    # other diagnosis
    "ABJ", "BJ",    # admitting diagnosis
    "APR", "PR",    # patient reason for visit
    "ABN", "BN",    # external cause of injury
}


class _Context:
    """Loop state carried down to whichever claim comes next."""

    def __init__(self):
        self.billing_provider_name: Optional[str] = None
        self.billing_provider_npi: Optional[str] = None
        self.subscriber_name: Optional[str] = None
        self.subscriber_id: Optional[str] = None
        self.subscriber_dob: Optional[str] = None
        self.patient_name: Optional[str] = None
        self.patient_id: Optional[str] = None
        self.patient_dob: Optional[str] = None
        self.payer_name: Optional[str] = None
        self.payer_id: Optional[str] = None
        self.filing_indicator: Optional[str] = None
        self.in_dependent_loop = False

    def effective_patient(self) -> tuple[Optional[str], Optional[str], Optional[str]]:
        """
        The patient is the subscriber unless a dependent (2000C) loop said
        otherwise. Getting this backwards silently attributes a child's claim
        to the policy holder.
        """
        if self.in_dependent_loop and (self.patient_name or self.patient_id):
            # A dependent rarely carries a member ID of their own; the
            # subscriber's is the identifier the payer knows them by.
            return (
                self.patient_name,
                self.patient_id or self.subscriber_id,
                self.patient_dob or self.subscriber_dob,
            )
        return self.subscriber_name, self.subscriber_id, self.subscriber_dob


def detect_claim_type(segments: list[Segment], metadata_reference: Optional[str]) -> str:
    """
    837P (005010X222) is professional, 837I (005010X223) is institutional.

    The implementation reference in ST03/GS08 is authoritative when present;
    otherwise SV1 vs SV2 gives it away.
    """
    reference = (metadata_reference or "").upper()
    if "X223" in reference:
        return "institutional"
    if "X222" in reference:
        return "professional"
    names = {s.name for s in segments}
    if "SV2" in names:
        return "institutional"
    return "professional"


def parse_837(
    segments: list[Segment],
    implementation_reference: Optional[str],
) -> tuple[list[ParsedClaim], list[str]]:
    """Walk an 837 transaction and return its claims plus any warnings."""
    claim_type = detect_claim_type(segments, implementation_reference)

    claims: list[ParsedClaim] = []
    warnings: list[str] = []
    ctx = _Context()

    claim: Optional[ParsedClaim] = None
    line: Optional[ParsedServiceLine] = None
    line_counter = 0

    def close_line():
        nonlocal line
        if line is not None and claim is not None:
            claim.service_lines.append(ParsedServiceLineDetail(service_line=line))
        line = None

    def close_claim():
        nonlocal claim, line_counter
        close_line()
        if claim is not None:
            _finalise(claim, warnings)
            claims.append(claim)
        claim = None
        line_counter = 0

    for seg in segments:
        name = seg.name

        if name == "HL":
            level = seg.el(3)
            if level == "20":          # billing provider loop
                close_claim()
                ctx.in_dependent_loop = False
            elif level == "22":        # subscriber loop
                close_claim()
                ctx.in_dependent_loop = False
                ctx.patient_name = ctx.patient_id = ctx.patient_dob = None
            elif level == "23":        # dependent loop
                close_claim()
                ctx.in_dependent_loop = True
                ctx.patient_name = ctx.patient_id = ctx.patient_dob = None
            continue

        if name == "SBR":
            ctx.filing_indicator = seg.el(9) or ctx.filing_indicator
            continue

        if name == "NM1":
            qualifier = seg.el(1)
            identifier = seg.el(9) or None
            if qualifier == "85":                     # billing provider
                ctx.billing_provider_name = person_name(seg)
                if seg.el(8) == "XX":
                    ctx.billing_provider_npi = identifier
            elif qualifier == "IL":                   # insured / subscriber
                ctx.subscriber_name = person_name(seg)
                ctx.subscriber_id = identifier
            elif qualifier == "QC":                   # patient (dependent)
                ctx.patient_name = person_name(seg)
                ctx.patient_id = identifier
            elif qualifier == "PR":                   # payer
                ctx.payer_name = person_name(seg)
                ctx.payer_id = identifier
            elif qualifier == "82" and claim is not None:   # rendering provider
                claim.provider_name = person_name(seg) or claim.provider_name
                if seg.el(8) == "XX" and identifier:
                    claim.provider_npi = identifier
            continue

        if name == "DMG":
            dob = to_date(seg.el(2))
            if claim is not None:
                claim.date_of_birth = claim.date_of_birth or dob
            elif ctx.in_dependent_loop:
                ctx.patient_dob = dob
            else:
                ctx.subscriber_dob = dob
            continue

        if name == "CLM":
            close_claim()
            patient_name, patient_id, patient_dob = ctx.effective_patient()
            claim = ParsedClaim(
                claim_id=seg.el(1) or "UNKNOWN",
                # An 837 is a submission: there is no adjudication status to
                # report, so this stays empty rather than inventing one.
                claim_status_code="",
                total_charged=to_float(seg.el(2)),
                total_paid=0.0,
                patient_responsibility=0.0,
                claim_filing_indicator=ctx.filing_indicator,
                # CLM05 is a composite: facility code : qualifier : frequency
                facility_type_code=seg.comp(5, 1) or None,
                frequency_code=seg.comp(5, 3) or None,
                claim_type=claim_type,
                patient_id=patient_id,
                patient_name=patient_name,
                date_of_birth=patient_dob,
                provider_npi=ctx.billing_provider_npi,
                provider_name=ctx.billing_provider_name,
                payer_name=ctx.payer_name,
                payer_id_number=ctx.payer_id,
            )
            line_counter = 0
            continue

        if claim is None:
            continue

        if name == "HI":
            claim.diagnosis_codes.extend(_diagnosis_codes(seg))

        elif name == "LX":
            close_line()
            line_counter += 1
            line = ParsedServiceLine(line_number=line_counter)

        elif name == "SV1":
            if line is None:
                line_counter += 1
                line = ParsedServiceLine(line_number=line_counter)
            _apply_sv1(line, seg)

        elif name == "SV2":
            if line is None:
                line_counter += 1
                line = ParsedServiceLine(line_number=line_counter)
            _apply_sv2(line, seg)

        elif name == "DTP":
            qualifier = seg.el(1)
            fmt = seg.el(2)
            value = seg.el(3)
            if qualifier == "472":
                start, end = _date_or_range(fmt, value)
                if line is not None:
                    line.service_date = start
                claim.service_from = claim.service_from or start
                claim.service_to = claim.service_to or (end or start)
            elif qualifier == "434":       # institutional statement period
                start, end = _date_or_range(fmt, value)
                claim.service_from = start or claim.service_from
                claim.service_to = end or start or claim.service_to

        elif name == "REF":
            if seg.el(1) == "6R" and line is not None:
                line.control_number = seg.el(2) or None

        elif name in ("SE", "GE", "IEA"):
            close_claim()

    close_claim()

    if not claims:
        warnings.append("No CLM claim loops found in this 837")

    return claims, warnings


def _diagnosis_codes(seg: Segment) -> list[str]:
    """HI carries up to twelve composites, each `QUALIFIER:CODE`."""
    codes = []
    for position in range(1, 13):
        qualifier = seg.comp(position, 1)
        code = seg.comp(position, 2)
        if code and qualifier.upper() in DIAGNOSIS_QUALIFIERS:
            codes.append(code)
    return codes


def _apply_sv1(line: ParsedServiceLine, seg: Segment):
    """SV1 - professional service. SV101 is a composite procedure."""
    qualifier = seg.comp(1, 1) or None
    code = seg.comp(1, 2) or None
    is_cpt = bool(code) and code.isdigit() and len(code) == 5

    line.procedure_qualifier = qualifier
    line.cpt_code = code if is_cpt else None
    line.hcpcs_code = None if is_cpt else code
    line.modifier_1 = seg.comp(1, 3) or None
    line.modifier_2 = seg.comp(1, 4) or None
    line.modifier_3 = seg.comp(1, 5) or None
    line.modifier_4 = seg.comp(1, 6) or None
    line.charge_amount = to_float(seg.el(2))
    line.units = to_float(seg.el(4)) or 1.0


def _apply_sv2(line: ParsedServiceLine, seg: Segment):
    """SV2 - institutional service. Revenue code is SV201; procedure SV202."""
    line.revenue_code = seg.el(1) or None
    qualifier = seg.comp(2, 1) or None
    code = seg.comp(2, 2) or None
    is_cpt = bool(code) and code.isdigit() and len(code) == 5

    line.procedure_qualifier = qualifier
    line.cpt_code = code if is_cpt else None
    line.hcpcs_code = None if is_cpt else code
    line.modifier_1 = seg.comp(2, 3) or None
    line.modifier_2 = seg.comp(2, 4) or None
    line.charge_amount = to_float(seg.el(3))
    line.units = to_float(seg.el(5)) or 1.0


def _date_or_range(fmt: str, value: str) -> tuple[Optional[str], Optional[str]]:
    """D8 is a single date; RD8 is `CCYYMMDD-CCYYMMDD`."""
    if not value:
        return None, None
    if fmt.upper() == "RD8" and "-" in value:
        start_raw, _, end_raw = value.partition("-")
        return to_date(start_raw), to_date(end_raw)
    return to_date(value), None


def _finalise(claim: ParsedClaim, warnings: list[str]):
    """Derive what the CLM does not carry, and check the one balance rule."""
    line_dates = [d.service_line.service_date for d in claim.service_lines if d.service_line.service_date]
    if line_dates:
        claim.service_from = claim.service_from or min(line_dates)
        claim.service_to = claim.service_to or max(line_dates)

    if not claim.patient_id:
        claim.patient_id = claim.claim_id

    # De-duplicate diagnosis codes while preserving order (principal first).
    seen = set()
    ordered = []
    for code in claim.diagnosis_codes:
        if code not in seen:
            seen.add(code)
            ordered.append(code)
    claim.diagnosis_codes = ordered

    # CLM02 must equal the sum of the service line charges.
    line_total = round(sum(d.service_line.charge_amount for d in claim.service_lines), 2)
    if claim.service_lines and abs(claim.total_charged - line_total) > 0.01:
        warnings.append(
            f"Claim {claim.claim_id} does not balance: CLM02 {claim.total_charged:.2f} "
            f"!= sum of service lines {line_total:.2f}"
        )
