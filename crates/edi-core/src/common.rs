//! Shared X12 primitives: delimiter detection, bounded tokenization, element access.

/// X12 input is untrusted. Keep parsing work and allocation bounded even when
/// an upload passed the transport-level size limit.
pub const MAX_SEGMENTS: usize = 200_000;
pub const MAX_SEGMENT_BYTES: usize = 16 * 1024;
pub const MAX_ELEMENTS_PER_SEGMENT: usize = 256;

/// The three separators an X12 interchange declares in its ISA.
#[derive(Debug, Clone)]
pub struct Delimiters {
    pub element: char,
    pub component: char,
    pub segment: char,
}

impl Delimiters {
    pub fn new(element: char, component: char, segment: char) -> Self {
        Self {
            element,
            component,
            segment,
        }
    }
}

/// One X12 segment, with 1-based element access.
#[derive(Debug, Clone)]
pub struct Segment {
    pub name: String,
    pub elements: Vec<String>,
    component_sep: char,
}

impl Segment {
    pub fn new(raw: &str, delims: &Delimiters) -> Self {
        let elements: Vec<String> = raw.split(delims.element).map(|s| s.to_string()).collect();
        let name = elements
            .first()
            .map(|s| s.trim().to_uppercase())
            .unwrap_or_default();
        Self {
            name,
            elements,
            component_sep: delims.component,
        }
    }

    /// Element by X12 position: el(1) is CLP01/CLM01. Never panics.
    pub fn el(&self, position: usize) -> &str {
        if position > 0 && position < self.elements.len() {
            self.elements[position].trim()
        } else {
            ""
        }
    }

    /// Component `index` (1-based) of composite element `position`.
    pub fn comp(&self, position: usize, index: usize) -> &str {
        let value = self.el(position);
        if value.is_empty() {
            return "";
        }
        let parts: Vec<&str> = value.split(self.component_sep).collect();
        if index > 0 && index <= parts.len() {
            parts[index - 1].trim()
        } else {
            ""
        }
    }

    pub fn component_separator(&self) -> char {
        self.component_sep
    }
}

/// Read the separators out of the ISA rather than guessing.
pub fn detect_delimiters(content: &str) -> Delimiters {
    const DEFAULT_ELEMENT: char = '*';
    const DEFAULT_COMPONENT: char = ':';
    const DEFAULT_SEGMENT: char = '~';

    let stripped = content.trim_start();
    let isa_at = if stripped.starts_with("ISA") {
        content.len() - stripped.len()
    } else {
        return Delimiters::new(DEFAULT_ELEMENT, DEFAULT_COMPONENT, DEFAULT_SEGMENT);
    };

    if content.len() < isa_at + 4 {
        return Delimiters::new(DEFAULT_ELEMENT, DEFAULT_COMPONENT, DEFAULT_SEGMENT);
    }

    let element = content.as_bytes()[isa_at + 3] as char;
    if element.is_alphanumeric() || element.is_whitespace() {
        return Delimiters::new(DEFAULT_ELEMENT, DEFAULT_COMPONENT, DEFAULT_SEGMENT);
    }

    let bytes = content.as_bytes();
    let mut cursor = isa_at;
    for _ in 0..16 {
        match content[cursor + 1..].find(element) {
            Some(offset) => cursor = cursor + 1 + offset,
            None => return Delimiters::new(element, DEFAULT_COMPONENT, DEFAULT_SEGMENT),
        }
    }

    let component = if cursor + 1 < bytes.len() {
        bytes[cursor + 1] as char
    } else {
        DEFAULT_COMPONENT
    };
    let mut segment = if cursor + 2 < bytes.len() {
        bytes[cursor + 2] as char
    } else {
        DEFAULT_SEGMENT
    };
    if segment == '\r' || segment == '\n' {
        segment = '\n';
    }

    Delimiters::new(element, component, segment)
}

/// Tokenize an X12 message with bounded segment and element counts. Parsing
/// retains normalized transaction context today, but this iterator-based
/// tokenizer does not first allocate a second vector of raw segment strings.
pub fn tokenize_segments(content: &str, delims: &Delimiters) -> Result<Vec<Segment>, String> {
    let content = content.replace("\r\n", "\n").replace('\r', "\n");
    let raw_segments = if delims.segment == '\n' {
        content.split('\n')
    } else {
        content.split(delims.segment)
    };
    let mut segments = Vec::new();
    for raw in raw_segments.map(str::trim).filter(|s| !s.is_empty()) {
        if raw.len() > MAX_SEGMENT_BYTES {
            return Err("X12 segment exceeds parser limit".into());
        }
        if raw.split(delims.element).count() > MAX_ELEMENTS_PER_SEGMENT {
            return Err("X12 segment has too many elements".into());
        }
        if segments.len() >= MAX_SEGMENTS {
            return Err("X12 message exceeds parser segment limit".into());
        }
        segments.push(Segment::new(raw, delims));
    }
    Ok(segments)
}

