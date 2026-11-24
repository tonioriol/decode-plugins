//! Semble `search.py` plus the score bookkeeping of `ranking/`: semantic and BM25 candidates fused by reciprocal
//! rank, file-coherence and definition / path-keyword boosts, then path penalties and per-file saturation. Several
//! sources can be searched together: their BM25 statistics are pooled, as Semble's `SembleIndex.merge` does, and
//! each source's fused scores are scaled by its boost.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::embed::Embedder;
use crate::index::{Slot, SourceIndex};
use crate::ranking::{
    DefinitionPattern, count_keyword_matches, embedded_stem_ok, embedded_symbols, extract_symbol_name,
    file_stem, is_symbol_query, path_parts, path_penalty, query_keywords, resolve_alpha, stem_matches,
};
use crate::tokens::tokenize;

/// Semble `_RRF_K`.
const RRF_K: f64 = 60.0;
/// BM25 term-frequency saturation (`_K1`).
const K1: f64 = 1.5;
/// BM25 document-length normalization (`_B`).
const B: f64 = 0.75;
/// Semble `_FILE_COHERENCE_BOOST_FRAC`.
const FILE_COHERENCE_BOOST_FRAC: f64 = 0.2;
/// Semble `_DEFINITION_BOOST_MULTIPLIER`.
const DEFINITION_BOOST: f64 = 3.0;
/// Semble `_EMBEDDED_SYMBOL_BOOST_SCALE`.
const EMBEDDED_SYMBOL_SCALE: f64 = 0.5;
/// Semble `_STEM_BOOST_MULTIPLIER`.
const STEM_BOOST: f64 = 1.0;
/// Semble `_FILE_SATURATION_THRESHOLD`.
const SATURATION_THRESHOLD: usize = 1;
/// Semble `_FILE_SATURATION_DECAY`.
const SATURATION_DECAY: f64 = 0.5;

/// One source to search and how much its results weigh.
#[derive(Debug, Clone, Copy)]
pub struct SearchSource<'a> {
    /// The name hits are labelled with.
    pub name: &'a str,
    /// Its index.
    pub index: &'a SourceIndex,
    /// Multiplies the source's fused scores; 1.0 leaves them as Semble ranks them.
    pub boost: f64,
    /// Only files under this folder (relative to the source root) are searched; `None` searches them all.
    pub path_prefix: Option<&'a str>,
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The source's name.
    pub source: String,
    /// The file, relative to its source root.
    pub path: String,
    /// First line, 1-based.
    pub start_line: u32,
    /// Last line, 1-based, inclusive.
    pub end_line: u32,
    /// The chunk's text.
    pub content: String,
    /// The final ranking score.
    pub score: f64,
}

/// A chunk across sources: `(source position, slot)`.
type Key = (usize, u32);

/// Scores in insertion order (Python dict semantics), with lookup by key.
#[derive(Debug, Default)]
struct Scores {
    entries: Vec<(Key, f64)>,
    positions: BTreeMap<Key, usize>,
}

impl Scores {
    fn insert(&mut self, key: Key, score: f64) {
        match self.positions.get(&key) {
            Some(&position) => {
                if let Some(entry) = self.entries.get_mut(position) {
                    entry.1 = score;
                }
            }
            None => {
                self.positions.insert(key, self.entries.len());
                self.entries.push((key, score));
            }
        }
    }

    fn contains(&self, key: Key) -> bool {
        self.positions.contains_key(&key)
    }

    fn max(&self) -> f64 {
        self.entries
            .iter()
            .map(|(_, score)| *score)
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

/// The searched sources.
struct Corpus<'a> {
    sources: &'a [SearchSource<'a>],
}

/// Whether `path` is under `prefix` (`./`, `.` and empty mean the whole source).
fn under_prefix(path: &str, prefix: Option<&str>) -> bool {
    let prefix = prefix
        .map(|prefix| prefix.trim_start_matches("./"))
        .filter(|prefix| !prefix.is_empty() && *prefix != ".");
    prefix.is_none_or(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || prefix.ends_with('/'))
    })
}

