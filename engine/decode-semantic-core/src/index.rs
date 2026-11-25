//! One source's index (Semble `SembleIndex` minus the model): chunks, their normalized embeddings and BM25 postings,
//! updated a file at a time.
//!
//! The bulk lives in a persisted file (a [`Blob`]): embedding rows, the term dictionary and postings are read from a
//! map when it has an OS handle, else with positioned reads; chunk texts always use positioned reads. Embeddings are
//! kept quantized ([`crate::quant`]), in the file and the overlay alike, so a chunk scores the same before and after
//! the index is written. Memory holds paths, lines, BM25 lengths and text locations, every [`SPARSE_EVERY`]th
//! dictionary term, and the chunks added since the file was written (the overlay); chunks of the file replaced or
//! removed since are masked. [`SourceIndex::write_to`] streams the live chunks of both into a new
//! file, which [`SourceIndex::open`] reads back.
//!
//! File layout, every number little-endian: the magic; the chunk texts back to back; one quantized row per chunk; one
//! record per chunk (first line, last line, BM25 length, text length: `u32` each); the postings, `(chunk u32,
//! frequency u32)` pairs, term after term; the dictionary, terms in byte order, each a `u32` length, the bytes, a
//! `u64` postings offset and a `u32` count; a JSON header with the files; then the six section offsets and the
//! magic.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, BufWriter, Read, Write};
use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chunking::{ChunkText, chunk_source};
use crate::embed::Embedder;
use crate::languages::{ExtraGrammar, detect_language, is_code_path};
use crate::mapped_file::MappedFile;
use crate::quant;
use crate::ranking::file_stem;
use crate::tokens::tokenize;

/// Bump when the persisted layout or anything that shapes chunks, tokens or embeddings changes.
const MAGIC: [u8; 8] = *b"rsidx007";
const CHUNK_RECORD: usize = 16;
const POSTING: usize = 8;
/// Bytes of a dictionary term's length.
const TERM_LENGTH: usize = size_of::<u32>();
const FOOTER: usize = 6 * 8 + MAGIC.len();
/// One dictionary entry in this many is kept in memory; a lookup reads the entries between two of them.
const SPARSE_EVERY: usize = 64;
/// Embedding rows per scan block; non-file blobs read one block at a time.
const SCAN_ROWS: u32 = 256;

/// Bytes read at given offsets: a completed persisted index. On-disk owners publish replacements by rename,
/// never overwrite or truncate an inode while an opened index uses it.
pub trait Blob: Send + Sync + fmt::Debug {
    /// Fills `buf` from `offset`; an error if the blob ends first.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    /// Its size in bytes.
    fn size(&self) -> io::Result<u64>;
    /// Its pinned OS handle, when backed by a file rather than an in-memory or simulated blob.
    fn as_file(&self) -> Option<&std::fs::File> {
        None
    }
}

impl Blob for std::fs::File {
    #[cfg(unix)]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let (mut done, mut offset) = (0, offset);
        while let Some(rest) = buf.get_mut(done..).filter(|rest| !rest.is_empty()) {
            let read = std::os::windows::fs::FileExt::seek_read(self, rest, offset)?;
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += read;
            offset += read as u64;
        }
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn as_file(&self) -> Option<&std::fs::File> {
        Some(self)
    }
}

impl Blob for Vec<u8> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let bytes = usize::try_from(offset)
            .ok()
            .and_then(|start| self.get(start..start.checked_add(buf.len())?))
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }
}

/// What identifies a file's version: its modification time and size, as the file listing saw them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// Modification time, nanoseconds since the epoch.
    pub mtime_ns: i64,
    /// Size in bytes.
    pub size: u64,
}

/// A file chunked, tokenized and embedded, ready to be put into an index. Built without touching any index, so it
/// can run in parallel and outside any lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedFile {
    chunks: Vec<ChunkText>,
    terms: Vec<BTreeMap<String, u32>>,
    vectors: Vec<f32>,
}

impl PreparedFile {
    /// Chunks, tokenizes and embeds `text`, the content of `path` (relative, `/`-separated).
    pub fn new(embedder: &Embedder, path: &str, text: &str) -> Self {
        Self::with_grammars(embedder, path, text, |_| None)
    }

    /// As [`PreparedFile::new`], chunking the languages this crate has no grammar for with `extra_grammar`.
    pub fn with_grammars(embedder: &Embedder, path: &str, text: &str, extra_grammar: ExtraGrammar) -> Self {
        let language = detect_language(path);
        let chunks = chunk_source(text, language, extra_grammar);
        let breadcrumbs = if language == Some("markdown") {
            markdown_breadcrumbs(text, &chunks)
        } else {
            Vec::new()
        };
        let terms = chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let mut enriched = enrich_for_bm25(&chunk.content, path);
                if let Some(crumb) = breadcrumbs.get(i).filter(|c| !c.is_empty()) {
                    enriched.push(' ');
                    enriched.push_str(crumb);
                }
                let mut counts = BTreeMap::new();
                for token in tokenize(&enriched) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                counts
            })
            .collect();
        let path_words = (language != Some("markdown")).then(|| path_words(path));
        let contents: Vec<String> = chunks
            .iter()
            .enumerate()
            .map(
                |(i, chunk)| match (breadcrumbs.get(i).filter(|c| !c.is_empty()), &path_words) {
                    (Some(context), _) | (None, Some(context)) => format!("{context}\n{}", chunk.content),
                    (None, None) => chunk.content.clone(),
                },
            )
            .collect();
        let vectors = embedder.embed(&contents);
        Self {
            chunks,
            terms,
            vectors,
        }
    }

    /// How many chunks the file has.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Whether a deserialized entry has one embedding and a valid term map per chunk.
    pub fn fits(&self, dim: usize) -> bool {
        self.chunks.len().checked_mul(dim) == Some(self.vectors.len())
            && self.terms.len() == self.chunks.len()
            && self.terms.iter().all(|terms| {
                terms
                    .values()
                    .copied()
                    .try_fold(0_u32, u32::checked_add)
                    .is_some()
            })
    }
}

