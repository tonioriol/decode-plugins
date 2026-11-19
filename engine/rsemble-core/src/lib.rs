//! Semantic code search: a Rust port of Semble 0.7.3's engine (tree-sitter chunking, model2vec embeddings, BM25,
//! reciprocal-rank fusion and Semble's ranking boosts). Callers supply source contents, stamps and persisted
//! index handles; completed on-disk indexes use read-only mappings for their embedding scans.

mod chunking;
mod embed;
mod index;
mod language_table;
mod languages;
mod mapped_file;
mod ranking;
mod rerank;
mod search;
mod sync;
mod tokens;

pub use chunking::{ChunkText, chunk_lines_text};
pub use embed::Embedder;
#[cfg(any(test, feature = "test-util"))]
pub use embed::test_embedder;
pub use index::{Blob, FileStamp, PreparedFile, SourceIndex};
pub use languages::{ExtraGrammar, detect_language, is_doc_language};
pub use mapped_file::MappedFile;
pub use ranking::resolve_alpha;
pub use rerank::Reranker;
pub use search::{Hit, SearchSource, search};
pub use sync::{Changes, Exclusions, changes};
pub use tokens::tokenize;
pub use tree_sitter::Language;
