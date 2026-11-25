//! Semble `search.py` plus the score bookkeeping of `ranking/`: semantic and BM25 candidates fused by reciprocal
//! rank, file-coherence and definition / path-keyword boosts, then path penalties and per-file saturation. Several
//! sources can be searched together: their BM25 statistics are pooled, as Semble's `SembleIndex.merge` does, and
//! each source's fused scores are scaled by its boost.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::Range;

use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::embed::Embedder;
use crate::index::{Slot, SourceIndex};
use crate::languages::is_doc_path;
use crate::quant;
use crate::ranking::{
    DefinitionPattern, count_keyword_matches, embedded_stem_ok, embedded_symbols, extract_symbol_name,
    file_stem, is_symbol_query, path_parts, path_penalty, query_keywords, resolve_alpha, stem_matches,
};
use crate::tokens::tokenize;
use crate::top::TopScores;

/// Semble `_RRF_K` is 60; 40 lets the top few ranks of either list count for more.
const RRF_K: f64 = 40.0;
/// BM25 term-frequency saturation (`_K1`).
const K1: f64 = 1.5;
/// BM25 document-length normalization (`_B`).
const B: f64 = 0.75;
/// Semble `_FILE_COHERENCE_BOOST_FRAC`.
const FILE_COHERENCE_BOOST_FRAC: f64 = 0.2;
/// Semble `_DEFINITION_BOOST_MULTIPLIER` is 3; 2 keeps a definition first without burying the chunks that use it.
const DEFINITION_BOOST: f64 = 2.0;
/// Beyond Semble, BM25 offers three times the semantic candidates: exact-term chunks it ranks past the first pool
/// still reach the boosts, and RRF keeps them below its top ranks.
const BM25_POOL_FACTOR: usize = 3;
/// Semble `_EMBEDDED_SYMBOL_BOOST_SCALE`.
const EMBEDDED_SYMBOL_SCALE: f64 = 0.5;
/// Semble `_STEM_BOOST_MULTIPLIER`.
const STEM_BOOST: f64 = 1.0;
/// Semble `_FILE_SATURATION_THRESHOLD`.
const SATURATION_THRESHOLD: usize = 1;
/// Semble `_FILE_SATURATION_DECAY`.
const SATURATION_DECAY: f64 = 0.5;
/// Documentation chunks weigh this much in a search that also covers code, where the question is usually about code.
const DOC_IN_CODE_PENALTY: f64 = 0.8;
/// Per-file saturation without code: a notes file holds many separate entries, so each further section of it is
/// discounted less than a code file's further chunks.
const NOTES_SATURATION_DECAY: f64 = 0.75;
/// Semantic weight of natural-language queries without code.
const NOTES_ALPHA_NL: f64 = 0.3;

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

    /// Whether any source holds a code file: without one, notes are ranked as notes rather than as code docs.
    fn has_code(&self) -> bool {
        self.sources.iter().any(|source| source.index.has_code())
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
    let alpha = match resolve_alpha(query) {
        // Notes repeat their headings' words, so the exact terms in BM25 pin the right section.
        _ if !corpus.has_code() && !is_symbol_query(query) => NOTES_ALPHA_NL,
        alpha => alpha,
    };
    let candidates = top_k * 5;

    // BM25 runs on one thread while the pool's others scan the vectors.
    let (semantic, bm25) = rayon::join(
        || semantic_candidates(embedder, &corpus, query, candidates),
        || bm25_candidates(&corpus, query, candidates * BM25_POOL_FACTOR),
    );
    let (semantic, bm25) = (semantic?, bm25?);
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

/// The `count` most similar chunks by cosine similarity of the quantized query and rows ([`quant::dot`]), best
/// first. Each pool thread scans one contiguous run of a source's rows.
fn semantic_candidates(
    embedder: &Embedder,
    corpus: &Corpus<'_>,
    query: &str,
    count: usize,
) -> io::Result<Vec<(Key, f64)>> {
    let (scale, values) = quant::quantize_query(&embedder.embed(&[query.to_owned()]));
    let parts = rayon::current_num_threads() as u32;
    let runs: Vec<(usize, &SearchSource<'_>, Range<u32>)> = corpus
        .sources
        .iter()
        .enumerate()
        .flat_map(|(source, entry)| {
            let slots = u32::try_from(entry.index.slot_count()).unwrap_or(u32::MAX);
            let run = slots.div_ceil(parts).max(1);
            (0..slots)
                .step_by(run as usize)
                .map(move |start| (source, entry, start..slots.min(start + run)))
        })
        .collect();
    let scanned = runs
        .into_par_iter()
        .map(|(source, entry, slots)| {
            let mut best = TopScores::new(count);
            entry.index.scan_vectors(slots, &mut Vec::new(), |slot, row| {
                if entry
                    .index
                    .slot(slot)
                    .is_some_and(|chunk| under_prefix(&chunk.path, entry.path_prefix))
                {
                    best.push(((source, slot), quant::dot(scale, &values, row)));
                }
            })?;
            Ok(best.finish())
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut best = TopScores::new(count);
    for candidate in scanned.into_iter().flatten() {
        best.push(candidate);
    }
    Ok(best
        .finish()
        .into_iter()
        .map(|(key, score)| (key, f64::from(score)))
        .collect())
}

thread_local! {
    /// BM25 totals of every searched slot, all zero between searches: a fresh array per search was a page fault per
    /// 4 KB of slots before the first posting was scored.
    static TOTALS: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
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
    let starts: Vec<usize> = corpus
        .sources
        .iter()
        .scan(0, |next, source| {
            let start = *next;
            *next += source.index.slot_count();
            Some(start)
        })
        .collect();
    let slots = starts
        .last()
        .zip(corpus.sources.last())
        .map_or(0, |(start, source)| start + source.index.slot_count());
    TOTALS.with_borrow_mut(|totals| {
        if totals.len() < slots {
            totals.resize(slots, 0.0);
        }
        let mut touched: Vec<(Key, usize)> = Vec::new();
        let scored = score_bm25(
            corpus,
            query_counts,
            corpus_size,
            average_length,
            &starts,
            totals,
            &mut touched,
        );
        let mut best = TopScores::new(count);
        for ((source, slot), at) in touched {
            let total = totals.get_mut(at).map_or(0.0, std::mem::take);
            // Semble accumulates into a float32 array.
            #[expect(clippy::cast_possible_truncation, reason = "Semble's score array is float32")]
            let total = total as f32;
            if total > 0.0
                && corpus
                    .slot((source, slot))
                    .is_some_and(|chunk| corpus.allows(source, &chunk.path))
            {
                best.push(((source, slot), total));
            }
        }
        scored?;
        Ok(best
            .finish()
            .into_iter()
            .map(|(key, score)| (key, f64::from(score)))
            .collect())
    })
}

/// Adds each query term's BM25 score to `totals` (offset by the source's start) and lists every slot it first
/// makes nonzero in `touched` with its position there, also when a posting read fails partway.
fn score_bm25(
    corpus: &Corpus<'_>,
    query_counts: BTreeMap<&str, u32>,
    corpus_size: f64,
    average_length: f64,
    starts: &[usize],
    totals: &mut [f64],
    touched: &mut Vec<(Key, usize)>,
) -> io::Result<()> {
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
        for (source, ((entry, list), start)) in corpus.sources.iter().zip(&postings).zip(starts).enumerate() {
            for &(slot, tf) in list {
                let length = entry.index.length(slot);
                let tf = f64::from(tf);
                let score = f64::from(query_tf) * idf * tf
                    / (K1 * (1.0 - B + B * f64::from(length) / average_length) + tf);
                let at = start + slot as usize;
                if let Some(total) = totals.get_mut(at) {
                    // Every term score is positive, so a zero total is a chunk not seen yet.
                    if *total == 0.0 {
                        touched.push(((source, slot), at));
                    }
                    *total += score;
                }
            }
        }
    }
    Ok(())
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
    let patterns: Vec<DefinitionPattern> = names.iter().map(|name| DefinitionPattern::new(name)).collect();
    let names_lower: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
    // A definition of a plain identifier holds it as a whole BM25 token, so only chunks in its postings are read.
    let plain = names_lower
        .iter()
        .all(|name| tokenize(name).first().is_some_and(|token| token == name));
    let holders: Option<BTreeSet<Key>> = if plain {
        let (mut bytes, mut found, mut keys) = (Vec::new(), Vec::new(), BTreeSet::new());
        for (source, entry) in corpus.sources.iter().enumerate() {
            for name in &names_lower {
                found.clear();
                entry.index.postings(name, &mut bytes, &mut found)?;
                keys.extend(found.iter().map(|&(slot, _)| (source, slot)));
            }
        }
        Some(keys)
    } else {
        None
    };
    let tier = |key: Key, slot: &Slot| -> io::Result<f64> {
        if is_doc_path(&slot.path) || holders.as_ref().is_some_and(|keys| !keys.contains(&key)) {
            return Ok(0.0);
        }
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
            if !under_prefix(path, entry.path_prefix)
                || !stem_ok(&file_stem(path).to_lowercase())
                || is_doc_path(path)
            {
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

/// Semble `rerank_topk`: path penalties, then a greedy pass discounting each further chunk of an already-picked file.
/// Beyond Semble, docs beside code weigh [`DOC_IN_CODE_PENALTY`], and a corpus without code keeps docs whole and
/// discounts further sections less.
fn rerank_topk(corpus: &Corpus<'_>, scores: &Scores, top_k: usize, penalise_paths: bool) -> Vec<(Key, f64)> {
    let corpus_has_code = corpus.has_code();
    let decay = if corpus_has_code {
        SATURATION_DECAY
    } else {
        NOTES_SATURATION_DECAY
    };
    let mut ranked: Vec<(Key, f64, &str)> = scores
        .entries
        .iter()
        .filter_map(|(key, score)| {
            let path = &*corpus.slot(*key)?.path;
            let factor = if !penalise_paths {
                1.0
            } else if is_doc_path(path) {
                if corpus_has_code {
                    path_penalty(path) * DOC_IN_CODE_PENALTY
                } else {
                    1.0
                }
            } else {
                path_penalty(path)
            };
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
            effective *= decay.powi(excess);
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
    use std::collections::BTreeSet;

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
    fn parallel_semantic_runs_match_one_full_sort_across_file_overlay_and_removed_slots() {
        fn pairs(list: &[(String, String)]) -> Vec<(&str, &str)> {
            list.iter()
                .map(|(path, text)| (path.as_str(), text.as_str()))
                .collect()
        }
        let embedder = test_embedder(WORDS).unwrap();
        let texts: Vec<(String, String)> = (0..41)
            .map(|at| {
                let words = [WORDS[at % 4], WORDS[at * 7 % 4]];
                (format!("f{at:02}.md"), words.join(" "))
            })
            .collect();
        let (written, added) = texts.split_at(33);
        let mut bytes = Vec::new();
        index(&pairs(written)).write_to(&mut bytes).unwrap();
        let mut source = SourceIndex::open(std::sync::Arc::new(bytes), WORDS.len()).unwrap();
        source.remove("f03.md");
        for (path, text) in added {
            source.upsert(
                path,
                FileStamp { mtime_ns: 0, size: 0 },
                PreparedFile::new(&embedder, path, text),
            );
        }
        source.remove("f35.md");
        let query = "retry parser";
        let (scale, values) = crate::quant::quantize_query(&embedder.embed(&[query.to_owned()]));
        let mut expected = Vec::new();
        source
            .scan_vectors(0..u32::MAX, &mut Vec::new(), |slot, row| {
                expected.push(((0, slot), crate::quant::dot(scale, &values, row)));
            })
            .unwrap();
        expected.sort_by(crate::score_order);
        let sources = [SearchSource {
            name: "code",
            index: &source,
            boost: 1.0,
            path_prefix: None,
        }];
        let corpus = super::Corpus { sources: &sources };
        for threads in [1, 3, 7] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for count in [1, 5, 50] {
                let found =
                    pool.install(|| super::semantic_candidates(&embedder, &corpus, query, count).unwrap());
                let wanted: Vec<_> = expected
                    .iter()
                    .take(count)
                    .map(|(key, score)| (*key, f64::from(*score)))
                    .collect();
                assert_eq!(found, wanted, "{threads} threads, top {count}");
            }
        }
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

    #[test]
    fn bm25_offers_definitions_past_the_semantic_pool() {
        let embedder = test_embedder(WORDS).unwrap();
        let mut files: Vec<(String, String)> = (0..10)
            .map(|n| {
                (
                    format!("calls/c{n}.py"),
                    "x = parse_token(retry, backoff, retry, backoff)\n".to_owned(),
                )
            })
            .collect();
        files.push((
            "lib/util.py".to_owned(),
            "def parse_token(text):\n    return text\n".to_owned(),
        ));
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let code = index(&files);
        let sources = [SearchSource {
            name: "code",
            index: &code,
            boost: 1.0,
            path_prefix: None,
        }];
        // One hit: 5 semantic candidates and 15 from BM25, which ranks the definition below every caller.
        let hits = search(&embedder, &sources, "how does parse_token retry with backoff", 1).unwrap();
        assert_eq!(ranked(&hits), [("code", "lib/util.py")]);
    }

    fn rerank(source: &SourceIndex, entries: &[(u32, f64)]) -> Vec<u32> {
        let sources = [SearchSource {
            name: "s",
            index: source,
            boost: 1.0,
            path_prefix: None,
        }];
        let corpus = super::Corpus { sources: &sources };
        let mut scores = super::Scores::default();
        for &(slot, score) in entries {
            scores.insert((0, slot), score);
        }
        super::rerank_topk(&corpus, &scores, 10, true)
            .into_iter()
            .map(|((_, slot), _)| slot)
            .collect()
    }

    #[test]
    fn docs_rank_whole_alone_and_below_code_beside_it() {
        let section = |title: &str| format!("# {title}\n\n{}\n", "retry backoff ".repeat(40));
        let notes = index(&[
            ("a.md", &format!("{}{}", section("One"), section("Two"))),
            ("c.md", "token\n"),
        ]);
        assert_eq!(
            notes
                .file_slots()
                .find(|(path, _)| *path == "a.md")
                .map(|(_, slots)| slots.len()),
            Some(2)
        );
        // Without code, a.md's second section (0.9 x 0.75) stays above c.md; Semble's halving would drop it below.
        assert_eq!(rerank(&notes, &[(0, 1.0), (1, 0.9), (2, 0.6)]), [0, 1, 2]);
        let mixed = index(&[("a.md", "retry\n"), ("b.rs", "fn retry() {}\n")]);
        assert_eq!(rerank(&mixed, &[(0, 1.0), (1, 0.9)]), [1, 0]);
    }

    #[test]
    fn definitions_in_docs_are_not_boosted() {
        let code = index(&[
            ("docs/parser.md", "pub struct Parser { token: u8 }\n"),
            ("src/parser.rs", "pub struct Parser { token: u8 }\n"),
        ]);
        let sources = [SearchSource {
            name: "code",
            index: &code,
            boost: 1.0,
            path_prefix: None,
        }];
        let corpus = super::Corpus { sources: &sources };
        let mut scores = super::Scores::default();
        scores.insert((0, 0), 1.0);
        super::boost_definitions(
            &corpus,
            &mut scores,
            &BTreeSet::from(["Parser".to_owned()]),
            3.0,
            |_| true,
        )
        .unwrap();
        assert_eq!(scores.entries, [((0, 0), 1.0), ((0, 1), 4.5)]);
    }
}