/// For each Markdown chunk, the `" > "`-joined chain of active ATX headings strictly above `chunk.start_line`.
/// Skips fenced code blocks so comments inside code snippets are never mistaken for headings.
fn markdown_breadcrumbs(source: &str, chunks: &[ChunkText]) -> Vec<String> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let mut headings: Vec<(u32, u8, &str)> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    for (idx, raw_line) in source.lines().enumerate() {
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let trimmed = raw_line.trim_start_matches(' ');
        let indent = raw_line.len() - trimmed.len();
        if indent <= 3 {
            let first = trimmed.as_bytes().first().copied().unwrap_or(0);
            if first == b'`' || first == b'~' {
                let run = trimmed.bytes().take_while(|&b| b == first).count();
                if run >= 3 {
                    match fence {
                        None => {
                            if first == b'~' || !trimmed[run..].contains('`') {
                                fence = Some((first, run));
                            }
                            continue;
                        }
                        Some((ch, len)) if ch == first && run >= len && trimmed[run..].trim().is_empty() => {
                            fence = None;
                            continue;
                        }
                        _ => {}
                    }
                }
            }
        }
        if fence.is_some() || indent > 3 {
            continue;
        }
        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        if !(1..=6).contains(&hashes) {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            headings.push((line_no, hashes as u8, title));
        }
    }

    let mut stack: Vec<(u8, &str)> = Vec::new();
    let mut next_heading = 0;
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        while let Some(&(ln, level, title)) = headings.get(next_heading) {
            if ln >= chunk.start_line {
                break;
            }
            while stack.last().is_some_and(|&(l, _)| l >= level) {
                stack.pop();
            }
            stack.push((level, title));
            next_heading += 1;
        }
        if stack.is_empty() {
            out.push(String::new());
        } else {
            let mut joined = String::new();
            for (i, &(_, title)) in stack.iter().enumerate() {
                if i > 0 {
                    joined.push_str(" > ");
                }
                joined.push_str(title);
            }
            out.push(joined);
        }
    }
    out
}

/// Beyond Semble: the words of a non-Markdown file's path (`src/auth/token_store.rs` → `src auth token store rs`),
/// embedded ahead of each of its chunks, so a chunk that never names its module still sits near queries about it.
/// Markdown chunks get their heading breadcrumbs instead.
fn path_words(path: &str) -> String {
    path.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Semble `enrich_for_bm25`: the chunk plus its file stem twice and its last three folder names.
fn enrich_for_bm25(content: &str, path: &str) -> String {
    let stem = file_stem(path);
    let folders: Vec<&str> = path
        .rsplit_once('/')
        .map(|(parent, _)| {
            parent
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .collect()
        })
        .unwrap_or_default();
    let last_three = folders
        .get(folders.len().saturating_sub(3)..)
        .unwrap_or_default()
        .join(" ");
    format!("{content} {stem} {stem} {last_three}")
}

/// One indexed chunk; its text and embedding stay in the file or the overlay.
#[derive(Debug, Clone)]
pub(crate) struct Slot {
    pub(crate) path: Arc<str>,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    /// In the file: the text's byte offset. In the overlay: unused (the slot's position there locates it).
    text_at: u64,
    text_len: u32,
}

#[derive(Debug, Clone)]
struct IndexedFile {
    stamp: FileStamp,
    slots: Vec<u32>,
}

/// The persisted file the index was opened from.
#[derive(Debug)]
struct Base {
    blob: Arc<dyn Blob>,
    mapped: Option<MappedFile>,
    /// Slots `0..chunks` are the file's chunks.
    chunks: u32,
    vectors_at: u64,
    postings_at: u64,
    terms_at: u64,
    terms_end: u64,
    /// Every [`SPARSE_EVERY`]th dictionary entry: its term and its offset in the file.
    sparse: Vec<(Box<[u8]>, u64)>,
}

/// The chunks added since the file was written; its chunk `i` is slot `file chunks + i`.
#[derive(Debug, Clone, Default)]
struct Overlay {
    contents: Vec<String>,
    /// One quantized row per chunk.
    vectors: Vec<u8>,
    /// Each chunk's terms, to drop its postings when it is removed.
    terms: Vec<Vec<String>>,
    postings: BTreeMap<String, Vec<(u32, u32)>>,
}

/// A source's chunks, embeddings and BM25 postings. Cloning is cheap: the file and the overlay are shared until
/// changed.
#[derive(Debug, Clone)]
pub struct SourceIndex {
    dim: usize,
    files: BTreeMap<Arc<str>, IndexedFile>,
    code_files: usize,
    slots: Vec<Option<Slot>>,
    /// Each slot's BM25 document length (its token count), dense for the BM25 loop; 0 once removed, which no slot
    /// in a posting list has while live.
    lengths: Vec<u32>,
    base: Option<Arc<Base>>,
    overlay: Arc<Overlay>,
    live: usize,
    total_length: u64,
}

fn corrupt(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("semantic index file: {what}"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

fn to_usize(value: u64) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| corrupt("a section too large"))
}

fn decode_postings(bytes: &[u8]) -> impl Iterator<Item = (u32, u32)> + '_ {
    bytes
        .as_chunks::<POSTING>()
        .0
        .iter()
        .map(|posting| (u32_at(posting, 0), u32_at(posting, 4)))
}

