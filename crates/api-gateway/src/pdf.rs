//! A minimal PDF writer for the appeal packet (FB-15).
//!
//! No page-layout crate is a workspace dependency, and adding one just for a
//! plain-text document is a bigger dependency than the job needs. `lopdf`
//! already ships transitively through `pdf-extract` (used elsewhere to read
//! PDFs) and can write one too — this builds a single Helvetica-only,
//! left-aligned, paginated document directly from lopdf's object model:
//! word-wrap to a fixed character width, a running Y cursor, and a new page
//! whenever a line would fall below the bottom margin.
//!
//! Deliberately not a general text-layout engine: no bold/italic, no tables,
//! no embedded images. The packet is plain prose and lists, which is all an
//! appeal letter, a claim summary and an attachment index need.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

const PAGE_WIDTH: f32 = 612.0; // US Letter, points
const PAGE_HEIGHT: f32 = 792.0;
const MARGIN: f32 = 54.0; // 0.75"
const FONT_SIZE: f32 = 10.5;
const LINE_HEIGHT: f32 = FONT_SIZE * 1.35;
const HEADING_SIZE: f32 = 13.0;
// Helvetica has no fixed character width, but at 10.5pt an average glyph
// (mixed-case English prose) runs close to 0.52em; this keeps wrapped lines
// comfortably inside the margins without measuring each glyph.
const AVG_CHAR_WIDTH_EM: f32 = 0.52;

/// One block of the packet, in the order it should print.
pub enum PacketBlock {
    Heading(String),
    Paragraph(String),
    /// A label/value pair rendered as one line ("Claim number: PCN10004").
    Field(String, String),
    /// A bullet list (the attachment index, the resolution steps).
    List(Vec<String>),
    Spacer,
}

