//! Builds the pinned Semble Vue grammar from its published sources; no generator or downloads are needed.

fn main() {
    let src = std::path::Path::new("grammars/vue/src");
    println!("cargo:rerun-if-changed={}", src.display());

    let mut build = cc::Build::new();
    build.std("c11").include(src).warnings(false);
    build.file(src.join("parser.c")).file(src.join("scanner.c"));

    // Prefixed entry points let a host binary link this alongside a different Vue grammar.
    for suffix in [
        "",
        "_external_scanner_create",
        "_external_scanner_destroy",
        "_external_scanner_scan",
        "_external_scanner_serialize",
        "_external_scanner_deserialize",
    ] {
        build.define(
            &format!("tree_sitter_vue{suffix}"),
            format!("decode_semantic_tree_sitter_vue{suffix}").as_str(),
        );
    }
    build.compile("decode-semantic-tree-sitter-vue");
}
