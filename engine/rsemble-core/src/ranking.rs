//! Semble `ranking/`: query classification, definition and path-keyword matching, and file-path penalties. The
//! score bookkeeping that uses them lives in `search`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::tokens::{for_each_identifier, split_identifier};

/// Semble `_ALPHA_SYMBOL`: symbol queries lean on BM25.
const ALPHA_SYMBOL: f64 = 0.3;
/// Semble `_ALPHA_NL`: natural-language queries weigh both equally.
const ALPHA_NL: f64 = 0.5;

/// Semble `_SYMBOL_QUERY_RE`.
#[expect(clippy::expect_used, reason = "a constant pattern, covered by tests")]
static SYMBOL_QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:",
        r"[A-Za-z_][A-Za-z0-9_]*(?:(?:::|\\|->|\.)[A-Za-z_][A-Za-z0-9_]*)+",
        r"|_[A-Za-z0-9_]*",
        r"|[A-Za-z][A-Za-z0-9]*[A-Z_][A-Za-z0-9_]*",
        r"|[A-Z][A-Za-z0-9]*",
        r")$"
    ))
    .expect("symbol query pattern")
});

/// Semble `_EMBEDDED_SYMBOL_RE`: PascalCase or camelCase identifiers inside a natural-language query.
#[expect(clippy::expect_used, reason = "a constant pattern, covered by tests")]
static EMBEDDED_SYMBOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:[A-Z][a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]*|[a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]+)\b")
        .expect("embedded symbol pattern")
});

/// Semble `_DEFINITION_KEYWORDS` (case-sensitive).
const DEFINITION_KEYWORDS: &[&str] = &[
    "class",
    "module",
    "defmodule",
    "def",
    "interface",
    "struct",
    "enum",
    "trait",
    "type",
    "func",
    "function",
    "object",
    "abstract class",
    "data class",
    "fn",
    "fun",
    "package",
    "namespace",
    "protocol",
    "record",
    "typedef",
];

/// Semble `_SQL_DEFINITION_KEYWORDS` (case-insensitive).
const SQL_DEFINITION_KEYWORDS: &[&str] = &[
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE PROCEDURE",
    "CREATE FUNCTION",
];

/// Semble `_STOPWORDS`.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "has", "have", "how", "if",
    "in", "is", "it", "not", "of", "on", "or", "the", "to", "was", "what", "when", "where", "which", "who",
    "why", "with",
];

/// Semble `_EMBEDDED_STEM_MIN_LEN`.
const EMBEDDED_STEM_MIN_LEN: usize = 4;

/// Semble `is_symbol_query`: a bare or namespace-qualified identifier, not a plain lowercase word.
pub(crate) fn is_symbol_query(query: &str) -> bool {
    SYMBOL_QUERY.is_match(query.trim())
}

/// Semble `resolve_alpha`: the weight of the semantic score.
pub fn resolve_alpha(query: &str) -> f64 {
    if is_symbol_query(query) {
        ALPHA_SYMBOL
    } else {
        ALPHA_NL
    }
}

/// Semble `_extract_symbol_name`: the last identifier of a namespace-qualified query.
pub(crate) fn extract_symbol_name(query: &str) -> &str {
    for separator in ["::", "\\", "->", "."] {
        if let Some((_, last)) = query.rsplit_once(separator) {
            return last;
        }
    }
    query.trim()
}

/// The identifiers `_EMBEDDED_SYMBOL_RE` finds in a natural-language query.
pub(crate) fn embedded_symbols(query: &str) -> BTreeSet<String> {
    EMBEDDED_SYMBOL
        .find_iter(query)
        .map(|found| found.as_str().to_owned())
        .collect()
}

/// Semble `_definition_pattern` for one symbol: the general and the SQL definition matcher.
#[derive(Debug)]
pub(crate) struct DefinitionPattern {
    general: Regex,
    sql: Regex,
}

impl DefinitionPattern {
    /// Matchers for definitions of `symbol`. Python's `(?<=\s)` lookbehind becomes a consumed `\s`: only whether a
    /// match exists is ever asked.
    pub(crate) fn new(symbol: &str) -> Option<Self> {
        let alternatives = |keywords: &[&str]| {
            keywords
                .iter()
                .map(|k| regex::escape(k))
                .collect::<Vec<_>>()
                .join("|")
        };
        let suffix = format!(
            r")\s+(?:[A-Za-z_][A-Za-z0-9_]*(?:\.|::))*{}(?:\s|[<({{:\[;]|$)",
            regex::escape(symbol)
        );
        Some(Self {
            general: Regex::new(&format!(
                r"(?m)(?:^|\s)(?:{}{suffix}",
                alternatives(DEFINITION_KEYWORDS)
            ))
            .ok()?,
            sql: Regex::new(&format!(
                r"(?mi)(?:^|\s)(?:{}{suffix}",
                alternatives(SQL_DEFINITION_KEYWORDS)
            ))
            .ok()?,
        })
    }