/// Reads `at..end` of a blob in order, a block at a time.
struct Sequential<'a> {
    blob: &'a dyn Blob,
    at: u64,
    end: u64,
    buf: Vec<u8>,
    pos: usize,
}

impl<'a> Sequential<'a> {
    fn new(blob: &'a dyn Blob, at: u64, end: u64) -> Self {
        Self {
            blob,
            at,
            end,
            buf: Vec::new(),
            pos: 0,
        }
    }

    /// Reuses read-ahead for nearby chunks, seeking over removed chunks without reading the gap.
    fn read_exact_at(&mut self, out: &mut [u8], offset: u64) -> io::Result<()> {
        let first = self.at - self.buf.len() as u64;
        if (first..=self.at).contains(&offset) {
            self.pos = to_usize(offset - first)?;
        } else {
            self.at = offset;
            self.buf.clear();
            self.pos = 0;
        }
        self.read_exact(out)
    }
}

impl Read for Sequential<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            let size = self.end.saturating_sub(self.at).min(1 << 16);
            if size == 0 {
                return Ok(0);
            }
            self.buf.resize(to_usize(size)?, 0);
            self.blob.read_exact_at(&mut self.buf, self.at)?;
            self.at += size;
            self.pos = 0;
        }
        let rest = self.buf.get(self.pos..).unwrap_or_default();
        let count = rest.len().min(out.len());
        if let (Some(to), Some(from)) = (out.get_mut(..count), rest.get(..count)) {
            to.copy_from_slice(from);
        }
        self.pos += count;
        Ok(count)
    }
}

/// One dictionary entry: `(term, postings offset, count)`; `None` at the end.
fn read_entry(reader: &mut impl Read) -> io::Result<Option<(Vec<u8>, u64, u32)>> {
    let mut length = [0; TERM_LENGTH];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut term = vec![0; to_usize(u64::from(u32::from_le_bytes(length)))?];
    reader.read_exact(&mut term)?;
    let mut tail = [0; 12];
    reader.read_exact(&mut tail)?;
    Ok(Some((term, u64_at(&tail, 0), u32_at(&tail, 8))))
}

/// A writer counting the bytes through it.
struct Counted<W> {
    inner: W,
    at: u64,
}

impl<W: Write> Counted<W> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.at += bytes.len() as u64;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    dim: usize,
    files: Vec<HeaderFile>,
}

#[derive(Serialize, Deserialize)]
struct HeaderFile {
    path: String,
    stamp: FileStamp,
    chunks: u32,
}

