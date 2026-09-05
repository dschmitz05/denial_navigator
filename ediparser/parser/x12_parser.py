"""
X12 parser - dispatches on ST01 to the 835 or 837 reader

835 (remittance advice) is handled here; 837 (claim submission) lives in
`x12_837.py`. Both share the primitives in `x12_common.py`.

Rewritten 2026-09-05. The previous implementation had five independent
defects; see the audit note for the full list. The important ones, so they
are not reintroduced:

  * Segments are terminated by a delimiter declared in the ISA, NOT by
    newlines. Real payer files are a single line. Splitting on newlines
    returns the whole file as one segment.
  * `segment.split(sep)[0]` is the segment NAME. Element NN is therefore at
    index NN, so CLP01 is index 1. Everything here uses a 1-based `el()`
    accessor to keep that from drifting again.
  * A CAS segment carries up to six (reason, amount, quantity) triplets,
    not one, and CAS03 is an AMOUNT - not a remark code.
  * Claim-level CAS appears before the first SVC. That is where most
    denials live, so it must be captured, not skipped.
  * SVC01 is a COMPOSITE (`HC:99213:25`) and must be split on the
    component separator declared in ISA16.

Delimiters are read from the interchange header rather than assumed.
"""

from datetime import datetime
from typing import Optional

from parser.x12_837 import parse_837
from parser.x12_common import (
    Delimiters,
    Segment,
    detect_delimiters,
    person_name,
    split_segments,
    to_date,
    to_float,
)
from parser.schema import (
    Parsed835Response,
    ParsedClaim,
    ParsedClaimAdjustment,
    ParsedDenial,
    ParsedPaymentInfo,
    ParsedServiceLine,
    ParsedServiceLineDetail,
    ParsedTransactionMetadata,
)

# The denials table constrains cagc to this set; emitting anything else
# would fail the insert, so adjustments outside it are not turned into
# denial rows (they remain visible on the claim).
VALID_GROUP_CODES = {"PR", "CO", "OA", "PI", "AB", "AS"}

# CLP02 values that mean the payer denied the claim outright.
DENIED_CLAIM_STATUS = {"4"}