fn wrap(text: &str, width_pts: f32, font_size: f32) -> Vec<String> {
    let max_chars = ((width_pts / (font_size * AVG_CHAR_WIDTH_EM)) as usize).max(10);
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if candidate.chars().count() > max_chars && !current.is_empty() {
                lines.push(current);
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

/// Escapes the characters PDF string literals treat specially.
fn pdf_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
        // The base-14 fonts are Latin-1 (WinAnsiEncoding); anything outside
        // it would print as garbage, so it is dropped rather than corrupt
        // the rest of the line.
        .chars()
        .map(|c| if (c as u32) < 256 { c } else { '?' })
        .collect()
}

struct Layout {
    ops: Vec<Vec<Operation>>, // one Vec<Operation> per page
    y: f32,
    page_width: f32,
}

impl Layout {
    fn new() -> Self {
        let mut layout = Self {
            ops: vec![Vec::new()],
            y: PAGE_HEIGHT - MARGIN,
            page_width: PAGE_WIDTH - 2.0 * MARGIN,
        };
        layout.start_page();
        layout
    }

    fn start_page(&mut self) {
        if !self.ops.last().is_some_and(Vec::is_empty) {
            self.ops.push(Vec::new());
        }
        self.y = PAGE_HEIGHT - MARGIN;
    }

    fn ensure_room(&mut self, needed: f32) {
        if self.y - needed < MARGIN {
            self.start_page();
        }
    }

    fn line(&mut self, text: &str, size: f32, indent: f32) {
        self.ensure_room(LINE_HEIGHT);
        let page = self.ops.last_mut().expect("at least one page");
        page.push(Operation::new("BT", vec![]));
        page.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
        page.push(Operation::new(
            "Td",
            vec![(MARGIN + indent).into(), self.y.into()],
        ));
        page.push(Operation::new(
            "Tj",
            vec![Object::string_literal(pdf_escape(text))],
        ));
        page.push(Operation::new("ET", vec![]));
        self.y -= LINE_HEIGHT;
    }

    fn render(&mut self, block: &PacketBlock) {
        match block {
            PacketBlock::Heading(text) => {
                self.y -= LINE_HEIGHT * 0.3;
                self.line(text, HEADING_SIZE, 0.0);
                self.y -= LINE_HEIGHT * 0.2;
            }
            PacketBlock::Paragraph(text) => {
                for line in wrap(text, self.page_width, FONT_SIZE) {
                    self.line(&line, FONT_SIZE, 0.0);
                }
            }
            PacketBlock::Field(label, value) => {
                for line in wrap(&format!("{label}: {value}"), self.page_width, FONT_SIZE) {
                    self.line(&line, FONT_SIZE, 0.0);
                }
            }
            PacketBlock::List(items) => {
                for item in items {
                    for (i, line) in wrap(item, self.page_width - 14.0, FONT_SIZE)
                        .into_iter()
                        .enumerate()
                    {
                        let prefix = if i == 0 { "- " } else { "  " };
                        self.line(&format!("{prefix}{line}"), FONT_SIZE, 14.0);
                    }
                }
            }
            PacketBlock::Spacer => self.y -= LINE_HEIGHT * 0.6,
        }
    }
}

/// Renders `blocks` into a paginated PDF and returns the file bytes.
pub fn render(title: &str, blocks: &[PacketBlock]) -> Result<Vec<u8>, String> {
    let mut layout = Layout::new();
    layout.line(title, HEADING_SIZE + 2.0, 0.0);
    layout.y -= LINE_HEIGHT * 0.5;
    for block in blocks {
        layout.render(block);
    }

    let mut doc = Document::with_version("1.5");
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let pages_id = doc.new_object_id();
    let mut page_ids = Vec::new();
    for page_ops in &layout.ops {
        let content = Content {
            operations: page_ops.clone(),
        };
        let content_bytes = content.encode().map_err(|e| e.to_string())?;
        let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        page_ids.push(Object::Reference(page_id));
    }
    let page_count = page_ids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids,
            "Count" => page_count,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{pdf_escape, render, wrap, PacketBlock};

    #[test]
    fn wraps_long_text_without_splitting_words() {
        let lines = wrap(
            "The quick brown fox jumps over the lazy dog again and again",
            120.0,
            10.5,
        );
        assert!(lines.len() > 1, "should have wrapped onto multiple lines");
        for line in &lines {
            for word in line.split_whitespace() {
                assert!(
                    "The quick brown fox jumps over the lazy dog again and again".contains(word),
                    "wrapping must not split or invent words, got {word:?}"
                );
            }
        }
    }

    #[test]
    fn escapes_pdf_string_special_characters() {
        assert_eq!(pdf_escape("Smith (M.D.)"), "Smith \\(M.D.\\)");
        assert_eq!(pdf_escape("C:\\path"), "C:\\\\path");
    }

    #[test]
    fn a_long_document_produces_more_than_one_page() {
        let paragraph = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(40);
        let blocks: Vec<PacketBlock> = (0..20)
            .map(|_| PacketBlock::Paragraph(paragraph.clone()))
            .collect();
        let bytes = render("Test Packet", &blocks).expect("renders");
        // A crude but reliable proxy: lopdf emits one "/Type /Page" per page
        // object in the trailer-adjacent object stream even after compress().
        let doc = lopdf::Document::load_mem(&bytes).expect("valid PDF");
        assert!(
            doc.get_pages().len() > 1,
            "expected pagination to add a second page"
        );
    }

    #[test]
    fn renders_a_short_packet_as_valid_pdf() {
        let blocks = vec![
            PacketBlock::Heading("Claim Summary".into()),
            PacketBlock::Field("Claim number".into(), "PCN10004".into()),
            PacketBlock::List(vec!["remittance.pdf".into(), "notes.txt".into()]),
        ];
        let bytes = render("Appeal Packet", &blocks).expect("renders");
        assert!(bytes.starts_with(b"%PDF-"));
        let doc = lopdf::Document::load_mem(&bytes).expect("valid PDF");
        assert_eq!(doc.get_pages().len(), 1);
    }
}
