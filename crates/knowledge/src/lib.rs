//! Text chunking. Ported from `rag-engine/main.py::chunk_text`.

/// Split `text` into chunks of at most `size` characters, preferring a
/// paragraph break, then a sentence end, then a line, then a space, with
/// `overlap` characters carried into the next chunk for context.
pub fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= size {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start = 0usize;
    while start < len {
        let mut end = (start + size).min(len);
        if end < len {
            let search_start = start + size / 2;
            // Prefer a paragraph break, then a sentence end, then a line, then
            // a space — the last occurrence in the window, mirroring Python's
            // `text.rfind(sep, search_start, end)`.
            for sep in ["\n\n", ". ", "\n", " "] {
                let sep_chars: Vec<char> = sep.chars().collect();
                let mut cut = None;
                let mut i = search_start;
                while i + sep_chars.len() <= end {
                    if chars[i..i + sep_chars.len()] == sep_chars[..] {
                        cut = Some(i);
                    }
                    i += 1;
                }
                if let Some(c) = cut {
                    end = c + sep_chars.len();
                    break;
                }
            }
        }
        let piece: String = chars[start..end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if !piece.is_empty() {
            chunks.push(piece);
        }
        if end >= len {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::chunk_text;

    #[test]
    fn returns_no_chunks_for_blank_input() {
        assert!(chunk_text("  \n", 100, 10).is_empty());
    }

    #[test]
    fn preserves_overlap_between_bounded_chunks() {
        let chunks = chunk_text("abcdefghij", 6, 2);

        assert_eq!(chunks, vec!["abcdef", "efghij"]);
    }
}
