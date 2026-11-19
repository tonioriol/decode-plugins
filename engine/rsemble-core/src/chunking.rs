//! Splitting a file into chunks (Semble `chunking/`): tree-sitter nodes merged up to ~750 bytes, or lines merged up
//! to ~750 characters when the language has no grammar here.

use std::cell::RefCell;
use std::collections::BTreeMap;

use tree_sitter::{Node, Parser};

use crate::languages::{ExtraGrammar, grammar};

/// Semble `_DESIRED_CHUNK_LENGTH_CHARS`.
const DESIRED_LENGTH: usize = 750;
/// Semble `_RECURSION_DEPTH`.
const RECURSION_DEPTH: usize = 500;
/// Semble `_MIN_CHUNK_SIZE`: nodes shorter than this are never split.
const MIN_CHUNK_SIZE: usize = 50;

/// One chunk of a file: its text and 1-based inclusive line range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkText {
    /// The chunk's source text.
    pub content: String,
    /// First line, 1-based.
    pub start_line: u32,
    /// Last line, 1-based, inclusive.
    pub end_line: u32,
}

/// A byte range of the source and its length in Semble's unit (bytes for tree-sitter, characters for lines).
#[derive(Debug, Clone, Copy)]
struct Boundary {
    start: usize,
    end: usize,
    length: usize,
}

impl Boundary {
    fn bytes(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            length: end - start,
        }
    }
}

thread_local! {
    static PARSERS: RefCell<BTreeMap<&'static str, Parser>> = const { RefCell::new(BTreeMap::new()) };
}

/// Semble `chunk_source`: the chunks of `source`, by grammar when `language` has one here, else by lines.
///
/// `extra_grammar` supplies the grammars of languages this crate has none for (see [`crate::ExtraGrammar`]).
pub(crate) fn chunk_source(
    source: &str,
    language: Option<&'static str>,
    extra_grammar: ExtraGrammar,
) -> Vec<ChunkText> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let boundaries = language
        .and_then(|language| chunk_tree(source, language, extra_grammar))
        .unwrap_or_else(|| chunk_lines(source));
    let newlines: Vec<usize> = source.match_indices('\n').map(|(at, _)| at).collect();
    let line_of =
        |offset: usize| u32::try_from(newlines.partition_point(|at| *at < offset) + 1).unwrap_or(u32::MAX);
    boundaries
        .into_iter()
        .filter_map(|boundary| {
            // Semble keeps `source[start : max(end - 1, start) + 1]`: an empty range still yields one character.
            let end = if boundary.end > boundary.start {
                boundary.end
            } else {
                boundary.start + source.get(boundary.start..)?.chars().next()?.len_utf8()
            };
            let content = source.get(boundary.start..end)?;
            let last_char = content.char_indices().last().map_or(0, |(at, _)| at);
            Some(ChunkText {
                content: content.to_owned(),
                start_line: line_of(boundary.start),
                end_line: line_of(boundary.start + last_char),
            })
        })
        .collect()
}

/// Prose split as Semble splits a file without a grammar: lines merged up to ~750 characters.
pub fn chunk_lines_text(text: &str) -> Vec<ChunkText> {
    chunk_source(text, None, |_| None)
}

/// Semble `chunk`: merged tree-sitter node ranges, or `None` without a grammar or parse.
fn chunk_tree(source: &str, language: &'static str, extra_grammar: ExtraGrammar) -> Option<Vec<Boundary>> {
    let grammar = grammar(language).or_else(|| extra_grammar(language))?;
    let tree = PARSERS.with_borrow_mut(|parsers| {
        let parser = match parsers.entry(language) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let mut parser = Parser::new();
                parser.set_language(&grammar).ok()?;
                entry.insert(parser)
            }
        };
        parser.parse(source, None)
    })?;
    let mut raw = Vec::new();
    merge_node_inner(tree.root_node(), 0, &mut raw);
    Some(merge_adjacent(&raw))
}