class X12Parser:
    """Parse X12 835 remittance files into structured data.

    Stateless between calls - safe to share one instance across requests.
    """

    def parse(self, content: str) -> Parsed835Response:
        if not content or not content.strip():
            raise ValueError("Empty file")

        delims = detect_delimiters(content)
        segments = split_segments(content, delims)
        if not segments:
            raise ValueError("No X12 segments found")

        names = {s.name for s in segments}
        if "ISA" not in names:
            raise ValueError("Missing ISA interchange header - not an X12 file")

        st = next((s for s in segments if s.name == "ST"), None)
        if st is None:
            raise ValueError("Missing ST transaction set header")

        transaction_type = st.el(1)
        if transaction_type not in ("835", "837"):
            raise ValueError(
                f"Unsupported transaction set ST01={transaction_type!r}; expected 835 or 837"
            )

        metadata = self._parse_metadata(segments, st, delims)

        if transaction_type == "837":
            # A claim submission carries no payment and no adjudication, so
            # there is nothing to derive denials from. Empty denials here is
            # the correct answer, not a failure.
            claims, warnings = parse_837(
                segments,
                metadata.implementation_convention_reference or metadata.version_identifier,
            )
            payment_info = ParsedPaymentInfo()
            denials: list[ParsedDenial] = []
        else:
            payment_info = self._parse_payment(segments)
            claims, warnings = self._parse_claims(segments, payment_info)
            denials = self._derive_denials(claims, payment_info)

        return Parsed835Response(
            metadata=metadata,
            payment_info=payment_info,
            claims=claims,
            denials=denials,
            total_claims=len(claims),
            total_denials=len(denials),
            total_charges=round(sum(c.total_charged for c in claims), 2),
            total_payments=round(sum(c.total_paid for c in claims), 2),
            total_adjustments=round(sum(c.total_adjustment for c in claims), 2),
            warnings=warnings,
        )

    def _parse_metadata(
        self, segments: list[Segment], st: Segment, delims: Delimiters
    ) -> ParsedTransactionMetadata:
        isa = next((s for s in segments if s.name == "ISA"), None)
        gs = next((s for s in segments if s.name == "GS"), None)
        return ParsedTransactionMetadata(
            transaction_set_identifier=st.el(1) or None,
            transaction_set_control_number=st.el(2) or None,
            implementation_convention_reference=st.el(3) or None,
            interchange_sender_id=(isa.el(6) if isa else None) or None,
            interchange_receiver_id=(isa.el(8) if isa else None) or None,
            functional_group_code=(gs.el(1) if gs else None) or None,
            version_identifier=(gs.el(8) if gs else None) or None,
            element_separator=delims.element,
            component_separator=delims.component,
            segment_terminator="\\n" if delims.segment == "\n" else delims.segment,
        )

    def _parse_payment(self, segments: list[Segment]) -> ParsedPaymentInfo:
        """BPR/TRN/DTM*405 plus the header N1 loops (payer and payee)."""
        info = ParsedPaymentInfo()

        bpr = next((s for s in segments if s.name == "BPR"), None)
        if bpr is not None:
            info.transaction_handling_code = bpr.el(1) or None
            info.payment_amount = to_float(bpr.el(2))
            info.credit_debit_flag = bpr.el(3) or None
            info.payment_method = bpr.el(4) or None
            info.payment_format = bpr.el(5) or None
            info.payment_date = to_date(bpr.el(16))

        trn = next((s for s in segments if s.name == "TRN"), None)
        if trn is not None:
            info.trace_number = trn.el(2) or None
            info.payer_identifier = trn.el(3) or None

        # Header-level N1 loops stop at the first CLP.
        for seg in segments:
            if seg.name == "CLP":
                break
            if seg.name == "DTM" and seg.el(1) == "405":
                info.production_date = to_date(seg.el(2))
            elif seg.name == "N1":
                entity = seg.el(1)
                if entity == "PR":
                    info.payer_name = seg.el(2) or None
                    info.payer_id_number = seg.el(4) or None
                elif entity == "PE":
                    info.payee_name = seg.el(2) or None
                    if seg.el(3) == "XX":
                        info.payee_npi = seg.el(4) or None

        return info

    # -- claim loops --

    def _parse_claims(
        self, segments: list[Segment], payment_info: ParsedPaymentInfo
    ) -> tuple[list[ParsedClaim], list[str]]:
        claims: list[ParsedClaim] = []
        warnings: list[str] = []

        claim: Optional[ParsedClaim] = None
        line: Optional[ParsedServiceLine] = None
        line_adjustments: list[ParsedClaimAdjustment] = []
        line_counter = 0

        def close_line():
            nonlocal line, line_adjustments
            if line is not None and claim is not None:
                claim.service_lines.append(
                    ParsedServiceLineDetail(
                        service_line=line, adjustments=list(line_adjustments)
                    )
                )
            line = None
            line_adjustments = []

        def close_claim():
            nonlocal claim, line_counter
            close_line()
            if claim is not None:
                self._finalise_claim(claim, warnings)
                claims.append(claim)
            claim = None
            line_counter = 0

        for seg in segments:
            name = seg.name

            if name == "CLP":
                close_claim()
                claim = self._parse_claim_header(seg, payment_info)
                continue

            if claim is None:
                continue  # still in the header; handled by _parse_payment

            if name == "SVC":
                close_line()
                line_counter += 1
                line = self._parse_service_line(seg, line_counter)

            elif name == "CAS":
                adjustments = self._parse_cas(seg, "line" if line is not None else "claim")
                if line is not None:
                    line_adjustments.extend(adjustments)
                else:
                    claim.claim_level_adjustments.extend(adjustments)

            elif name == "NM1":
                qualifier = seg.el(1)
                if qualifier == "QC":  # patient
                    claim.patient_name = person_name(seg)
                    claim.patient_id = seg.el(9) or claim.patient_id
                elif qualifier in ("82", "85"):  # rendering / billing provider
                    claim.provider_name = person_name(seg)
                    if seg.el(8) == "XX":
                        claim.provider_npi = seg.el(9) or None

            elif name == "DMG":
                claim.date_of_birth = to_date(seg.el(2)) or claim.date_of_birth

            elif name == "DTM":
                qualifier = seg.el(1)
                value = to_date(seg.el(2))
                if qualifier == "472" and line is not None:
                    line.service_date = value
                elif qualifier == "232":
                    claim.service_from = value
                elif qualifier == "233":
                    claim.service_to = value
                elif qualifier == "472":
                    claim.service_from = claim.service_from or value
                    claim.service_to = claim.service_to or value

            elif name == "AMT":
                if seg.el(1) == "B6" and line is not None:
                    line.allowed_amount = to_float(seg.el(2))

            elif name == "REF":
                if seg.el(1) == "6R" and line is not None:
                    line.control_number = seg.el(2) or None

            elif name == "LQ":
                # LQ*HE*<RARC>
                code = seg.el(2)
                if code:
                    if line is not None:
                        line.remark_codes.append(code)
                    else:
                        claim.remark_codes.append(code)

            elif name == "MOA":
                # MOA03..MOA07 carry remark codes.
                for pos in range(3, 8):
                    code = seg.el(pos)
                    if code:
                        claim.remark_codes.append(code)

            elif name == "MIA":
                # MIA05 and MIA20..MIA23 carry remark codes.
                for pos in (5, 20, 21, 22, 23):
                    code = seg.el(pos)
                    if code:
                        claim.remark_codes.append(code)

            elif name in ("SE", "GE", "IEA"):
                close_claim()

        close_claim()

        if not claims:
            warnings.append("No CLP claim loops found in this 835")

        return claims, warnings

    def _parse_claim_header(
        self, seg: Segment, payment_info: ParsedPaymentInfo
    ) -> ParsedClaim:
        """CLP01..CLP09 - note el(N) is CLP-N, which is what the old code got wrong."""
        return ParsedClaim(
            claim_id=seg.el(1) or "UNKNOWN",
            claim_status_code=seg.el(2),
            total_charged=to_float(seg.el(3)),
            total_paid=to_float(seg.el(4)),
            patient_responsibility=to_float(seg.el(5)),
            claim_filing_indicator=seg.el(6) or None,
            payer_claim_control_number=seg.el(7) or None,
            facility_type_code=seg.el(8) or None,
            frequency_code=seg.el(9) or None,
            # Payer identity is a property of the remittance, not the claim,
            # but the DB stores it per-claim, so carry it down.
            payer_name=payment_info.payer_name,
            payer_id_number=payment_info.payer_id_number,
            provider_npi=payment_info.payee_npi,
            provider_name=payment_info.payee_name,
        )

    def _parse_service_line(self, seg: Segment, line_number: int) -> ParsedServiceLine:
        """SVC01 is a composite: qualifier:code:mod1:mod2:mod3:mod4."""
        qualifier = seg.comp(1, 1) or None
        code = seg.comp(1, 2) or None

        # CPT is five digits; HCPCS Level II is a letter followed by four.
        is_cpt = bool(code) and code.isdigit() and len(code) == 5

        return ParsedServiceLine(
            line_number=line_number,
            procedure_qualifier=qualifier,
            cpt_code=code if is_cpt else None,
            hcpcs_code=None if is_cpt else code,
            modifier_1=seg.comp(1, 3) or None,
            modifier_2=seg.comp(1, 4) or None,
            modifier_3=seg.comp(1, 5) or None,
            modifier_4=seg.comp(1, 6) or None,
            charge_amount=to_float(seg.el(2)),
            paid_amount=to_float(seg.el(3)),
            revenue_code=seg.el(4) or None,
            units=to_float(seg.el(5)) or 1.0,
        )

    def _parse_cas(self, seg: Segment, level: str) -> list[ParsedClaimAdjustment]:
        """
        A CAS carries up to six (reason, amount, quantity) triplets:
        CAS01 group, then 02-04, 05-07, 08-10, 11-13, 14-16, 17-19.
        The old parser read only one and took the amount from the wrong field.
        """
        group = seg.el(1)
        adjustments: list[ParsedClaimAdjustment] = []

        for start in (2, 5, 8, 11, 14, 17):
            reason = seg.el(start)
            amount_raw = seg.el(start + 1)
            quantity_raw = seg.el(start + 2)
            if not reason and not amount_raw:
                continue
            adjustments.append(
                ParsedClaimAdjustment(
                    adjustment_group_code=group,
                    reason_code=reason or None,
                    amount=to_float(amount_raw),
                    quantity=to_float(quantity_raw) if quantity_raw else None,
                    level=level,
                )
            )

        return adjustments

    def _finalise_claim(self, claim: ParsedClaim, warnings: list[str]):
        """Derive totals and dates that are not carried on the CLP itself."""
        all_adjustments = list(claim.claim_level_adjustments)
        for detail in claim.service_lines:
            all_adjustments.extend(detail.adjustments)
        claim.total_adjustment = round(sum(a.amount for a in all_adjustments), 2)

        # Attach claim-level remark codes to claim-level adjustments that have none.
        if claim.remark_codes:
            for adjustment in claim.claim_level_adjustments:
                if adjustment.remark_code is None:
                    adjustment.remark_code = claim.remark_codes[0]

        for detail in claim.service_lines:
            remark = detail.service_line.remark_codes[0] if detail.service_line.remark_codes else None
            for adjustment in detail.adjustments:
                if adjustment.remark_code is None:
                    adjustment.remark_code = remark

        # Fall back to the span of the service lines when DTM*232/233 are absent.
        line_dates = [
            d.service_line.service_date
            for d in claim.service_lines
            if d.service_line.service_date
        ]
        if line_dates:
            claim.service_from = claim.service_from or min(line_dates)
            claim.service_to = claim.service_to or max(line_dates)

        if not claim.patient_id:
            claim.patient_id = claim.claim_id

        self._check_balance(claim, warnings)

    def _check_balance(self, claim: ParsedClaim, warnings: list[str]):
        """
        X12 835 requires each claim to balance:
            CLP03 (charged) == CLP04 (paid) + every CAS amount on the claim
        and each service line:
            SVC02 (charge) == SVC03 (paid) + that line's CAS amounts

        A file that does not balance is either malformed or misparsed. Either
        way the caller needs to know, because the failure mode this replaces
        was silently producing confident, wrong numbers.
        """
        expected = round(claim.total_paid + claim.total_adjustment, 2)
        if abs(claim.total_charged - expected) > 0.01:
            warnings.append(
                f"Claim {claim.claim_id} does not balance: charged "
                f"{claim.total_charged:.2f} != paid {claim.total_paid:.2f} "
                f"+ adjustments {claim.total_adjustment:.2f}"
            )

        for detail in claim.service_lines:
            sl = detail.service_line
            line_adjustment = round(sum(a.amount for a in detail.adjustments), 2)
            line_expected = round(sl.paid_amount + line_adjustment, 2)
            if abs(sl.charge_amount - line_expected) > 0.01:
                warnings.append(
                    f"Claim {claim.claim_id} line {sl.line_number} does not balance: "
                    f"charge {sl.charge_amount:.2f} != paid {sl.paid_amount:.2f} "
                    f"+ adjustments {line_adjustment:.2f}"
                )

    # -- denials --

    def _derive_denials(
        self, claims: list[ParsedClaim], payment_info: ParsedPaymentInfo
    ) -> list[ParsedDenial]:
        """
        One denial row per adjustment that carries a CARC.

        Deliberately NOT limited to claims whose CLP02 is 4. A partially
        denied claim - paid in part, with a CO/PI adjustment on one line -
        is the common case this application exists to work on, and the old
        status filter excluded all of them.
        """
        denial_date = (
            payment_info.production_date
            or payment_info.payment_date
            or datetime.now().strftime("%Y-%m-%d")
        )
        denials: list[ParsedDenial] = []

        for claim in claims:
            for adjustment in claim.claim_level_adjustments:
                if not self._is_denial(adjustment):
                    continue
                denials.append(
                    ParsedDenial(
                        claim_id=claim.claim_id,
                        charge_amount=claim.total_charged,
                        payment_amount=claim.total_paid,
                        adjustment_amount=adjustment.amount,
                        cagc=adjustment.adjustment_group_code,
                        carc_code=adjustment.reason_code,
                        rarc_code=adjustment.remark_code,
                        denial_reason=adjustment.description,
                        denial_date=denial_date,
                    )
                )

            for detail in claim.service_lines:
                sl = detail.service_line
                for adjustment in detail.adjustments:
                    if not self._is_denial(adjustment):
                        continue
                    denials.append(
                        ParsedDenial(
                            claim_id=claim.claim_id,
                            service_line_number=sl.line_number,
                            cpt_code=sl.cpt_code,
                            hcpcs_code=sl.hcpcs_code,
                            modifier_1=sl.modifier_1,
                            modifier_2=sl.modifier_2,
                            charge_amount=sl.charge_amount,
                            payment_amount=sl.paid_amount,
                            adjustment_amount=adjustment.amount,
                            cagc=adjustment.adjustment_group_code,
                            carc_code=adjustment.reason_code,
                            rarc_code=adjustment.remark_code,
                            denial_reason=adjustment.description,
                            denial_date=denial_date,
                        )
                    )

        return denials

    def _is_denial(self, adjustment: ParsedClaimAdjustment) -> bool:
        return (
            adjustment.adjustment_group_code in VALID_GROUP_CODES
            and bool(adjustment.reason_code)
            and adjustment.amount != 0.0
        )

    def __repr__(self):
        return "<X12Parser 835>"