impl SourceIndex {
    /// An empty index for embeddings of `dim` dimensions.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            files: BTreeMap::new(),
            code_files: 0,
            slots: Vec::new(),
            base: None,
            overlay: Arc::default(),
            live: 0,
            lengths: Vec::new(),
            total_length: 0,
        }
    }

    /// The embedding dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// The stamp a file was indexed at, if it is indexed.
    pub fn stamp(&self, path: &str) -> Option<FileStamp> {
        self.files.get(path).map(|file| file.stamp)
    }

    /// The indexed files and their stamps, by path.
    pub fn files(&self) -> impl Iterator<Item = (&str, FileStamp)> {
        self.files.iter().map(|(path, file)| (&**path, file.stamp))
    }

    /// How many files are indexed.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// How many chunks are indexed.
    pub fn chunk_count(&self) -> usize {
        self.live
    }

    /// Whether any indexed file in this source is a code (non-documentation) file.
    pub(crate) fn has_code(&self) -> bool {
        self.code_files > 0
    }

    fn base_chunks(&self) -> u32 {
        self.base.as_ref().map_or(0, |base| base.chunks)
    }

    /// Replaces `path`'s chunks with `prepared`, recorded at `stamp`.
    pub fn upsert(&mut self, path: &str, stamp: FileStamp, prepared: PreparedFile) {
        self.remove(path);
        let path: Arc<str> = Arc::from(path);
        let PreparedFile {
            chunks,
            terms,
            vectors,
        } = prepared;
        let overlay = Arc::make_mut(&mut self.overlay);
        let mut slots = Vec::with_capacity(chunks.len());
        for (index, (chunk, counts)) in chunks.into_iter().zip(terms).enumerate() {
            let slot = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            let mut length = 0;
            let mut names = Vec::with_capacity(counts.len());
            for (term, count) in counts {
                overlay
                    .postings
                    .entry(term.clone())
                    .or_default()
                    .push((slot, count));
                names.push(term);
                length += count;
            }
            let row = overlay.vectors.len();
            quant::quantize(
                vectors
                    .get(index * self.dim..(index + 1) * self.dim)
                    .unwrap_or_default(),
                &mut overlay.vectors,
            );
            overlay.vectors.resize(row + quant::row_bytes(self.dim), 0);
            overlay.terms.push(names);
            let text_len = u32::try_from(chunk.content.len()).unwrap_or(u32::MAX);
            overlay.contents.push(chunk.content);
            self.slots.push(Some(Slot {
                path: Arc::clone(&path),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                text_at: 0,
                text_len,
            }));
            self.lengths.push(length);
            self.live += 1;
            self.total_length += u64::from(length);
            slots.push(slot);
        }
        if is_code_path(&path) {
            self.code_files += 1;
        }
        self.files.insert(path, IndexedFile { stamp, slots });
    }

    /// Drops `path`'s chunks; a no-op when it is not indexed.
    pub fn remove(&mut self, path: &str) {
        let Some(file) = self.files.remove(path) else {
            return;
        };
        if is_code_path(path) {
            self.code_files -= 1;
        }
        let first = self.base_chunks();
        for slot in file.slots {
            if self.slots.get_mut(slot as usize).and_then(Option::take).is_none() {
                continue;
            }
            if slot >= first {
                let overlay = Arc::make_mut(&mut self.overlay);
                let position = (slot - first) as usize;
                for term in overlay
                    .terms
                    .get_mut(position)
                    .map(std::mem::take)
                    .unwrap_or_default()
                {
                    if let Some(list) = overlay.postings.get_mut(&term) {
                        list.retain(|(doc, _)| *doc != slot);
                        if list.is_empty() {
                            overlay.postings.remove(&term);
                        }
                    }
                }
                if let Some(content) = overlay.contents.get_mut(position) {
                    *content = String::new();
                }
            }
            self.live -= 1;
            if let Some(length) = self.lengths.get_mut(slot as usize) {
                self.total_length -= u64::from(std::mem::take(length));
            }
        }
    }

    /// The chunk in `slot`, if it is live.
    pub(crate) fn slot(&self, slot: u32) -> Option<&Slot> {
        self.slots.get(slot as usize).and_then(Option::as_ref)
    }

    /// One more than the highest slot number, live or not.
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// The live slots of each file.
    pub(crate) fn file_slots(&self) -> impl Iterator<Item = (&str, &[u32])> {
        self.files
            .iter()
            .map(|(path, file)| (&**path, file.slots.as_slice()))
    }

    /// The text of the live chunk in `slot`.
    pub(crate) fn content(&self, slot: u32) -> io::Result<String> {
        let Some(entry) = self.slot(slot) else {
            return Ok(String::new());
        };
        let first = self.base_chunks();
        match &self.base {
            Some(base) if slot < first => {
                let mut bytes = vec![0; entry.text_len as usize];
                base.blob.read_exact_at(&mut bytes, entry.text_at)?;
                Ok(String::from_utf8(bytes)
                    .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
            }
            _ => Ok(self
                .overlay
                .contents
                .get((slot - first) as usize)
                .cloned()
                .unwrap_or_default()),
        }
    }

    /// Calls `visit` with each live slot in `slots` and its quantized embedding row, in slot order. `bytes` is working
    /// memory for non-file blobs; mapped rows are borrowed directly.
    pub(crate) fn scan_vectors(
        &self,
        slots: Range<u32>,
        bytes: &mut Vec<u8>,
        mut visit: impl FnMut(u32, &[u8]),
    ) -> io::Result<()> {
        let row_len = quant::row_bytes(self.dim);
        let first = self.base_chunks();
        if let Some(base) = &self.base {
            let (mut start, end) = (slots.start.min(first), slots.end.min(first));
            while start < end {
                let count = (end - start).min(SCAN_ROWS);
                if (start..start + count).any(|slot| self.slot(slot).is_some()) {
                    let len = count as usize * row_len;
                    let at = base.vectors_at + u64::from(start) * row_len as u64;
                    for (slot, row) in (start..).zip(base.bytes(at, len, bytes)?.chunks_exact(row_len)) {
                        if self.slot(slot).is_some() {
                            visit(slot, row);
                        }
                    }
                }
                start += count;
            }
        }
        for slot in slots.start.max(first)..slots.end {
            let at = (slot - first) as usize * row_len;
            match self.overlay.vectors.get(at..at + row_len) {
                Some(row) if self.slot(slot).is_some() => visit(slot, row),
                Some(_) => {}
                None => break,
            }
        }
        Ok(())
    }

    /// Appends the live postings of `term`, `(slot, frequency)`, to `out`; `bytes` is working memory.
    pub(crate) fn postings(
        &self,
        term: &str,
        bytes: &mut Vec<u8>,
        out: &mut Vec<(u32, u32)>,
    ) -> io::Result<()> {
        if let Some(base) = &self.base {
            out.extend(
                decode_postings(base.postings(term.as_bytes(), bytes)?)
                    .filter(|(slot, _)| self.length(*slot) > 0),
            );
        }
        if let Some(list) = self.overlay.postings.get(term) {
            out.extend_from_slice(list);
        }
        Ok(())
    }

    /// The BM25 document length of the live chunk in `slot`; 0 for a removed one.
    pub(crate) fn length(&self, slot: u32) -> u32 {
        self.lengths.get(slot as usize).copied().unwrap_or(0)
    }

    /// Total BM25 document length over live chunks.
    pub(crate) fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Writes the live chunks as an index file for [`Self::open`], streaming: only the dictionary is gathered in
    /// memory.
    pub fn write_to(&self, out: &mut dyn Write) -> io::Result<()> {
        let mut out = Counted {
            inner: BufWriter::with_capacity(1 << 16, out),
            at: 0,
        };
        out.put(&MAGIC)?;
        let order: Vec<u32> = self
            .files
            .values()
            .flat_map(|file| file.slots.iter().copied())
            .filter(|slot| self.slot(*slot).is_some())
            .collect();
        let first = self.base_chunks();
        let base = self.base.as_deref().filter(|_| first > 0);
        let mut input = base.map(|base| {
            (
                base,
                Sequential::new(base.blob.as_ref(), MAGIC.len() as u64, base.postings_at),
            )
        });
        let mut buf = Vec::new();

        let texts_at = out.at;
        for &slot in &order {
            let Some(entry) = self.slot(slot) else { continue };
            match input.as_mut() {
                Some((_, input)) if slot < first => {
                    buf.resize(entry.text_len as usize, 0);
                    input.read_exact_at(&mut buf, entry.text_at)?;
                    out.put(&buf)?;
                }
                _ => out.put(
                    self.overlay
                        .contents
                        .get((slot - first) as usize)
                        .map_or(&[][..], |text| text.as_bytes()),
                )?,
            }
        }

        let vectors_at = out.at;
        let row_bytes = quant::row_bytes(self.dim);
        for &slot in &order {
            match input.as_mut() {
                Some((base, input)) if slot < first => {
                    buf.resize(row_bytes, 0);
                    input.read_exact_at(&mut buf, base.vectors_at + u64::from(slot) * row_bytes as u64)?;
                    out.put(&buf)?;
                }
                _ => {
                    let at = (slot - first) as usize * row_bytes;
                    buf.clear();
                    buf.extend_from_slice(self.overlay.vectors.get(at..at + row_bytes).unwrap_or_default());
                    buf.resize(row_bytes, 0);
                    out.put(&buf)?;
                }
            }
        }

        let chunks_at = out.at;
        let mut renumbered = vec![u32::MAX; self.slots.len()];
        for (new, &slot) in order.iter().enumerate() {
            let Some(entry) = self.slot(slot) else { continue };
            if let Some(number) = renumbered.get_mut(slot as usize) {
                *number = u32::try_from(new).unwrap_or(u32::MAX);
            }
            for value in [
                entry.start_line,
                entry.end_line,
                self.length(slot),
                entry.text_len,
            ] {
                out.put(&value.to_le_bytes())?;
            }
        }
        let renumber = |slot: u32| {
            renumbered
                .get(slot as usize)
                .copied()
                .filter(|new| *new != u32::MAX)
        };

        // The file's terms and the overlay's, merged in byte order.
        let postings_at = out.at;
        let mut dictionary: Vec<(Vec<u8>, u64, u32)> = Vec::new();
        let mut stream = base.map(|base| {
            (
                Sequential::new(base.blob.as_ref(), base.terms_at, base.terms_end),
                Sequential::new(base.blob.as_ref(), base.postings_at, base.terms_at),
            )
        });
        let mut next_base = || -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
            let Some((terms, postings)) = stream.as_mut() else {
                return Ok(None);
            };
            let Some((term, _, count)) = read_entry(terms)? else {
                return Ok(None);
            };
            let mut bytes = vec![0; count as usize * POSTING];
            postings.read_exact(&mut bytes)?;
            Ok(Some((term, bytes)))
        };
        let mut head = next_base()?;
        let mut added = self.overlay.postings.iter().peekable();
        let mut merged = Vec::new();
        loop {
            let (from_base, from_overlay) = match (&head, added.peek()) {
                (None, None) => break,
                (Some(_), None) => (true, false),
                (None, Some(_)) => (false, true),
                (Some((term, _)), Some((other, _))) => match term.as_slice().cmp(other.as_bytes()) {
                    Ordering::Less => (true, false),
                    Ordering::Equal => (true, true),
                    Ordering::Greater => (false, true),
                },
            };
            merged.clear();
            let mut term = Vec::new();
            if from_base && let Some((found, bytes)) = head.take() {
                merged.extend(decode_postings(&bytes).filter_map(|(slot, tf)| Some((renumber(slot)?, tf))));
                term = found;
                head = next_base()?;
            }
            if from_overlay && let Some((found, list)) = added.next() {
                merged.extend(list.iter().filter_map(|(slot, tf)| Some((renumber(*slot)?, *tf))));
                term = found.as_bytes().to_vec();
            }
            if merged.is_empty() {
                continue;
            }
            merged.sort_unstable();
            let offset = out.at - postings_at;
            for (slot, tf) in &merged {
                out.put(&slot.to_le_bytes())?;
                out.put(&tf.to_le_bytes())?;
            }
            dictionary.push((term, offset, u32::try_from(merged.len()).unwrap_or(u32::MAX)));
        }

        let terms_at = out.at;
        for (term, offset, count) in &dictionary {
            let length = u32::try_from(term.len()).map_err(|_| corrupt("a term over 4 GB"))?;
            out.put(&length.to_le_bytes())?;
            out.put(term)?;
            out.put(&offset.to_le_bytes())?;
            out.put(&count.to_le_bytes())?;
        }
        drop(dictionary);

        let header_at = out.at;
        let header = Header {
            dim: self.dim,
            files: self
                .files
                .iter()
                .map(|(path, file)| HeaderFile {
                    path: path.to_string(),
                    stamp: file.stamp,
                    chunks: u32::try_from(file.slots.len()).unwrap_or(u32::MAX),
                })
                .collect(),
        };
        out.put(&serde_json::to_vec(&header).map_err(io::Error::other)?)?;
        for offset in [texts_at, vectors_at, chunks_at, postings_at, terms_at, header_at] {
            out.put(&offset.to_le_bytes())?;
        }
        out.put(&MAGIC)?;
        out.inner.flush()
    }

    /// Opens an index file written by [`Self::write_to`]; an error when it is unreadable, of another layout or of
    /// another dimension.
    pub fn open(blob: Arc<dyn Blob>, dim: usize) -> io::Result<Self> {
        let size = blob.size()?;
        let mut magic = [0; MAGIC.len()];
        blob.read_exact_at(&mut magic, 0)?;
        let footer_at = size
            .checked_sub(FOOTER as u64)
            .filter(|_| magic == MAGIC)
            .ok_or_else(|| corrupt("not an index of this version"))?;
        let mut footer = [0; FOOTER];
        blob.read_exact_at(&mut footer, footer_at)?;
        let [texts_at, vectors_at, chunks_at, postings_at, terms_at, header_at] =
            [0, 1, 2, 3, 4, 5].map(|at| u64_at(&footer, at * 8));
        let ordered = [
            MAGIC.len() as u64,
            texts_at,
            vectors_at,
            chunks_at,
            postings_at,
            terms_at,
            header_at,
            footer_at,
        ]
        .windows(2)
        .all(|pair| pair.first() <= pair.get(1));
        if footer.get(48..) != Some(&MAGIC[..]) || !ordered {
            return Err(corrupt("a broken footer"));
        }
        let mut bytes = vec![0; to_usize(footer_at - header_at)?];
        blob.read_exact_at(&mut bytes, header_at)?;
        let header: Header = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        drop(bytes);
        if header.dim != dim {
            return Err(corrupt("embeddings of another model"));
        }
        let chunks: u64 = header.files.iter().map(|file| u64::from(file.chunks)).sum();
        if postings_at - chunks_at != chunks * CHUNK_RECORD as u64
            || chunks_at - vectors_at != chunks * quant::row_bytes(dim) as u64
        {
            return Err(corrupt("sections of the wrong size"));
        }
        let mut records = vec![0; to_usize(postings_at - chunks_at)?];
        blob.read_exact_at(&mut records, chunks_at)?;
        let mut index = Self::new(dim);
        index.slots.reserve_exact(to_usize(chunks)?);
        index.lengths.reserve_exact(to_usize(chunks)?);
        let mut records = records.as_chunks::<CHUNK_RECORD>().0.iter();
        let mut text_at = texts_at;
        for file in header.files {
            let path: Arc<str> = Arc::from(file.path);
            let mut slots = Vec::with_capacity(file.chunks as usize);
            for _ in 0..file.chunks {
                let record = records.next().ok_or_else(|| corrupt("too few chunks"))?;
                let slot = Slot {
                    path: Arc::clone(&path),
                    start_line: u32_at(record, 0),
                    end_line: u32_at(record, 4),
                    text_at,
                    text_len: u32_at(record, 12),
                };
                text_at += u64::from(slot.text_len);
                index.lengths.push(u32_at(record, 8));
                index.total_length += u64::from(u32_at(record, 8));
                slots.push(u32::try_from(index.slots.len()).unwrap_or(u32::MAX));
                index.slots.push(Some(slot));
            }
            if is_code_path(&path) {
                index.code_files += 1;
            }
            index.files.insert(
                path,
                IndexedFile {
                    stamp: file.stamp,
                    slots,
                },
            );
        }
        if text_at != vectors_at {
            return Err(corrupt("texts of the wrong size"));
        }
        index.live = index.slots.len();
        let mut sparse = Vec::new();
        let mut reader = Sequential::new(blob.as_ref(), terms_at, header_at);
        let mut at = terms_at;
        let mut count = 0_usize;
        while let Some((term, _, _)) = read_entry(&mut reader)? {
            let next = at + (TERM_LENGTH + term.len() + 12) as u64;
            if count.is_multiple_of(SPARSE_EVERY) {
                sparse.push((term.into_boxed_slice(), at));
            }
            at = next;
            count += 1;
        }
        index.base = Some(Arc::new(Base {
            mapped: blob
                .as_file()
                .map(|file| MappedFile::open(file, size))
                .transpose()?,
            blob,
            chunks: u32::try_from(index.slots.len()).map_err(|_| corrupt("too many chunks"))?,
            vectors_at,
            postings_at,
            terms_at,
            terms_end: header_at,
            sparse,
        }));
        Ok(index)
    }
}