    /// Semble `_chunk_defines_symbol`.
    pub(crate) fn defined_in(&self, content: &str) -> bool {
        self.general.is_match(content) || self.sql.is_match(content)
    }
}

/// Python `Path(path).name`.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Python `Path(path).stem`: the name without its last suffix.
pub(crate) fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name.get(..dot).unwrap_or(name),
        _ => name,
    }
}

/// Python `Path(path).parent.name`.
fn parent_name(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| file_name(parent))
}

/// Semble `_stem_matches`: exact, without underscores, or without trailing `s`.
pub(crate) fn stem_matches(stem: &str, name: &str) -> bool {
    let normalized = stem.replace('_', "");
    stem == name
        || normalized == name
        || stem.trim_end_matches('s') == name
        || normalized.trim_end_matches('s') == name
}

/// The `stem_ok` of Semble `_boost_embedded_symbols`: the stem names one of the symbols or is a long-enough prefix.
pub(crate) fn embedded_stem_ok(stem: &str, symbols_lower: &BTreeSet<String>) -> bool {
    let normalized = stem.replace('_', "");
    symbols_lower.iter().any(|symbol| {
        stem == symbol
            || normalized == *symbol
            || (stem.len() >= EMBEDDED_STEM_MIN_LEN && symbol.starts_with(stem))
            || (normalized.len() >= EMBEDDED_STEM_MIN_LEN && symbol.starts_with(&normalized))
    })
}

/// The keywords of Semble `_boost_stem_matches`: lowercase identifiers longer than two characters, minus stopwords.
pub(crate) fn query_keywords(query: &str) -> BTreeSet<String> {
    let mut keywords = BTreeSet::new();
    for_each_identifier(query, |word| {
        let lower = word.to_ascii_lowercase();
        if word.len() > 2 && !STOPWORDS.contains(&lower.as_str()) {
            keywords.insert(lower);
        }
    });
    keywords
}

/// The path parts Semble `_boost_stem_matches` compares keywords with: the stem's and the parent folder's
/// identifier parts.
pub(crate) fn path_parts(path: &str) -> BTreeSet<String> {
    let mut parts: BTreeSet<String> = split_identifier(file_stem(path)).into_iter().collect();
    let parent = parent_name(path);
    if !parent.is_empty() && parent != "." && parent != ".." {
        parts.extend(split_identifier(parent));
    }
    parts
}

/// Semble `_count_keyword_matches`: exact matches plus keywords sharing a prefix of at least 3 with a part.
pub(crate) fn count_keyword_matches(keywords: &BTreeSet<String>, parts: &BTreeSet<String>) -> usize {
    keywords
        .iter()
        .filter(|keyword| {
            parts.contains(*keyword)
                || parts.iter().any(|part| {
                    let (shorter, longer) = if keyword.len() <= part.len() {
                        (keyword.as_str(), part.as_str())
                    } else {
                        (part.as_str(), keyword.as_str())
                    };
                    shorter.len() >= 3 && longer.starts_with(shorter)
                })
        })
        .count()
}

/// Semble `_TEST_FILE_RE`.
#[expect(clippy::expect_used, reason = "a constant pattern, covered by tests")]
static TEST_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:^|/)(?:",
        r"test_[^/]*\.py|[^/]*_test\.py",
        r"|[^/]*_test\.go",
        r"|[^/]*Tests?\.java",
        r"|[^/]*Test\.php",
        r"|[^/]*_spec\.rb|[^/]*_test\.rb",
        r"|[^/]*\.test\.[jt]sx?|[^/]*\.spec\.[jt]sx?",
        r"|[^/]*Tests?\.kt|[^/]*Spec\.kt",
        r"|[^/]*Tests?\.swift|[^/]*Spec\.swift",
        r"|[^/]*Tests?\.cs",
        r"|test_[^/]*\.cpp|[^/]*_test\.cpp|test_[^/]*\.c|[^/]*_test\.c",
        r"|[^/]*Spec\.scala|[^/]*Suite\.scala|[^/]*Test\.scala",
        r"|[^/]*_test\.dart|test_[^/]*\.dart",
        r"|[^/]*_spec\.lua|[^/]*_test\.lua|test_[^/]*\.lua",
        r"|test_helpers?[^/]*\.\w+",
        r")$"
    ))
    .expect("test file pattern")
});

