//! Rust binding to the Vue parser built from this crate's vendored C sources.

use tree_sitter_language::LanguageFn;

#[expect(
    unsafe_code,
    reason = "the Vue entry point is a C function defined in the vendored parser.c"
)]
unsafe extern "C" {
    fn rsemble_tree_sitter_vue() -> *const ();
}

#[expect(
    unsafe_code,
    reason = "the Vue entry point returns the static TSLanguage of the vendored parser"
)]
pub(crate) const LANGUAGE_VUE: LanguageFn = unsafe { LanguageFn::from_raw(rsemble_tree_sitter_vue) };