impl<'a> Corpus<'a> {
    fn slot(&self, (source, slot): Key) -> Option<&'a Slot> {
        self.sources.get(source)?.index.slot(slot)
    }

    /// Whether `path` of source `source` is under that source's prefix.
    fn allows(&self, source: usize, path: &str) -> bool {
        self.sources
            .get(source)
            .is_some_and(|entry| under_prefix(path, entry.path_prefix))
    }

    /// The text of a chunk.
    fn content(&self, (source, slot): Key) -> io::Result<String> {
        self.sources
            .get(source)
            .map_or_else(|| Ok(String::new()), |entry| entry.index.content(slot))
    }
}

/// Semble `SembleIndex.search` over `sources`: the best `top_k` chunks for `query`, each source limited to its
/// `path_prefix`. An error when an index file cannot be read.
pub fn search(
    embedder: &Embedder,
    sources: &[SearchSource<'_>],
    query: &str,
    top_k: usize,
) -> io::Result<Vec<Hit>> {
    if query.trim().is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }
    let corpus = Corpus { sources };
    let alpha = resolve_alpha(query);
    let candidates = top_k * 5;

    let semantic = semantic_candidates(embedder, &corpus, query, candidates)?;
    let bm25 = bm25_candidates(&corpus, query, candidates)?;
    let semantic_rrf = rrf(&semantic);
    let bm25_rrf = rrf(&bm25);

    // Sorted by start line, as Semble does to counter hash order; then by key for determinism.
    let mut keys: Vec<Key> = semantic_rrf
        .keys()
        .chain(bm25_rrf.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    keys.sort_by_key(|key| (corpus.slot(*key).map_or(0, |slot| slot.start_line), *key));
    let mut scores = Scores::default();
    for key in keys {
        let boost = sources.get(key.0).map_or(1.0, |source| source.boost);
        let score = boost
            * (alpha * semantic_rrf.get(&key).copied().unwrap_or(0.0)
                + (1.0 - alpha) * bm25_rrf.get(&key).copied().unwrap_or(0.0));
        if score != 0.0 {
            scores.insert(key, score);
        }
    }
    if scores.entries.is_empty() {
        return Ok(Vec::new());
    }
    boost_multi_chunk_files(&corpus, &mut scores);
    apply_query_boost(&corpus, &mut scores, query)?;
    let mut hits = Vec::new();
    for (key, score) in rerank_topk(&corpus, &scores, top_k, alpha < 1.0) {
        let (Some(slot), Some(source)) = (corpus.slot(key), sources.get(key.0)) else {
            continue;
        };
        hits.push(Hit {
            source: source.name.to_owned(),
            path: slot.path.to_string(),
            start_line: slot.start_line,
            end_line: slot.end_line,
            content: corpus.content(key)?,
            score,
        });
    }
    Ok(hits)
}

/// The `count` most similar chunks by cosine similarity, best first.
fn semantic_candidates(
    embedder: &Embedder,
    corpus: &Corpus<'_>,
    query: &str,
    count: usize,
) -> io::Result<Vec<(Key, f64)>> {
    let query_vector = embedder.embed(&[query.to_owned()]);
    let mut scored: Vec<(Key, f32)> = Vec::new();
    let mut bytes = Vec::new();
    for (source, entry) in corpus.sources.iter().enumerate() {
        entry.index.scan_vectors(&mut bytes, |slot, row| {
            if entry
                .index
                .slot(slot)
                .is_some_and(|chunk| under_prefix(&chunk.path, entry.path_prefix))
            {
                let score = row.dot(&query_vector);
                scored.push(((source, slot), score));
            }
        })?;
    }
    top_by_score(&mut scored, count);
    Ok(scored
        .into_iter()
        .map(|(key, score)| (key, f64::from(score)))
        .collect())
}

/// The `count` best chunks by BM25, best first, without zero scores. Corpus statistics cover every searched
/// source; the path filter only masks scores, as Semble's selector does.
fn bm25_candidates(corpus: &Corpus<'_>, query: &str, count: usize) -> io::Result<Vec<(Key, f64)>> {
    let tokens = tokenize(query);
    let corpus_size: usize = corpus
        .sources
        .iter()
        .map(|source| source.index.chunk_count())
        .sum();
    if tokens.is_empty() || corpus_size == 0 {
        return Ok(Vec::new());
    }
    let total_length: u64 = corpus
        .sources
        .iter()
        .map(|source| source.index.total_length())
        .sum();
    #[expect(clippy::cast_precision_loss, reason = "corpus sizes are far below 2^52")]
    let (corpus_size, average_length) = (corpus_size as f64, total_length as f64 / corpus_size as f64);
    let mut query_counts: BTreeMap<&str, u32> = BTreeMap::new();
    for token in &tokens {
        *query_counts.entry(token.as_str()).or_insert(0) += 1;
    }
    // Dense per-slot totals, as Semble's score array: a map lookup per posting was a quarter of a warm search.
    let mut totals: Vec<Vec<f64>> = corpus
        .sources
        .iter()
        .map(|source| vec![0.0; source.index.slot_count()])
        .collect();
    let mut touched: Vec<Key> = Vec::new();
    let mut bytes = Vec::new();
    let mut postings: Vec<Vec<(u32, u32)>> = vec![Vec::new(); corpus.sources.len()];
    for (term, query_tf) in query_counts {
        for (entry, list) in corpus.sources.iter().zip(&mut postings) {
            list.clear();
            entry.index.postings(term, &mut bytes, list)?;
        }
        let document_frequency: usize = postings.iter().map(Vec::len).sum();
        if document_frequency == 0 {
            continue;
        }
        #[expect(clippy::cast_precision_loss, reason = "document counts are far below 2^52")]
        let df = document_frequency as f64;
        let idf = (1.0 + (corpus_size - df + 0.5) / (df + 0.5)).ln();
        for (source, ((entry, list), totals)) in
            corpus.sources.iter().zip(&postings).zip(&mut totals).enumerate()
        {
            for &(slot, tf) in list {
                let length = entry.index.slot(slot).map_or(0, |chunk| chunk.length);
                let tf = f64::from(tf);
                let score = f64::from(query_tf) * idf * tf
                    / (K1 * (1.0 - B + B * f64::from(length) / average_length) + tf);
                if let Some(total) = totals.get_mut(slot as usize) {
                    // Every term score is positive, so a zero total is a chunk not seen yet.
                    if *total == 0.0 {
                        touched.push((source, slot));
                    }
                    *total += score;
                }
            }
        }
    }
    let mut scored: Vec<(Key, f32)> = touched
        .into_iter()
        .filter(|key| {
            corpus
                .slot(*key)
                .is_some_and(|slot| corpus.allows(key.0, &slot.path))
        })
        // Semble accumulates into a float32 array.
        .map(|key| {
            let total = totals
                .get(key.0)
                .and_then(|totals| totals.get(key.1 as usize))
                .copied()
                .unwrap_or(0.0);
            (key, total as f32)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();
    top_by_score(&mut scored, count);
    Ok(scored
        .into_iter()
        .map(|(key, score)| (key, f64::from(score)))
        .collect())
}

/// Keeps the `count` highest scores, sorted best first (ties by key).
fn top_by_score(scored: &mut Vec<(Key, f32)>, count: usize) {
    let order = |a: &(Key, f32), b: &(Key, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
    if scored.len() > count && count > 0 {
        scored.select_nth_unstable_by(count - 1, order);
        scored.truncate(count);
    }
    scored.sort_by(order);
}

/// Semble `_rrf_scores`: `1 / (60 + rank)` by descending raw score.
fn rrf(ranked: &[(Key, f64)]) -> BTreeMap<Key, f64> {
    let mut order: Vec<&(Key, f64)> = ranked.iter().collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    #[expect(clippy::cast_precision_loss, reason = "ranks are small")]
    order
        .into_iter()
        .enumerate()
        .map(|(rank, (key, _))| (*key, 1.0 / (RRF_K + (rank + 1) as f64)))
        .collect()
}

/// Semble `boost_multi_chunk_files`: each file's best chunk gains a share of 20% of the top score, by the file's
/// summed score over the best file's.
fn boost_multi_chunk_files(corpus: &Corpus<'_>, scores: &mut Scores) {
    let max_score = scores.max();
    if max_score == 0.0 {
        return;
    }
    let mut file_sum: BTreeMap<(usize, &str), f64> = BTreeMap::new();
    let mut best: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for (position, (key, score)) in scores.entries.iter().enumerate() {
        let Some(slot) = corpus.slot(*key) else { continue };
        let file = (key.0, &*slot.path);
        *file_sum.entry(file).or_insert(0.0) += score;
        let better = best
            .get(&file)
            .and_then(|best| scores.entries.get(*best))
            .is_none_or(|(_, best_score)| score > best_score);
        if better {
            best.insert(file, position);
        }
    }
    let max_file_sum = file_sum.values().copied().fold(f64::NEG_INFINITY, f64::max);
    let unit = max_score * FILE_COHERENCE_BOOST_FRAC;
    for (file, position) in best {
        let sum = file_sum.get(&file).copied().unwrap_or(0.0);
        if let Some(entry) = scores.entries.get_mut(position) {
            entry.1 += unit * sum / max_file_sum;
        }
    }
}

/// Semble `apply_query_boost`: definition boosts for symbol queries; path-keyword and embedded-symbol boosts for
/// natural-language ones.
fn apply_query_boost(corpus: &Corpus<'_>, scores: &mut Scores, query: &str) -> io::Result<()> {
    let max_score = scores.max();
    if is_symbol_query(query) {
        let symbol = extract_symbol_name(query);
        let mut names = BTreeSet::from([symbol.to_owned()]);
        if symbol != query.trim() {
            names.insert(query.trim().to_owned());
        }
        let symbol_lower = symbol.to_lowercase();
        boost_definitions(corpus, scores, &names, max_score * DEFINITION_BOOST, |stem| {
            stem_matches(stem, &symbol_lower)
        })?;
    } else {
        boost_stem_matches(corpus, scores, query, max_score);
        let names = embedded_symbols(query);
        if !names.is_empty() {
            let lower: BTreeSet<String> = names.iter().map(|name| name.to_lowercase()).collect();
            boost_definitions(
                corpus,
                scores,
                &names,
                max_score * DEFINITION_BOOST * EMBEDDED_SYMBOL_SCALE,
                |stem| embedded_stem_ok(stem, &lower),
            )?;
        }
    }
    Ok(())
}

/// Semble `_boost_definitions`: candidates defining one of `names` gain `unit` (×1.5 when the file stem names it
/// too); chunks of files whose stem passes `stem_ok` join the candidates when they define one.
fn boost_definitions(
    corpus: &Corpus<'_>,
    scores: &mut Scores,
    names: &BTreeSet<String>,
    unit: f64,
    stem_ok: impl Fn(&str) -> bool,
) -> io::Result<()> {
    let patterns: Vec<DefinitionPattern> = names
        .iter()
        .filter_map(|name| DefinitionPattern::new(name))
        .collect();
    let names_lower: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
    let tier = |key: Key, slot: &Slot| -> io::Result<f64> {
        let content = corpus.content(key)?;
        if !patterns.iter().any(|pattern| pattern.defined_in(&content)) {
            return Ok(0.0);
        }
        let stem = file_stem(&slot.path).to_lowercase();
        Ok(if names_lower.iter().any(|name| stem_matches(&stem, name)) {
            unit * 1.5
        } else {
            unit
        })
    };
    for entry in &mut scores.entries {
        if let Some(slot) = corpus.slot(entry.0) {
            entry.1 += tier(entry.0, slot)?;
        }
    }
    for (source, entry) in corpus.sources.iter().enumerate() {
        for (path, slots) in entry.index.file_slots() {
            if !under_prefix(path, entry.path_prefix) || !stem_ok(&file_stem(path).to_lowercase()) {
                continue;
            }
            for &slot in slots {
                let key = (source, slot);
                if scores.contains(key) {
                    continue;
                }
                let Some(chunk) = corpus.slot(key) else { continue };
                let boost = tier(key, chunk)?;
                if boost != 0.0 {
                    scores.insert(key, boost);
                }
            }
        }
    }
    Ok(())
}

/// Semble `_boost_stem_matches`: candidates whose file stem or folder matches at least 10% of the query keywords
/// gain the top score times the matched share.
fn boost_stem_matches(corpus: &Corpus<'_>, scores: &mut Scores, query: &str, max_score: f64) {
    let keywords = query_keywords(query);
    if keywords.is_empty() {
        return;
    }
    let boost = max_score * STEM_BOOST;
    let mut parts_cache: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for entry in &mut scores.entries {
        let Some(slot) = corpus.slot(entry.0) else {
            continue;
        };
        let parts = parts_cache
            .entry(&slot.path)
            .or_insert_with(|| path_parts(&slot.path));
        let matches = count_keyword_matches(&keywords, parts);
        #[expect(clippy::cast_precision_loss, reason = "keyword counts are small")]
        let ratio = matches as f64 / keywords.len() as f64;
        if matches > 0 && ratio >= 0.10 {
            entry.1 += boost * ratio;
        }
    }
}

/// Semble `rerank_topk`: path penalties, then a greedy pass halving each further chunk of an already-picked file.
fn rerank_topk(corpus: &Corpus<'_>, scores: &Scores, top_k: usize, penalise_paths: bool) -> Vec<(Key, f64)> {
    let mut ranked: Vec<(Key, f64, &str)> = scores
        .entries
        .iter()
        .filter_map(|(key, score)| {
            let path = &*corpus.slot(*key)?.path;
            let factor = if penalise_paths { path_penalty(path) } else { 1.0 };
            Some((*key, score * factor, path))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut per_file: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    let mut selected: Vec<(f64, Key)> = Vec::new();
    let mut min_selected = f64::INFINITY;
    for (key, score, path) in ranked {
        if selected.len() >= top_k && score <= min_selected {
            break;
        }
        let already = per_file.entry((key.0, path)).or_insert(0);
        let mut effective = score;
        if *already >= SATURATION_THRESHOLD {
            let excess = i32::try_from(*already - SATURATION_THRESHOLD + 1).unwrap_or(i32::MAX);
            effective *= SATURATION_DECAY.powi(excess);
        }
        selected.push((effective, key));
        *already += 1;
        if selected.len() >= top_k {
            min_selected = selected
                .iter()
                .map(|(score, _)| *score)
                .fold(f64::INFINITY, f64::min);
        }
    }
    selected.sort_by(|a, b| b.0.total_cmp(&a.0));
    selected.truncate(top_k);
    selected.into_iter().map(|(score, key)| (key, score)).collect()
}

#[cfg(test)]
mod tests {
    use super::{SearchSource, search};
    use crate::embed::test_embedder;
    use crate::index::{FileStamp, PreparedFile, SourceIndex};

    const WORDS: &[&str] = &["retry", "backoff", "parser", "token"];

    fn index(files: &[(&str, &str)]) -> SourceIndex {
        let embedder = test_embedder(WORDS).unwrap();
        let mut index = SourceIndex::new(WORDS.len());
        for (path, text) in files {
            index.upsert(
                path,
                FileStamp { mtime_ns: 0, size: 0 },
                PreparedFile::new(&embedder, path, text),
            );
        }
        index
    }

    fn ranked(hits: &[super::Hit]) -> Vec<(&str, &str)> {
        hits.iter()
            .map(|hit| (hit.source.as_str(), hit.path.as_str()))
            .collect()
    }

    #[test]
    fn sources_are_searched_together_and_weighted_by_boost() {
        let embedder = test_embedder(WORDS).unwrap();
        let code = index(&[
            ("net/retry.rs", "fn retry_loop() { backoff(); retry(); }\n"),
            ("lex/parser.rs", "fn parse() { token(); }\n"),
        ]);
        let notes = index(&[("retry-notes.md", "retry with backoff, retry again\n")]);
        let query = "how does retry backoff work";
        let even = [
            SearchSource {
                name: "code",
                index: &code,
                boost: 1.0,
                path_prefix: None,
            },
            SearchSource {
                name: "notes",
                index: &notes,
                boost: 1.0,
                path_prefix: None,
            },
        ];
        let hits = search(&embedder, &even, query, 3).unwrap();
        assert_eq!(
            ranked(&hits),
            [
                ("code", "net/retry.rs"),
                ("notes", "retry-notes.md"),
                ("code", "lex/parser.rs")
            ]
        );

        let favour_notes = [
            SearchSource {
                name: "code",
                index: &code,
                boost: 1.0,
                path_prefix: None,
            },
            SearchSource {
                name: "notes",
                index: &notes,
                boost: 4.0,
                path_prefix: None,
            },
        ];
        let hits = search(&embedder, &favour_notes, query, 3).unwrap();
        assert_eq!(ranked(&hits).first(), Some(&("notes", "retry-notes.md")));

        let only_notes = [SearchSource {
            name: "notes",
            index: &notes,
            boost: 1.0,
            path_prefix: None,
        }];
        assert!(
            search(&embedder, &only_notes, query, 3)
                .unwrap()
                .iter()
                .all(|hit| hit.source == "notes")
        );
    }

    #[test]
    fn path_prefix_keeps_only_files_under_it() {
        let embedder = test_embedder(WORDS).unwrap();
        let code = index(&[
            ("net/retry.rs", "fn retry() { backoff(); }\n"),
            ("network/retry.rs", "fn retry() { backoff(); }\n"),
            ("lex/token.rs", "fn token() { retry(); }\n"),
        ]);
        let under = |prefix: Option<&'static str>| {
            search(
                &embedder,
                &[SearchSource {
                    name: "code",
                    index: &code,
                    boost: 1.0,
                    path_prefix: prefix,
                }],
                "retry",
                10,
            )
            .unwrap()
        };
        assert_eq!(ranked(&under(Some("./net"))), [("code", "net/retry.rs")]);
        assert_eq!(under(Some("net/")).len(), 1);
        assert_eq!(under(None).len(), 3);
    }

    #[test]
    fn bm25_adds_each_query_terms_score_to_its_chunk_in_its_source() {
        let code = index(&[
            ("net/retry.rs", "fn retry_loop() { backoff(); retry(); }\n"),
            ("lex/parser.rs", "fn parse() { token(); retry(); }\n"),
        ]);
        let notes = index(&[("retry-notes.md", "backoff before the next attempt\n")]);
        let sources = [
            SearchSource {
                name: "code",
                index: &code,
                boost: 1.0,
                path_prefix: None,
            },
            SearchSource {
                name: "notes",
                index: &notes,
                boost: 1.0,
                path_prefix: None,
            },
        ];
        let corpus = super::Corpus { sources: &sources };
        let scores = |query: &str| super::bm25_candidates(&corpus, query, 10).unwrap();
        let both = scores("retry backoff");
        let mut keys: Vec<_> = both.iter().map(|(key, _)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), both.len(), "{both:?}");
        assert_eq!(
            keys.iter().filter(|(source, _)| *source == 1).count(),
            1,
            "{both:?}"
        );
        let single = [scores("retry"), scores("backoff")];
        for (key, score) in &both {
            let expected: f64 = single
                .iter()
                .flatten()
                .filter(|(other, _)| other == key)
                .map(|(_, score)| score)
                .sum();
            assert!(
                (score - expected).abs() < 1e-5 * expected,
                "{key:?}: {score} vs {expected}"
            );
        }
    }

    #[test]
    fn symbol_query_ranks_the_definition_first() {
        let embedder = test_embedder(WORDS).unwrap();
        let code = index(&[
            (
                "a/uses.rs",
                "fn caller() { Parser::new(); Parser::new(); let p = Parser::default(); }\n",
            ),
            ("b/parser.rs", "pub struct Parser { token: u8 }\n"),
        ]);
        let sources = [SearchSource {
            name: "code",
            index: &code,
            boost: 1.0,
            path_prefix: None,
        }];
        let hits = search(&embedder, &sources, "Parser", 2).unwrap();
        assert_eq!(ranked(&hits).first(), Some(&("code", "b/parser.rs")));
    }
}
