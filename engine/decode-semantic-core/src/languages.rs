//! Which files Semble indexes (by extension) and the tree-sitter grammar each language is chunked with.

use tree_sitter::Language;

use crate::language_table::{EXTENSIONS, Kind};

/// The language of `path` by its last extension, lowercased (Python `Path.suffix.lower()`), if Semble indexes it as
/// code or docs.
pub fn detect_language(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    // `Path.suffix`: the text after the last dot, unless the name starts with that dot or ends with it.
    let dot = name.rfind('.').filter(|dot| *dot > 0 && *dot + 1 < name.len())?;
    let extension = name.get(dot + 1..)?.to_ascii_lowercase();
    EXTENSIONS
        .binary_search_by(|(ext, _, _)| (*ext).cmp(extension.as_str()))
        .ok()
        .and_then(|index| EXTENSIONS.get(index))
        .map(|(_, language, _)| *language)
}

/// Whether `language` is documentation (Markdown, reStructuredText, …) rather than code.
pub fn is_doc_language(language: &str) -> bool {
    EXTENSIONS
        .iter()
        .any(|(_, name, kind)| *name == language && *kind == Kind::Docs)
}

/// The grammar of a language [`grammar`] has none for, supplied by the caller: `None` leaves the language to line
/// chunking.
pub type ExtraGrammar = fn(&str) -> Option<Language>;

/// The built-in tree-sitter grammar for `language`, when one is available.
/// Others fall back to line chunking, as Semble does for a language its bundle lacks.
pub(crate) fn grammar(language: &str) -> Option<Language> {
    let function = match language {
        "bash" => tree_sitter_bash::LANGUAGE,
        "c" => tree_sitter_c::LANGUAGE,
        "cpp" => tree_sitter_cpp::LANGUAGE,
        "csharp" => tree_sitter_c_sharp::LANGUAGE,
        "css" => tree_sitter_css::LANGUAGE,
        "dart" => tree_sitter_dart_orchard::LANGUAGE,
        "elixir" => tree_sitter_elixir::LANGUAGE,
        "go" => tree_sitter_go::LANGUAGE,
        "haskell" => tree_sitter_haskell::LANGUAGE,
        "html" => tree_sitter_html::LANGUAGE,
        "java" => tree_sitter_java::LANGUAGE,
        "javascript" => tree_sitter_javascript::LANGUAGE,
        "kotlin" => tree_sitter_kotlin_sg::LANGUAGE,
        "lua" => tree_sitter_lua::LANGUAGE,
        "markdown" => tree_sitter_md::LANGUAGE,
        "ocaml" => tree_sitter_ocaml::LANGUAGE_OCAML,
        // The workspace pins tree-sitter-php 0.22, which still exposes a function returning the `Language`.
        "php" => return Some(tree_sitter_php::language_php()),
        "python" => tree_sitter_python::LANGUAGE,
        "ruby" => tree_sitter_ruby::LANGUAGE,
        "rust" => tree_sitter_rust::LANGUAGE,
        "solidity" => tree_sitter_solidity::LANGUAGE,
        "swift" => tree_sitter_swift::LANGUAGE,
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX,
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        "vue" => crate::vendored_grammar::LANGUAGE_VUE,
        "zig" => tree_sitter_zig::LANGUAGE,
        _ => return None,
    };
    Some(function.into())
}

#[cfg(test)]
mod tests {
    use super::{detect_language, grammar};
    use crate::language_table::EXTENSIONS;

    #[test]
    fn detects_by_last_lowercased_extension() {
        assert_eq!(detect_language("src/a.d.ts"), Some("typescript"));
        assert_eq!(detect_language("A.RS"), Some("rust"));
        assert_eq!(detect_language("docs/README.md"), Some("markdown"));
        assert_eq!(detect_language(".rs"), None);
        assert_eq!(detect_language("Makefile"), None);
        assert_eq!(detect_language("data.json"), None);
    }

    #[test]
    fn table_is_sorted_for_binary_search() {
        assert!(
            EXTENSIONS
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if a.0 < b.0))
        );
    }

    #[test]
    fn every_grammar_loads_into_a_parser() {
        for (_, language, _) in EXTENSIONS {
            if let Some(grammar) = grammar(language) {
                assert!(
                    tree_sitter::Parser::new().set_language(&grammar).is_ok(),
                    "{language}"
                );
            }
        }
    }
}
