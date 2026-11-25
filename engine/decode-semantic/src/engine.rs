//! One persisted index per folder, kept in step with its files before each search.
//!
//! The index of a folder is the file `<index dir>/<model revision>/<SHA-256 of the folder's path>.bin`, the name and
//! format Decode gives it, so Decode and Decode Semantic pointed at the same index folder keep one index per folder.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use decode_semantic_core::{
    Embedder, Exclusions, FileStamp, PreparedFile, SearchSource, SourceIndex, changes, search,
};
use sha2::{Digest, Sha256};

use crate::files::Ignores;
use crate::model::MODEL_REVISION;
use decode_semantic_core::Watch;

/// One search result, with an absolute path.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The position of the source it is from.
    pub source: usize,
    /// Absolute file path.
    pub file: String,
    /// First line, 1-based.
    pub start_line: u32,
    /// Last line, 1-based, inclusive.
    pub end_line: u32,
    /// Fused ranking score.
    pub score: f64,
    /// The chunk's text.
    pub content: String,
}

/// A folder searched.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// The folder.
    pub root: PathBuf,
    /// Its results' scores are multiplied by this.
    pub boost: f64,
    /// Which ignore files its listing applies.
    pub ignores: Ignores,
}

/// Progress of the work a search does before it can answer: a stage name and how far it is.
pub type Progress<'a> = &'a dyn Fn(&str, usize, usize);