/// Semble `_TEST_DIR_RE`, `_COMPAT_DIR_RE`, `_EXAMPLES_DIR_RE`, `_TYPE_DEFS_RE`.
#[expect(clippy::expect_used, reason = "constant patterns, covered by tests")]
static PATH_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        r"(?:^|/)(?:tests?|__tests__|spec|testing)(?:/|$)",
        r"(?:^|/)(?:compat|_compat|legacy)(?:/|$)",
        r"(?:^|/)(?:_?examples?|docs?_src)(?:/|$)",
        r"\.d\.ts$",
    ]
    .map(|pattern| Regex::new(pattern).expect("path pattern"))
});

/// Semble `_file_path_penalty`: test files and folders, compat shims, example code ×0.3; `__init__.py` and
/// `package-info.java` ×0.5; `.d.ts` stubs ×0.7.
pub(crate) fn path_penalty(path: &str) -> f64 {
    let normalized = path.replace('\\', "/");
    let [test_dir, compat, examples, type_defs] = &*PATH_PATTERNS;
    let mut penalty = 1.0;
    if TEST_FILE.is_match(&normalized) || test_dir.is_match(&normalized) {
        penalty *= 0.3;
    }
    if matches!(file_name(path), "__init__.py" | "package-info.java") {
        penalty *= 0.5;
    }
    if compat.is_match(&normalized) {
        penalty *= 0.3;
    }
    if examples.is_match(&normalized) {
        penalty *= 0.3;
    }
    if type_defs.is_match(&normalized) {
        penalty *= 0.7;
    }
    penalty
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        DefinitionPattern, count_keyword_matches, embedded_symbols, extract_symbol_name, is_symbol_query,
        path_parts, path_penalty, query_keywords, stem_matches,
    };

    #[test]
    fn classifies_symbol_queries() {
        for symbol in [
            "Sinatra::Base",
            "_private",
            "getUser",
            "snake_case",
            "Client",
            "a.b",
        ] {
            assert!(is_symbol_query(symbol), "{symbol}");
        }
        for text in ["session", "how does auth work", "get user"] {
            assert!(!is_symbol_query(text), "{text}");
        }
        assert_eq!(extract_symbol_name("Sinatra::Base"), "Base");
        assert_eq!(extract_symbol_name(" Client "), "Client");
    }

    #[test]
    fn finds_definitions_with_keyword_and_namespace() {
        let pattern = DefinitionPattern::new("Router").unwrap();
        assert!(pattern.defined_in("defmodule Phoenix.Router do"));
        assert!(pattern.defined_in("x\npub struct Router {"));
        assert!(pattern.defined_in("create table Router;"));
        assert!(!pattern.defined_in("let r = Router::new();"));
        assert!(!pattern.defined_in("structRouter {"));
        assert!(!pattern.defined_in("CLASS Router"));
    }

    #[test]
    fn matches_path_keywords_by_prefix() {
        let keywords = query_keywords("Where are the dependencies resolved for a project?");
        assert_eq!(
            keywords,
            BTreeSet::from(["dependencies", "project", "resolved"].map(String::from))
        );
        // "resolve" prefixes "resolved", "project" is exact, "deps" is no prefix of "dependencies".
        let parts = path_parts("src/deps/resolve_project.rs");
        assert_eq!(count_keyword_matches(&keywords, &parts), 2);
        assert_eq!(
            count_keyword_matches(&keywords, &path_parts("src/project/dependency_graph.rs")),
            1
        );
        assert!(stem_matches("user_sessions", "usersession"));
        assert_eq!(
            embedded_symbols("where is StateManager and getUser"),
            BTreeSet::from(["StateManager".into(), "getUser".into()])
        );
    }

    #[test]
    fn penalises_tests_examples_and_stubs() {
        assert!((path_penalty("src/foo.test.ts") - 0.3).abs() < 1e-9);
        assert!((path_penalty("tests/examples/x.rs") - 0.09).abs() < 1e-9);
        assert!((path_penalty("pkg/__init__.py") - 0.5).abs() < 1e-9);
        assert!((path_penalty("types/index.d.ts") - 0.7).abs() < 1e-9);
        assert!((path_penalty("src/lib.rs") - 1.0).abs() < 1e-9);
    }
}
