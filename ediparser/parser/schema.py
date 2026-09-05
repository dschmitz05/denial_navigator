"""
Pydantic schemas for parsed EDI data
Maps X12 segments to a canonical JSON structure

Field names here are the contract the api-gateway reads in
`_clean_claim` / `_clean_denial` (api-gateway/routes/ingestion.py).
Renaming anything below means changing that file too.
"""

from typing import Optional

from pydantic import BaseModel, Field


# -- Payment Summary --
class ParsedPaymentInfo(BaseModel):
    """BPR segment - Beginning Segment for Payment Order/Remittance Advice"""
    transaction_handling_code: Optional[str] = None   # BPR01: I, H, C, D, ...
    payment_amount: float = 0.0                       # BPR02
    credit_debit_flag: Optional[str] = None           # BPR03: C or D
    payment_method: Optional[str] = None              # BPR04: CHK, ACH, FWT, NON, BOP
    payment_format: Optional[str] = None              # BPR05: CCP, CTX
    payment_date: Optional[str] = None                # BPR16 check/EFT effective date
    trace_number: Optional[str] = None                # TRN02
    payer_identifier: Optional[str] = None            # TRN03
    production_date: Optional[str] = None             # DTM*405
    payer_name: Optional[str] = None                  # N1*PR
    payer_id_number: Optional[str] = None
    payee_name: Optional[str] = None                  # N1*PE
    payee_npi: Optional[str] = None                   # N1*PE with XX qualifier


# -- Claim Adjustment --
class ParsedClaimAdjustment(BaseModel):
    """One adjustment triplet from a CAS segment"""
    adjustment_group_code: str                # CAS01: PR, CO, OA, PI (AB/AS legacy)
    reason_code: Optional[str] = None         # CARC
    amount: float = 0.0
    quantity: Optional[float] = None
    remark_code: Optional[str] = None         # RARC, sourced from LQ / MOA / MIA
    description: Optional[str] = None
    level: str = "claim"                      # "claim" or "line"


# -- Service Line --
class ParsedServiceLine(BaseModel):
    """SVC segment - Service Payment Information"""
    line_number: int
    procedure_qualifier: Optional[str] = None   # SVC01-1: HC, HP, IV, ER, ...
    cpt_code: Optional[str] = None
    hcpcs_code: Optional[str] = None
    modifier_1: Optional[str] = None
    modifier_2: Optional[str] = None
    modifier_3: Optional[str] = None
    modifier_4: Optional[str] = None
    charge_amount: float = 0.0                  # SVC02
    paid_amount: float = 0.0                    # SVC03
    revenue_code: Optional[str] = None          # SVC04
    units: float = 1.0                          # SVC05
    allowed_amount: Optional[float] = None      # AMT*B6
    service_date: Optional[str] = None          # DTM*472
    control_number: Optional[str] = None        # REF*6R
    remark_codes: list[str] = Field(default_factory=list)


# -- Service Line with Adjustments --
class ParsedServiceLineDetail(BaseModel):
    """Combined service line with its adjustments"""
    service_line: ParsedServiceLine
    adjustments: list[ParsedClaimAdjustment] = Field(default_factory=list)


# -- Claim --
class ParsedClaim(BaseModel):
    """CLP segment - Claim Payment Information, plus its loop"""
    claim_id: str                                     # CLP01 patient control number
    claim_status_code: str                            # CLP02: 1 paid, 4 denied, 22 reversal
    total_charged: float = 0.0                        # CLP03
    total_paid: float = 0.0                           # CLP04
    patient_responsibility: float = 0.0               # CLP05
    total_adjustment: float = 0.0                     # summed from CAS, not read from CLP
    claim_filing_indicator: Optional[str] = None      # CLP06
    payer_claim_control_number: Optional[str] = None  # CLP07
    facility_type_code: Optional[str] = None          # CLP08
    frequency_code: Optional[str] = None              # CLP09
    claim_type: str = "professional"                  # DB CHECK: professional/institutional/pharmacy

    patient_id: Optional[str] = None                  # NM1*QC identifier
    patient_name: Optional[str] = None                # NM1*QC name
    date_of_birth: Optional[str] = None               # DMG02 when present
    provider_npi: Optional[str] = None                # NM1*82 rendering provider
    provider_name: Optional[str] = None
    payer_name: Optional[str] = None                  # carried down from the N1*PR loop
    payer_id_number: Optional[str] = None

    service_from: Optional[str] = None                # DTM*232 or earliest line date
    service_to: Optional[str] = None                  # DTM*233 or latest line date
    diagnosis_codes: list[str] = Field(default_factory=list)

    service_lines: list[ParsedServiceLineDetail] = Field(default_factory=list)
    claim_level_adjustments: list[ParsedClaimAdjustment] = Field(default_factory=list)
    remark_codes: list[str] = Field(default_factory=list)


# -- Denial (derived from claim data) --
class ParsedDenial(BaseModel):
    """Denial record derived from a CAS adjustment"""
    claim_id: str
    service_line_number: Optional[int] = None
    cpt_code: Optional[str] = None
    hcpcs_code: Optional[str] = None
    modifier_1: Optional[str] = None
    modifier_2: Optional[str] = None
    charge_amount: float = 0.0
    payment_amount: float = 0.0
    adjustment_amount: float = 0.0
    cagc: str                                   # CO, PR, OA, PI - DB CHECK constrains this
    carc_code: Optional[str] = None
    rarc_code: Optional[str] = None
    denial_reason: Optional[str] = None
    denial_date: Optional[str] = None


# -- Transaction Metadata --
class ParsedTransactionMetadata(BaseModel):
    """ISA/GS/ST envelope info"""
    transaction_set_identifier: Optional[str] = None            # ST01: 835 or 837
    transaction_set_control_number: Optional[str] = None        # ST02
    implementation_convention_reference: Optional[str] = None   # ST03
    interchange_sender_id: Optional[str] = None                 # ISA06
    interchange_receiver_id: Optional[str] = None               # ISA08
    functional_group_code: Optional[str] = None                 # GS01
    version_identifier: Optional[str] = None                    # GS08
    element_separator: Optional[str] = None
    component_separator: Optional[str] = None
    segment_terminator: Optional[str] = None


# -- Full Parsed Response --
class Parsed835Response(BaseModel):
    """Complete parsed EDI response (835 remittance or 837 claim submission)"""
    metadata: ParsedTransactionMetadata
    payment_info: ParsedPaymentInfo
    claims: list[ParsedClaim]
    denials: list[ParsedDenial]
    total_claims: int
    total_denials: int
    total_charges: float
    total_payments: float
    total_adjustments: float
    warnings: list[str] = Field(default_factory=list)
