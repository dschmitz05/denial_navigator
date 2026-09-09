//! Shared X12 primitives: delimiter detection, segment splitting, element access.

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
        let elements: Vec<String> = raw
            .split(delims.element)
            .map(|s| s.to_string())
            .collect();
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
        return Delimiters::new(
            DEFAULT_ELEMENT,
            DEFAULT_COMPONENT,
            DEFAULT_SEGMENT,
        );
    };

    if content.len() < isa_at + 4 {
        return Delimiters::new(
            DEFAULT_ELEMENT,
            DEFAULT_COMPONENT,
            DEFAULT_SEGMENT,
        );
    }

    let element = content.as_bytes()[isa_at + 3] as char;
    if element.is_alphanumeric() || element.is_whitespace() {
        return Delimiters::new(
            DEFAULT_ELEMENT,
            DEFAULT_COMPONENT,
            DEFAULT_SEGMENT,
        );
    }

    let bytes = content.as_bytes();
    let mut cursor = isa_at;
    for _ in 0..16 {
        match content[cursor + 1..].find(element) {
            Some(offset) => cursor = cursor + 1 + offset,
            None => {
                return Delimiters::new(
                    element,
                    DEFAULT_COMPONENT,
                    DEFAULT_SEGMENT,
                )
            }
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

/// Split on the declared terminator, tolerating pretty-printed files.
pub fn split_segments(content: &str, delims: &Delimiters) -> Vec<Segment> {
    let content = content.replace("\r\n", "\n").replace('\r', "\n");
    let raw_segments: Vec<&str> = if delims.segment == '\n' {
        content.split('\n').collect()
    } else {
        content.split(delims.segment).collect()
    };
    raw_segments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Segment::new(s, delims))
        .collect()
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
        Some(parts.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "))
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
