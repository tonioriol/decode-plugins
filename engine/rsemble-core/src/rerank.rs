//! Query-dependent scoring of a small candidate pool, independent of the index format.

use std::io;

/// A bi-encoder: queries and passages map into one space where the dot product of two L2-normalized vectors
/// scores relevance. A passage's vector does not depend on the query, so an index can compute it once and store it,
/// and a search then embeds only the query. Implementations may keep a loaded inference session and reuse its
/// working memory between calls.
pub trait Reranker {
    /// The vector of `query`.
    fn embed_query(&mut self, query: &str) -> io::Result<Vec<f32>>;
    /// One vector per passage, in their supplied order, each as long as a query's.
    fn embed_passages(&mut self, passages: &[String]) -> io::Result<Vec<Vec<f32>>>;
}
