//! Semantic code search: a Rust port of Semble 0.7.3's engine (tree-sitter chunking, model2vec embeddings, BM25,
//! reciprocal-rank fusion and Semble's ranking boosts). Callers supply source contents, stamps and persisted
//! index handles; completed on-disk indexes use read-only mappings for their embedding scans.

mod chunking;
mod embed;
pub mod files;
mod index;
mod language_table;
mod languages;
mod latest;
mod mapped_file;
pub mod model_assets;
pub mod quant;
mod ranking;
mod rerank;
mod search;
mod skipped;
mod sync;
mod tokens;
mod top;
mod vendored_grammar;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod watch;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod watch {
    use std::{collections::BTreeMap, fs::Metadata, io, path::Path};

    use crate::files::{self, Ignores};
    use crate::{Exclusions, FileStamp};

    /// No kernel queue on this platform: every folder is listed on every search.
    #[derive(Debug)]
    pub enum Watch {}

    impl Watch {
        pub fn unchanged(&mut self, _: &Metadata) -> bool {
            false
        }

        pub fn list(
            root: &Path,
            exclusions: &Exclusions,
            docs_only: bool,
            ignores: Ignores,
        ) -> io::Result<(BTreeMap<String, FileStamp>, Option<Self>)> {
            files::list(root, exclusions, docs_only, ignores).map(|listing| (listing, None))
        }
    }
}

pub use chunking::{ChunkText, chunk_lines_text};
pub use embed::Embedder;
#[cfg(any(test, feature = "test-util"))]
pub use embed::test_embedder;
pub use index::{Blob, FileStamp, PreparedFile, SourceIndex};
pub use languages::{ExtraGrammar, detect_language, is_doc_language};
pub use latest::Latest;
pub use mapped_file::MappedFile;
pub use ranking::resolve_alpha;
pub use rerank::Reranker;
pub use search::{Hit, SearchSource, search};
pub use skipped::{SKIPPED_DIRS, in_skipped_dir, is_unindexed};
pub use sync::{Changes, Exclusions, changes};
pub use tokens::tokenize;
pub use top::{TopScores, score_order, top_by_score};
pub use tree_sitter::Language;
pub use watch::Watch;