/// Ensure generic X12 interchange, functional-group, and transaction envelopes
/// are complete before a version-specific parser consumes their contents.
pub fn validate_envelopes(segments: &[Segment]) -> Result<(), String> {
    if segments.first().map(|s| s.name.as_str()) != Some("ISA") {
        return Err("Missing ISA interchange header".into());
    }
    if segments.last().map(|s| s.name.as_str()) != Some("IEA") {
        return Err("Missing IEA interchange trailer".into());
    }

    let mut in_group = false;
    let mut in_transaction = false;
    let mut transaction_count = 0usize;
    for segment in segments {
        match segment.name.as_str() {
            "GS" if in_group => return Err("Nested or unclosed GS group".into()),
            "GS" => {
                in_group = true;
                transaction_count = 0;
            }
            "GE" if !in_group || in_transaction => {
                return Err("GE trailer without a closed transaction group".into())
            }
            "GE" => {
                if segment.el(1).parse::<usize>().ok() != Some(transaction_count) {
                    return Err("GE transaction count does not match ST count".into());
                }
                in_group = false;
            }
            "ST" if !in_group || in_transaction => {
                return Err("ST transaction outside GS/GE envelope".into())
            }
            "ST" => {
                in_transaction = true;
                transaction_count += 1;
            }
            "SE" if !in_transaction => return Err("SE trailer without ST header".into()),
            "SE" => in_transaction = false,
            _ => {}
        }
    }
    if in_group || in_transaction {
        return Err("Unclosed X12 group or transaction".into());
    }
    Ok(())
}

/// X12 numerics may be signed and may be empty. Never panics.
pub fn to_float(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    value.parse::<f64>().unwrap_or(0.0)
}

/// CCYYMMDD (and legacy YYMMDD) to ISO. Returns None on anything else.
pub fn to_date(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        let y: i32 = value[0..4].parse().ok()?;
        let m: u32 = value[4..6].parse().ok()?;
        let d: u32 = value[6..8].parse().ok()?;
        return format_date(y, m, d);
    }
    if value.len() == 6 && value.chars().all(|c| c.is_ascii_digit()) {
        let yy: i32 = value[0..2].parse().ok()?;
        let m: u32 = value[2..4].parse().ok()?;
        let d: u32 = value[4..6].parse().ok()?;
        let y = if yy <= 68 { 2000 + yy } else { 1900 + yy };
        return format_date(y, m, d);
    }
    None
}

fn format_date(y: i32, m: u32, d: u32) -> Option<String> {
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some(format!("{:04}-{:02}-{:02}", y, m, d))
    } else {
        None
    }
}

/// NM1 name: organisation in NM103, or 'First Last' for a person.
pub fn person_name(seg: &Segment) -> Option<String> {
    let last_or_org = seg.el(3).to_string();
    let first = seg.el(4).to_string();
    if seg.el(2) == "2" {
        return if last_or_org.is_empty() {
            None
        } else {
            Some(last_or_org)
        };
    }
    let mut parts = Vec::new();
    if !first.is_empty() {
        parts.push(&first);
    }
    if !last_or_org.is_empty() {
        parts.push(&last_or_org);
    }
    if parts.is_empty() {
        None
    } else {
        Some(
            parts
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

/// Empty string to None, else Some.
pub fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Round to 2 decimal places.
pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// 0.0 becomes 1.0, else pass through.
pub fn one_or(v: f64) -> f64 {
    if v == 0.0 {
        1.0
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(name: &str, elements: &[&str]) -> Segment {
        let raw = std::iter::once(name)
            .chain(elements.iter().copied())
            .collect::<Vec<_>>()
            .join("*");
        Segment::new(&raw, &Delimiters::new('*', ':', '~'))
    }

    #[test]
    fn tokenization_rejects_a_giant_segment() {
        let input = format!("ISA*{}~", "x".repeat(MAX_SEGMENT_BYTES));
        assert!(tokenize_segments(&input, &Delimiters::new('*', ':', '~')).is_err());
    }

    #[test]
    fn envelope_validation_rejects_mismatched_transaction_count() {
        let segments = vec![
            segment("ISA", &[]),
            segment("GS", &[]),
            segment("ST", &["835", "1"]),
            segment("SE", &["2", "1"]),
            segment("GE", &["2", "1"]),
            segment("IEA", &["1", "1"]),
        ];
        assert!(validate_envelopes(&segments).is_err());
    }
}