/// `path` made absolute with `.` and `..` resolved by name, symbolic links kept: the form its index is named by.
pub fn normalize(path: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// The file holding the index of `root` (normalized) in `cache_dir`: named by the SHA-256 of its path, as Decode
/// names it.
pub fn cache_file(cache_dir: &Path, root: &Path) -> PathBuf {
    let digest: String = Sha256::digest(root.to_string_lossy().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cache_dir.join(format!("{digest}.bin"))
}

/// The embedding model and the indexes of the folders searched so far.
#[derive(Debug)]
pub struct Engine {
    cache_dir: PathBuf,
    embedder: Embedder,
    indexes: Mutex<BTreeMap<PathBuf, RootIndex>>,
}

#[derive(Debug)]
struct RootIndex {
    index: SourceIndex,
    /// Only valid for the listing that produced this index, not another ignore or language scope.
    watch: Option<(Ignores, bool, Watch)>,
}

impl Engine {
    /// An engine persisting indexes under `<index_dir>/<model revision>`. One index folder holds the indexes of one
    /// kind of listing: all files, or only documentation.
    pub fn new(index_dir: &Path, embedder: Embedder) -> Self {
        Self {
            cache_dir: index_dir.join(MODEL_REVISION),
            embedder,
            indexes: Mutex::default(),
        }
    }

    /// [`Engine::search_sources`] over whole projects of equal weight.
    pub fn search(
        &self,
        roots: &[PathBuf],
        query: &str,
        limit: usize,
        docs_only: bool,
        progress: Progress<'_>,
    ) -> io::Result<Vec<Found>> {
        let sources: Vec<Source> = roots
            .iter()
            .map(|root| Source {
                root: root.clone(),
                boost: 1.0,
                ignores: Ignores::WithParents,
            })
            .collect();
        self.search_sources(&sources, query, limit, docs_only, progress)
    }

    /// Brings the index of each source up to date with its folder, then returns the best `limit` chunks for `query`
    /// across them, weighted by their boosts, only documentation files when `docs_only`.
    pub fn search_sources(
        &self,
        sources: &[Source],
        query: &str,
        limit: usize,
        docs_only: bool,
        progress: Progress<'_>,
    ) -> io::Result<Vec<Found>> {
        self.search_sources_in(sources, query, limit, docs_only, progress, None)
    }

    /// Searches the same source indexes, but ranks only files under `scope` when it is set.
    pub(crate) fn search_sources_in(
        &self,
        sources: &[Source],
        query: &str,
        limit: usize,
        docs_only: bool,
        progress: Progress<'_>,
        scope: Option<&Path>,
    ) -> io::Result<Vec<Found>> {
        let mut indexes = self.indexes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut exclusions = None;
        let mut listed = Vec::with_capacity(sources.len());
        for (done, source) in sources.iter().enumerate() {
            let root = normalize(&source.root)?;
            let meta = fs::metadata(&root)
                .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", root.display())))?;
            let cached = indexes.get_mut(&root);
            let current = cached.is_some_and(|cached| {
                if cached.watch.as_mut().is_some_and(|(ignores, docs, watch)| {
                    *ignores == source.ignores && *docs == docs_only && watch.unchanged(&meta)
                }) {
                    true
                } else {
                    cached.watch = None;
                    false
                }
            });
            let refresh = if current {
                None
            } else {
                progress("listing folders", done, sources.len());
                let exclusions = match &mut exclusions {
                    Some(exclusions) => exclusions,
                    None => exclusions.insert(Exclusions::new(&[]).map_err(io::Error::other)?),
                };
                Some(Watch::list(&root, exclusions, docs_only, source.ignores)?)
            };
            listed.push((root, refresh));
        }
        if exclusions.is_some() {
            progress("listing folders", sources.len(), sources.len());
        }
        let total: usize = listed
            .iter()
            .filter_map(|(_, refresh)| refresh.as_ref().map(|(files, _)| files.len()))
            .sum();
        let mut done = 0;
        let mut changed = Vec::new();
        for ((root, refresh), source) in listed.iter_mut().zip(sources) {
            let Some((files, watch)) = refresh.take() else {
                continue;
            };
            let cached = indexes.entry(root.clone()).or_insert_with(|| RootIndex {
                index: self.restore(root),
                watch: None,
            });
            let report = |files_done: usize| progress("indexing files", done + files_done, total);
            let (updated, complete) = update(&self.embedder, root, &mut cached.index, &files, &report);
            cached.watch = watch
                .filter(|_| complete)
                .map(|watch| (source.ignores, docs_only, watch));
            if updated && !changed.contains(root) {
                changed.push(root.clone());
            }
            done += files.len();
        }
        for (saved, root) in changed.iter().enumerate() {
            progress("saving indexes", saved, changed.len());
            if let Some(cached) = indexes.get_mut(root) {
                self.persist(root, &mut cached.index);
            }
        }
        if !changed.is_empty() {
            progress("saving indexes", changed.len(), changed.len());
        }
        let names: Vec<String> = (0..listed.len()).map(|position| position.to_string()).collect();
        let prefixes: Vec<Option<String>> = listed
            .iter()
            .map(|(root, _)| {
                scope
                    .and_then(|path| path.strip_prefix(root).ok())
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        let searched: Vec<SearchSource<'_>> = listed
            .iter()
            .zip(sources)
            .zip(&names)
            .zip(&prefixes)
            .filter_map(|((((root, _), source), name), prefix)| {
                if scope.is_some() && prefix.is_none() {
                    return None;
                }
                Some(SearchSource {
                    name,
                    index: &indexes.get(root)?.index,
                    boost: source.boost,
                    path_prefix: prefix.as_deref(),
                })
            })
            .collect();
        Ok(search(&self.embedder, &searched, query, limit)?
            .into_iter()
            .filter_map(|hit| {
                let source = hit.source.parse::<usize>().ok()?;
                let (root, _) = listed.get(source)?;
                Some(Found {
                    source,
                    file: root.join(&hit.path).to_string_lossy().replace('\\', "/"),
                    start_line: hit.start_line,
                    end_line: hit.end_line,
                    score: hit.score,
                    content: hit.content,
                })
            })
            .collect())
    }

    /// The persisted index of `root`, or an empty one when there is none or it does not fit the model.
    fn restore(&self, root: &Path) -> SourceIndex {
        self.open(&cache_file(&self.cache_dir, root))
            .unwrap_or_else(|_| SourceIndex::new(self.embedder.dim()))
    }

    fn open(&self, file: &Path) -> io::Result<SourceIndex> {
        SourceIndex::open(Arc::new(fs::File::open(file)?), self.embedder.dim())
    }

    /// Writes the index through a temporary file and a rename, then reads it from that file. The index is rebuilt
    /// from its folder when the copy is missing, so a failed write only costs time.
    fn persist(&self, root: &Path, index: &mut SourceIndex) {
        let file = cache_file(&self.cache_dir, root);
        let written = fs::create_dir_all(&self.cache_dir).and_then(|()| {
            // A private temporary keeps concurrent writers from sharing the inode even before locking.
            let mut tmp = tempfile::NamedTempFile::new_in(&self.cache_dir)?;
            #[cfg(test)]
            tests::pause_after_temp_open();
            index.write_to(tmp.as_file_mut())?;

            // Match decode-index's stable lock sibling; never remove it while another writer may use it.
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(file.with_extension("lock"))?;
            lock.lock()?;
            let published = tmp.persist(&file)?;
            SourceIndex::open(Arc::new(published), self.embedder.dim())
        });
        match written {
            Ok(reopened) => *index = reopened,
            Err(error) => {
                eprintln!(
                    "decode-semantic: could not persist the index of {}: {error}",
                    root.display()
                );
            }
        }
    }
}

/// Re-indexes the files of `listing` that changed since `index` saw them and drops the ones gone. Files are read
/// and embedded in parallel on half the cores; `report` is called on this thread with the files done. Returns
/// whether anything changed and whether every intended file was read.
fn update(
    embedder: &Embedder,
    root: &Path,
    index: &mut SourceIndex,
    listing: &BTreeMap<String, FileStamp>,
    report: &dyn Fn(usize),
) -> (bool, bool) {
    let changed = changes(index, listing);
    let unchanged = listing.len() - changed.upsert.len();
    report(unchanged);
    if changed.upsert.is_empty() && changed.remove.is_empty() {
        return (false, true);
    }
    for path in &changed.remove {
        index.remove(path);
    }
    let workers = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .div_ceil(2)
        .min(changed.upsert.len())
        .max(1);
    let prepare = |(path, stamp): &(String, FileStamp)| {
        // Deleted or unreadable since it was listed: left out until the next search lists it again.
        let bytes = fs::read(root.join(path)).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        Some((path.clone(), *stamp, PreparedFile::new(embedder, path, &text)))
    };
    let prepared: Vec<(String, FileStamp, PreparedFile)> = std::thread::scope(|scope| {
        let chunk = changed.upsert.len().div_ceil(workers).max(1);
        let (sender, receiver) = std::sync::mpsc::channel();
        let handles: Vec<_> = changed
            .upsert
            .chunks(chunk)
            .map(|files| {
                let sender = sender.clone();
                scope.spawn(move || {
                    let mut prepared = Vec::new();
                    for file in files {
                        prepared.extend(prepare(file));
                        let _ = sender.send(());
                    }
                    prepared
                })
            })
            .collect();
        drop(sender);
        for (done, ()) in receiver.iter().enumerate() {
            report(unchanged + done + 1);
        }
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    let read: BTreeSet<&str> = prepared.iter().map(|(path, _, _)| path.as_str()).collect();
    let complete = read.len() == changed.upsert.len();
    for (path, _) in &changed.upsert {
        if !read.contains(path.as_str()) {
            index.remove(path);
        }
    }
    for (path, stamp, file) in prepared {
        index.upsert(&path, stamp, file);
    }
    (true, complete)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Read;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use decode_semantic_core::test_embedder;

    use super::*;

    fn embedder() -> Embedder {
        test_embedder(&["deploy", "bundle", "bucket", "parse", "token"]).unwrap()
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn files_of(found: &[Found]) -> Vec<String> {
        found
            .iter()
            .map(|hit| hit.file.rsplit('/').next().unwrap_or_default().to_owned())
            .collect()
    }

    fn quiet(_: &str, _: usize, _: usize) {}

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn warm_search_skips_listing_until_the_files_change() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_path_buf();
        let engine = Engine::new(storage.path(), embedder());
        let progress = std::cell::RefCell::new(Vec::new());
        let search = || {
            progress.borrow_mut().clear();
            engine
                .search(
                    std::slice::from_ref(&root),
                    "deploy bundle",
                    10,
                    false,
                    &|stage, _, _| {
                        progress.borrow_mut().push(stage.to_owned());
                    },
                )
                .unwrap()
        };
        write(
            &root,
            "src/deploy.md",
            "# Deploy\n\nDeploy the original bundle.\n",
        );
        let original = search();
        assert_eq!(files_of(&original), ["deploy.md"]);
        assert!(progress.borrow().iter().any(|stage| stage == "listing folders"));
        assert_eq!(search(), original);
        assert!(progress.borrow().is_empty(), "warm search did work: {progress:?}");

        write(
            &root,
            "src/deploy.md",
            "# Deploy\n\nDeploy the replacement bundle to the bucket.\n",
        );
        let edited = search();
        assert!(
            edited
                .iter()
                .any(|hit| hit.content.contains("replacement bundle")),
            "{edited:?}"
        );
        assert!(
            !edited.iter().any(|hit| hit.content.contains("original bundle")),
            "{edited:?}"
        );

        write(
            &root,
            "new/bucket.md",
            "# Bucket\n\nDeploy the bundle to this bucket.\n",
        );
        assert!(files_of(&search()).contains(&"bucket.md".to_owned()));
        fs::remove_file(root.join("src/deploy.md")).unwrap();
        assert_eq!(files_of(&search()), ["bucket.md"]);
        search();
        assert!(progress.borrow().is_empty(), "warm search did work: {progress:?}");

        // Replacing an inode must also replace its watch, so the next in-place edit is seen.
        write(
            &root,
            "replacement",
            "# Bucket\n\nDeploy another bundle instead.\n",
        );
        fs::rename(root.join("replacement"), root.join("new/bucket.md")).unwrap();
        assert!(search().iter().any(|hit| hit.content.contains("another bundle")));
        write(
            &root,
            "new/bucket.md",
            "# Bucket\n\nDeploy the final bundle into the bucket.\n",
        );
        assert!(search().iter().any(|hit| hit.content.contains("final bundle")));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn warm_search_observes_ignore_rules_outside_the_source() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().join("source");
        let external = project.path().join("rules");
        write(
            &root,
            "deploy.md",
            "# Deploy\n\nDeploy the bundle to the bucket.\n",
        );
        std::os::unix::fs::symlink(&external, root.join(".decodeignore")).unwrap();
        let engine = Engine::new(storage.path(), embedder());
        let search = || {
            engine
                .search(std::slice::from_ref(&root), "deploy", 10, false, &quiet)
                .unwrap()
        };
        assert_eq!(files_of(&search()), ["deploy.md"]);
        fs::write(&external, "deploy.md\n").unwrap();
        assert!(search().is_empty());
        fs::remove_file(&external).unwrap();
        assert_eq!(files_of(&search()), ["deploy.md"]);

        fs::write(project.path().join(".ignore"), "deploy.md\n").unwrap();
        assert!(search().is_empty());
        fs::write(project.path().join(".ignore"), "retain.md\n").unwrap();
        assert_eq!(files_of(&search()), ["deploy.md"]);
    }

    type TempOpenHook = (Sender<()>, Receiver<()>);
    thread_local! {
        static TEMP_OPEN: Cell<Option<TempOpenHook>> = const { Cell::new(None) };
    }

    pub(super) fn pause_after_temp_open() {
        if let Some((opened, resume)) = TEMP_OPEN.take() {
            opened.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    #[test]
    fn concurrent_writers_never_modify_a_published_index_in_place() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_path_buf();
        let a = Engine::new(storage.path(), embedder());
        let b = Engine::new(storage.path(), embedder());
        write(&root, "a.md", "# Alpha\n\ndeploy bundle alpha\n");
        let (opened_tx, opened_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let root = &root;
            let first = scope.spawn(move || {
                TEMP_OPEN.set(Some((opened_tx, resume_rx)));
                a.search(std::slice::from_ref(root), "deploy", 3, false, &quiet)
                    .unwrap();
            });
            opened_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            write(root, "a.md", "# Bravo\n\ndeploy bundle bravo\n");
            b.search(std::slice::from_ref(root), "deploy", 3, false, &quiet)
                .unwrap();
            let file = cache_file(&b.cache_dir, root);
            let bytes = fs::read(&file).unwrap();
            // Keep B's exact publication open across A's write and possible replacement.
            let mut published = fs::File::open(&file).unwrap();
            resume_tx.send(()).unwrap();
            first.join().unwrap();
            let mut after = Vec::new();
            published.read_to_end(&mut after).unwrap();
            assert!(
                bytes == after,
                "the other writer modified the published index in place"
            );
            assert!(
                bytes != fs::read(&file).unwrap(),
                "the first writer must publish a replacement"
            );
            b.open(&file).unwrap();
        });
    }

    #[test]
    fn finds_files_by_meaning_and_follows_edits_and_deletions() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_path_buf();
        write(
            &root,
            "deploy.md",
            "# Deploy\n\nDeploy the bundle to the bucket.\n",
        );
        write(&root, "parser.rs", "pub fn parse_token() {}\n");
        let engine = Engine::new(storage.path(), embedder());

        let found = engine
            .search(
                std::slice::from_ref(&root),
                "deploy bundle bucket",
                5,
                false,
                &quiet,
            )
            .unwrap();
        assert_eq!(files_of(&found).first().map(String::as_str), Some("deploy.md"));

        // An edit and a deletion are picked up by the next search.
        write(
            &root,
            "parser.rs",
            "pub fn parse_token() { /* deploy bundle bucket deploy bundle bucket */ }\n",
        );
        fs::remove_file(root.join("deploy.md")).unwrap();
        let found = engine
            .search(
                std::slice::from_ref(&root),
                "deploy bundle bucket",
                5,
                false,
                &quiet,
            )
            .unwrap();
        assert_eq!(files_of(&found), ["parser.rs"]);
    }

    #[test]
    fn a_new_engine_restores_the_persisted_index_instead_of_re_reading_the_files() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_path_buf();
        write(
            &root,
            "deploy.md",
            "# Deploy\n\nDeploy the bundle to the bucket.\n",
        );
        Engine::new(storage.path(), embedder())
            .search(std::slice::from_ref(&root), "deploy", 3, false, &quiet)
            .unwrap();

        // Same size and mtime-independent content check: the stamp is unchanged, so a restored index answers
        // from the cached chunk text even though the file now says something else.
        let path = root.join("deploy.md");
        let meta = fs::metadata(&path).unwrap();
        let original = "# Deploy\n\nDeploy the bundle to the bucket.\n";
        fs::write(&path, "x".repeat(original.len())).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(meta.modified().unwrap())
            .unwrap();
        let found = Engine::new(storage.path(), embedder())
            .search(&[root], "deploy", 3, false, &quiet)
            .unwrap();
        assert!(
            found.first().is_some_and(|hit| hit.content.contains("bucket")),
            "{found:?}"
        );
    }

    #[test]
    fn reports_progress_while_indexing_and_fails_on_a_missing_folder() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_path_buf();
        write(&root, "a.md", "# A\n\ndeploy\n");
        write(&root, "b.md", "# B\n\nbundle\n");
        let engine = Engine::new(storage.path(), embedder());
        let seen = std::cell::RefCell::new(Vec::new());
        engine
            .search(
                std::slice::from_ref(&root),
                "deploy",
                3,
                false,
                &|stage, done, total| {
                    seen.borrow_mut().push((stage.to_owned(), done, total));
                },
            )
            .unwrap();
        assert!(seen.borrow().contains(&("indexing files".to_owned(), 2, 2)));
        assert_eq!(seen.borrow().last(), Some(&("saving indexes".to_owned(), 1, 1)));
        let missing = root.join("nope");
        assert!(engine.search(&[missing], "deploy", 3, false, &quiet).is_err());
    }

    /// The same note in two folders: the boosted folder's ranks first, and each result names its source.
    #[test]
    fn a_boosted_source_ranks_first_and_indexes_are_named_by_folder_path() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let note = "# Deploy\n\nDeploy the bundle to the bucket.\n";
        write(project.path(), "a/deploy.md", note);
        write(project.path(), "b/deploy.md", note);
        let source = |name: &str, boost: f64| Source {
            root: project.path().join(name).join("."),
            boost,
            ignores: Ignores::Own,
        };
        let engine = Engine::new(storage.path(), embedder());
        let ranked = |sources: &[Source]| -> Vec<usize> {
            engine
                .search_sources(sources, "deploy bundle", 5, true, &quiet)
                .unwrap()
                .iter()
                .map(|hit| hit.source)
                .collect()
        };
        assert_eq!(ranked(&[source("a", 1.0), source("b", 2.0)]), [1, 0]);
        assert_eq!(ranked(&[source("a", 2.0), source("b", 1.0)]), [0, 1]);
        let cache = storage.path().join("e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b");
        assert!(cache_file(&cache, &project.path().join("a")).is_file());
        assert!(
            !storage
                .path()
                .join("1b0ff71095656b23306542bbad34a09109673720")
                .exists()
        );
    }
}