impl Base {
    /// The `len` bytes at `at`: borrowed from the map when the file has one, else read into `buf`.
    fn bytes<'a>(&'a self, at: u64, len: usize, buf: &'a mut Vec<u8>) -> io::Result<&'a [u8]> {
        match &self.mapped {
            Some(mapped) => {
                let start = to_usize(at)?;
                start
                    .checked_add(len)
                    .and_then(|end| mapped.bytes().get(start..end))
                    .ok_or_else(|| corrupt("a section past its end"))
            }
            None => {
                buf.resize(len, 0);
                self.blob.read_exact_at(buf, at)?;
                Ok(buf.as_slice())
            }
        }
    }

    /// The encoded postings of `term`, empty when no chunk has it; `buf` is working memory.
    fn postings<'a>(&'a self, term: &[u8], buf: &'a mut Vec<u8>) -> io::Result<&'a [u8]> {
        let after = self
            .sparse
            .partition_point(|(sampled, _)| sampled.as_ref() <= term);
        let Some(start) = after.checked_sub(1).and_then(|at| self.sparse.get(at)) else {
            return Ok(&[]);
        };
        let end = self.sparse.get(after).map_or(self.terms_end, |(_, at)| *at);
        let mut block = Vec::new();
        let mut reader = self.bytes(start.1, to_usize(end - start.1)?, &mut block)?;
        while let Some((found, offset, count)) = read_entry(&mut reader)? {
            match found.as_slice().cmp(term) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return self.bytes(self.postings_at + offset, count as usize * POSTING, buf);
                }
                Ordering::Greater => break,
            }
        }
        Ok(&[])
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Blob, FileStamp, PreparedFile, SourceIndex, enrich_for_bm25, markdown_breadcrumbs, path_words,
    };
    use crate::ChunkText;
    use crate::embed::test_embedder;

    const WORDS: &[&str] = &["alpha", "beta", "gamma", "delta"];

    fn stamp(size: u64) -> FileStamp {
        FileStamp { mtime_ns: 1, size }
    }

    fn add(index: &mut SourceIndex, path: &str, text: &str) {
        let prepared = PreparedFile::new(&test_embedder(WORDS).unwrap(), path, text);
        index.upsert(path, stamp(text.len() as u64), prepared);
    }

    fn bytes(index: &SourceIndex) -> Vec<u8> {
        let mut bytes = Vec::new();
        index.write_to(&mut bytes).unwrap();
        bytes
    }

    fn reopen(index: &SourceIndex) -> SourceIndex {
        SourceIndex::open(Arc::new(bytes(index)), WORDS.len()).unwrap()
    }

    fn postings(index: &SourceIndex, term: &str) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        index.postings(term, &mut Vec::new(), &mut out).unwrap();
        out
    }

    fn rows(index: &SourceIndex) -> Vec<(u32, Vec<u8>)> {
        let mut rows = Vec::new();
        index
            .scan_vectors(0..u32::MAX, &mut Vec::new(), |slot, row| {
                rows.push((slot, row.to_vec()));
            })
            .unwrap();
        rows
    }

    #[derive(Debug)]
    struct CountedBlob<T> {
        inner: T,
        reads: AtomicUsize,
    }

    impl<T: Blob> Blob for CountedBlob<T> {
        fn read_exact_at(&self, out: &mut [u8], offset: u64) -> io::Result<()> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.inner.read_exact_at(out, offset)
        }

        fn size(&self) -> io::Result<u64> {
            self.inner.size()
        }

        fn as_file(&self) -> Option<&File> {
            self.inner.as_file()
        }
    }

    #[test]
    fn mapped_rows_match_the_overlay_rows_they_were_written_from_with_tombstones() {
        let mut original = SourceIndex::new(WORDS.len());
        add(&mut original, "a.md", "alpha delta");
        add(&mut original, "b.md", "beta");
        let written = bytes(&original);
        let path = std::env::temp_dir().join(format!("decode_semantic_mapped_{}", std::process::id()));
        std::fs::write(&path, &written).unwrap();
        let blob = Arc::new(CountedBlob {
            inner: File::open(&path).unwrap(),
            reads: AtomicUsize::new(0),
        });
        let mut mapped = SourceIndex::open(blob.clone(), WORDS.len()).unwrap();
        std::fs::remove_file(&path).unwrap();
        blob.reads.store(0, Ordering::Relaxed);
        assert_eq!(rows(&mapped), rows(&original));
        mapped.remove("a.md");
        original.remove("a.md");
        add(&mut mapped, "c.md", "gamma delta");
        add(&mut original, "c.md", "gamma delta");
        assert_eq!(rows(&mapped), rows(&original));
        assert_eq!(
            blob.reads.load(Ordering::Relaxed),
            0,
            "mapped scans must not copy vector blocks through Blob"
        );
    }

    #[test]
    fn buffered_chunk_reads_seek_across_gaps_and_back_into_read_ahead() {
        let data: Vec<u8> = (0..200_000).map(|at| (at * 37 % 251) as u8).collect();
        let mut reader = super::Sequential::new(&data, 0, data.len() as u64);
        for (at, count) in [
            (53, 19),
            (65_000, 300),
            (65_500, 200),
            (65_600, 7),
            (180_000, 64),
            (179_980, 40),
        ] {
            let mut out = vec![0; count];
            reader.read_exact_at(&mut out, at as u64).unwrap();
            assert_eq!(out, data[at..at + count]);
        }
        assert_eq!(
            reader.read_exact_at(&mut [0; 15], 199_990).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn persistence_coalesces_small_base_reads_without_changing_the_written_index() {
        let chunks: Vec<_> = (0..4096)
            .map(|line| ChunkText {
                content: format!("{line:04} {}\n", "alpha beta ".repeat(11)),
                start_line: line + 1,
                end_line: line + 1,
            })
            .collect();
        let mut index = SourceIndex::new(WORDS.len());
        let count = chunks.len();
        index.upsert(
            "many.md",
            stamp(1),
            PreparedFile {
                chunks,
                terms: vec![Default::default(); count],
                vectors: vec![0.25; count * WORDS.len()],
            },
        );
        let expected = bytes(&index);
        let bound = expected.len().div_ceil(1 << 16) + 2;
        let blob = Arc::new(CountedBlob {
            inner: expected.clone(),
            reads: AtomicUsize::new(0),
        });
        let restored = SourceIndex::open(blob.clone(), WORDS.len()).unwrap();
        blob.reads.store(0, Ordering::Relaxed);
        assert_eq!(bytes(&restored), expected);
        assert!(
            blob.reads.load(Ordering::Relaxed) <= bound,
            "persistence must read blocks, not one chunk/row per syscall"
        );
    }

    /// Changes on top of an opened file (replacing and removing its files, adding and dropping new ones) leave
    /// nothing of the old chunks in postings, vectors or statistics, and write the same file as a fresh build.
    #[test]
    fn incremental_updates_match_a_fresh_build() {
        let mut updated = SourceIndex::new(WORDS.len());
        add(&mut updated, "a.py", "def alpha(): beta()\n");
        add(&mut updated, "b.py", "def gamma(): gamma()\n");
        add(&mut updated, "c.py", "def beta(): beta()\n");
        let mut updated = reopen(&updated);
        add(&mut updated, "a.py", "def delta(): alpha()\n");
        add(&mut updated, "d.py", "def gamma(): delta()\n");
        updated.remove("b.py");
        updated.remove("d.py");
        let mut fresh = SourceIndex::new(WORDS.len());
        add(&mut fresh, "a.py", "def delta(): alpha()\n");
        add(&mut fresh, "c.py", "def beta(): beta()\n");

        assert_eq!((updated.file_count(), updated.chunk_count()), (2, 2));
        assert_eq!(updated.total_length(), fresh.total_length());
        assert!(postings(&updated, "gamma").is_empty());
        assert_eq!(postings(&updated, "beta").len(), 1);
        assert_eq!(postings(&updated, "delta").len(), 1);
        assert_eq!(rows(&updated).len(), 2);
        assert_eq!(bytes(&updated), bytes(&fresh));
        assert_eq!(bytes(&reopen(&updated)), bytes(&fresh));
    }

    #[test]
    fn a_written_index_opens_as_the_same_index() {
        let mut index = SourceIndex::new(WORDS.len());
        add(&mut index, "src/a.py", "def alpha(): beta()\n");
        add(&mut index, "src/b.rs", "fn gamma() { delta() }\n");
        let restored = reopen(&index);
        assert_eq!(bytes(&restored), bytes(&index));
        assert_eq!(restored.stamp("src/b.rs"), index.stamp("src/b.rs"));
        assert_eq!(postings(&restored, "gamma"), postings(&index, "gamma"));
        assert_eq!(rows(&restored), rows(&index));
        assert_eq!(restored.total_length(), index.total_length());
        assert_eq!(restored.content(1).unwrap(), index.content(1).unwrap());
        let written = bytes(&index);
        assert!(SourceIndex::open(Arc::new(written.clone()), WORDS.len() + 1).is_err());
        assert!(SourceIndex::open(Arc::new(written[..written.len() - 1].to_vec()), WORDS.len()).is_err());
    }

    #[test]
    fn a_corrupt_offset_past_the_address_space_is_an_error() {
        let path =
            std::env::temp_dir().join(format!("decode_semantic_offset_overflow_{}", std::process::id()));
        let mut index = SourceIndex::new(WORDS.len());
        add(&mut index, "a.md", "retry parser");
        std::fs::write(&path, bytes(&index)).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let reopened = SourceIndex::open(Arc::new(file), WORDS.len()).unwrap();
        let base = reopened.base.as_ref().unwrap();
        assert!(base.mapped.is_some());
        assert!(base.bytes(u64::MAX - 3, 8, &mut Vec::new()).is_err());
    }

    #[test]
    fn a_term_over_64_kb_persists_and_reopens() {
        // A pasted hash or blob is one identifier; Semble's BM25 keeps it as a term.
        let long = "f".repeat(70_000);
        let mut index = SourceIndex::new(WORDS.len());
        add(&mut index, "blob.md", &format!("alpha {long} beta\n"));
        let restored = reopen(&index);
        assert_eq!(postings(&restored, &long), vec![(0, 1)]);
        assert_eq!(postings(&restored, "beta"), postings(&index, "beta"));
    }

    #[test]
    fn a_reopened_dictionary_finds_terms_past_its_first_sparse_block() {
        let words: Vec<String> = (0..super::SPARSE_EVERY * 3)
            .map(|at| format!("w{at:03}"))
            .collect();
        let mut index = SourceIndex::new(WORDS.len());
        add(&mut index, "many.md", &words.join(" "));
        let restored = reopen(&index);
        for word in &words {
            assert_eq!(postings(&restored, word), vec![(0, 1)], "{word}");
        }
    }

    #[test]
    fn enriches_with_stem_twice_and_last_three_folders() {
        assert_eq!(enrich_for_bm25("x", "a/b/c/d/file.rs"), "x file file b c d");
        assert_eq!(enrich_for_bm25("x", "file.d.ts"), "x file.d file.d ");
    }

    #[test]
    fn counts_code_files_through_upsert_remove_and_reopen() {
        let mut index = SourceIndex::new(WORDS.len());
        add(&mut index, "notes.md", "alpha\n");
        assert!(!index.has_code());
        add(&mut index, "a.py", "alpha\n");
        add(&mut index, "a.py", "beta\n");
        add(&mut index, "b.py", "beta\n");
        index.remove("a.py");
        index.remove("missing.py");
        assert!(index.has_code());
        assert!(
            SourceIndex::open(Arc::new(bytes(&index)), WORDS.len())
                .unwrap()
                .has_code()
        );
        index.remove("b.py");
        assert!(!index.has_code());
    }

    #[test]
    fn code_chunks_embed_their_path_words_and_markdown_chunks_do_not() {
        let embedder = test_embedder(WORDS).unwrap();
        let vectors = |path: &str| PreparedFile::new(&embedder, path, "alpha\n").vectors;
        assert_eq!(path_words("src/beta_gamma/delta.py"), "src beta gamma delta py");
        assert_ne!(vectors("beta/gamma.py"), vectors("delta.py"));
        assert_eq!(vectors("beta/gamma.md"), vectors("delta.md"));
    }

    #[test]
    fn markdown_breadcrumbs_follow_the_heading_chain_and_skip_fenced_code() {
        let source = "# Guide\n## Setup\ntext\n```sh\n# not a heading\n```\n### Linux\nmore\n## Usage\nend\n";
        let chunk = |line: u32| ChunkText {
            content: String::new(),
            start_line: line,
            end_line: line,
        };
        assert_eq!(
            markdown_breadcrumbs(source, &[chunk(1), chunk(3), chunk(8), chunk(10)]),
            ["", "Guide > Setup", "Guide > Setup > Linux", "Guide > Usage"]
        );
    }
}
