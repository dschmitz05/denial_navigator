"""
Shared X12 primitives: delimiter detection, segment splitting, element access.

Both the 835 (remittance) and 837 (claim submission) parsers build on this,
so the hard-won rules live in exactly one place:

  * Segments are terminated by a delimiter declared in the ISA, NOT by
    newlines. Real payer files are a single line.
  * `segment.split(sep)[0]` is the segment NAME, so element NN sits at index
    NN. `Segment.el()` is 1-based to keep that from drifting.
  * Composite elements (`HC:99213:25`) split on the separator declared in
    ISA16, which is not always ":".
"""

from datetime import datetime
from typing import Optional

# Used only when no usable ISA is present.
DEFAULT_ELEMENT_SEP = "*"
DEFAULT_COMPONENT_SEP = ":"
DEFAULT_SEGMENT_TERM = "~"


class Delimiters:
    """The three separators an X12 interchange declares in its ISA."""

    __slots__ = ("element", "component", "segment")

    def __init__(self, element: str, component: str, segment: str):
        self.element = element
        self.component = component
        self.segment = segment


class Segment:
    """One X12 segment, with 1-based element access."""

    __slots__ = ("name", "elements", "_delims")

    def __init__(self, raw: str, delims: Delimiters):
        parts = raw.split(delims.element)
        self.name = parts[0].strip().upper()
        self.elements = parts
        self._delims = delims

    def el(self, position: int, default: str = "") -> str:
        """Element by X12 position: el(1) is CLP01/CLM01. Never raises."""
        if 0 < position < len(self.elements):
            return self.elements[position].strip()
        return default

    def comp(self, position: int, index: int, default: str = "") -> str:
        """Component `index` (1-based) of composite element `position`."""
        value = self.el(position)
        if not value:
            return default
        parts = value.split(self._delims.component)
        if 0 < index <= len(parts):
            return parts[index - 1].strip()
        return default

    def __repr__(self):
        return f"<Segment {self.name} n={len(self.elements) - 1}>"


def detect_delimiters(content: str) -> Delimiters:
    """
    Read the separators out of the ISA rather than guessing.

    The element separator is the character right after "ISA". ISA16 is the
    component separator, and the character immediately following it is the
    segment terminator. Walking 16 element separators from the start of the
    ISA is the only reliable way to find ISA16, because ISA fields are
    fixed-width and may legitimately contain spaces.
    """
    isa_at = content.find("ISA")
    if isa_at == -1 or len(content) < isa_at + 4:
        return Delimiters(DEFAULT_ELEMENT_SEP, DEFAULT_COMPONENT_SEP, DEFAULT_SEGMENT_TERM)

    element = content[isa_at + 3]
    if element.isalnum() or element.isspace():
        # Not a plausible separator; fall back rather than corrupt the parse.
        return Delimiters(DEFAULT_ELEMENT_SEP, DEFAULT_COMPONENT_SEP, DEFAULT_SEGMENT_TERM)

    cursor = isa_at
    for _ in range(16):
        nxt = content.find(element, cursor + 1)
        if nxt == -1:
            return Delimiters(element, DEFAULT_COMPONENT_SEP, DEFAULT_SEGMENT_TERM)
        cursor = nxt

    component = content[cursor + 1] if cursor + 1 < len(content) else DEFAULT_COMPONENT_SEP
    terminator = content[cursor + 2] if cursor + 2 < len(content) else DEFAULT_SEGMENT_TERM

    # A terminator of CR/LF is legal; normalise so splitting still works.
    if terminator in ("\r", "\n"):
        terminator = "\n"

    return Delimiters(element, component, terminator)


def split_segments(content: str, delims: Delimiters) -> list[Segment]:
    """Split on the declared terminator, tolerating pretty-printed files."""
    content = content.replace("\r\n", "\n").replace("\r", "\n")
    if delims.segment == "\n":
        raw_segments = content.split("\n")
    else:
        # Newlines after a terminator are cosmetic; strip them so a file that
        # is both terminated and line-wrapped still works.
        raw_segments = content.split(delims.segment)
    out = []
    for raw in raw_segments:
        raw = raw.strip().strip("\n").strip()
        if raw:
            out.append(Segment(raw, delims))
    return out


def to_float(value: str) -> float:
    """X12 numerics may be signed and may be empty. Never raise."""
    if not value:
        return 0.0
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def to_date(value: str) -> Optional[str]:
    """CCYYMMDD (and legacy YYMMDD) to ISO. Returns None on anything else."""
    if not value:
        return None
    value = value.strip()
    if len(value) == 8 and value.isdigit():
        try:
            return datetime.strptime(value, "%Y%m%d").strftime("%Y-%m-%d")
        except ValueError:
            return None
    if len(value) == 6 and value.isdigit():
        try:
            return datetime.strptime(value, "%y%m%d").strftime("%Y-%m-%d")
        except ValueError:
            return None
    return None


def person_name(seg: Segment) -> Optional[str]:
    """NM1 name: organisation in NM103, or 'First Last' for a person."""
    last_or_org = seg.el(3)
    first = seg.el(4)
    if seg.el(2) == "2":  # non-person entity
        return last_or_org or None
    parts = [p for p in (first, last_or_org) if p]
    return " ".join(parts) or None