/// Semble `_merge_node_inner`: a node's children grouped into runs of at most the desired length, recursing into
/// children longer than that.
fn merge_node_inner(node: Node<'_>, depth: usize, out: &mut Vec<Boundary>) {
    let whole = Boundary::bytes(node.start_byte(), node.end_byte());
    if node.child_count() == 0 || depth > RECURSION_DEPTH || whole.length < MIN_CHUNK_SIZE {
        out.push(whole);
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    let mut index = 0;
    while let Some(child) = children.get(index) {
        let start = child.start_byte();
        let mut end = child.end_byte();
        let mut length = end - start;
        index += 1;
        if length > DESIRED_LENGTH {
            merge_node_inner(*child, depth + 1, out);
            continue;
        }
        while let Some(next) = children.get(index) {
            let next_length = next.end_byte() - next.start_byte();
            if length + next_length > DESIRED_LENGTH {
                break;
            }
            end = next.end_byte();
            length += next_length;
            index += 1;
        }
        out.push(Boundary::bytes(start, end));
    }
}

/// Semble `_merge_adjacent_chunks`: consecutive ranges joined while their summed lengths fit the desired length.
fn merge_adjacent(chunks: &[Boundary]) -> Vec<Boundary> {
    let Some(first) = chunks.first() else {
        return Vec::new();
    };
    let mut merged = Vec::new();
    let mut current = *first;
    for chunk in chunks.iter().skip(1) {
        if current.length + chunk.length > DESIRED_LENGTH {
            merged.push(current);
            current = *chunk;
            continue;
        }
        current.end = chunk.end;
        current.length += chunk.length;
    }
    merged.push(current);
    merged
}

/// Semble `chunk_lines`: Python `str.splitlines(keepends=True)` lines, merged by character count.
fn chunk_lines(source: &str) -> Vec<Boundary> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut length = 0;
    let mut chars = source.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        length += 1;
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        let mut end = at + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            chars.next();
            end += 1;
            length += 1;
        }
        lines.push(Boundary { start, end, length });
        start = end;
        length = 0;
    }
    if start < source.len() {
        lines.push(Boundary {
            start,
            end: source.len(),
            length,
        });
    }
    merge_adjacent(&lines)
}

#[cfg(test)]
mod tests {
    use super::{ChunkText, chunk_source};

    fn spans(chunks: &[ChunkText]) -> Vec<(u32, u32)> {
        chunks
            .iter()
            .map(|chunk| (chunk.start_line, chunk.end_line))
            .collect()
    }

    #[test]
    fn line_chunks_merge_up_to_750_characters() {
        // 30 lines of 40 characters: 18 fit in 750 (720), the 19th would overflow.
        let source: String = (0..30).map(|i| format!("{i:038}é\n")).collect();
        let chunks = chunk_source(&source, None, |_| None);
        assert_eq!(spans(&chunks), [(1, 18), (19, 30)]);
        assert_eq!(chunks.first().map(|c| c.content.chars().count()), Some(18 * 40));
    }

    #[test]
    fn tree_chunks_split_long_items_and_merge_short_ones() {
        let long_body: String = (0..40)
            .map(|i| format!("    let value_{i} = {i} * 2 + 1;\n"))
            .collect();
        let source = format!("use a::b;\nuse c::d;\n\nfn long() {{\n{long_body}}}\n\nfn short() {{}}\n");
        let chunks = chunk_source(&source, Some("rust"), |_| None);
        // Semble 0.7.3's output for the same source: lengths sum node bytes, so a chunk's span (with the gaps
        // between nodes) can exceed 750.
        assert_eq!(spans(&chunks), [(1, 4), (4, 33), (34, 47)]);
        let lengths: Vec<usize> = chunks.iter().map(|chunk| chunk.content.len()).collect();
        assert_eq!(lengths, [30, 880, 353]);
    }

    #[test]
    fn a_language_without_a_built_in_grammar_is_chunked_with_the_callers() {
        let long_body: String = (0..40)
            .map(|i| format!("    let value_{i} = {i} * 2 + 1;\n"))
            .collect();
        let source = format!("use a::b;\nuse c::d;\n\nfn long() {{\n{long_body}}}\n\nfn short() {{}}\n");
        let by_lines = chunk_source(&source, Some("lua"), |_| None);
        let by_caller = chunk_source(&source, Some("lua"), |language| {
            (language == "lua").then(|| tree_sitter_rust::LANGUAGE.into())
        });
        assert_eq!(by_caller, chunk_source(&source, Some("rust"), |_| None));
        assert_ne!(by_caller, by_lines);
    }

    #[test]
    fn blank_source_has_no_chunks() {
        assert!(chunk_source(" \n\t\n", Some("rust"), |_| None).is_empty());
    }
}
