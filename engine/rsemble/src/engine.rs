//! One persisted index per folder, kept in step with its files before each search.
//!
//! The index of a folder is the file `<index dir>/<model revision>/<SHA-256 of the folder's path>.bin`, the name and
//! format Decode gives it, so Decode and rsemble pointed at the same index folder keep one index per folder.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rsemble_core::{
    Embedder, Exclusions, FileStamp, PreparedFile, SearchSource, SourceIndex, changes, search,
};
use sha2::{Digest, Sha256};

use crate::files::{self, Ignores};
use crate::model::MODEL_REVISION;

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
    indexes: Mutex<BTreeMap<PathBuf, SourceIndex>>,
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
        let exclusions = Exclusions::new(&[]).map_err(io::Error::other)?;
        let mut listed = Vec::with_capacity(sources.len());
        for (done, source) in sources.iter().enumerate() {
            progress("listing folders", done, sources.len());
            let root = normalize(&source.root)?;
            fs::metadata(&root)
                .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", root.display())))?;
            let files = files::list(&root, &exclusions, docs_only, source.ignores)?;
            listed.push((root, files));
        }
        progress("listing folders", sources.len(), sources.len());
        let mut indexes = self.indexes.lock().unwrap_or_else(PoisonError::into_inner);
        let total: usize = listed.iter().map(|(_, files)| files.len()).sum();
        let mut done = 0;
        let mut changed = Vec::new();
        for (root, files) in &listed {
            let index = indexes.entry(root.clone()).or_insert_with(|| self.restore(root));
            let report = |files_done: usize| progress("indexing files", done + files_done, total);
            if update(&self.embedder, root, index, files, &report) && !changed.contains(root) {
                changed.push(root.clone());
            }
            done += files.len();
        }
        for (saved, root) in changed.iter().enumerate() {
            progress("saving indexes", saved, changed.len());
            if let Some(index) = indexes.get_mut(root) {
                self.persist(root, index);
            }
        }
        if !changed.is_empty() {
            progress("saving indexes", changed.len(), changed.len());
        }
        let names: Vec<String> = (0..listed.len()).map(|position| position.to_string()).collect();
        let searched: Vec<SearchSource<'_>> = listed
            .iter()
            .zip(sources)
            .zip(&names)
            .filter_map(|(((root, _), source), name)| {
                Some(SearchSource {
                    name,
                    index: indexes.get(root)?,
                    boost: source.boost,
                    path_prefix: None,
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
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        let written = fs::create_dir_all(&self.cache_dir)
            .and_then(|()| index.write_to(&mut fs::File::create(&tmp)?))
            .and_then(|()| fs::rename(&tmp, &file))
            .and_then(|()| self.open(&file));
        match written {
            Ok(reopened) => *index = reopened,
            Err(error) => {
                drop(fs::remove_file(&tmp));
                eprintln!(
                    "rsemble: could not persist the index of {}: {error}",
                    root.display()
                );
            }
        }
    }
}

/// Re-indexes the files of `listing` that changed since `index` saw them and drops the ones gone. Files are read
/// and embedded in parallel on half the cores; `report` is called on this thread with the files done. Returns
/// whether anything changed.
fn update(
    embedder: &Embedder,
    root: &Path,
    index: &mut SourceIndex,
    listing: &BTreeMap<String, FileStamp>,
    report: &dyn Fn(usize),
) -> bool {
    let changed = changes(index, listing);
    let unchanged = listing.len() - changed.upsert.len();
    report(unchanged);
    if changed.upsert.is_empty() && changed.remove.is_empty() {
        return false;
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
    for (path, _) in &changed.upsert {
        if !read.contains(path.as_str()) {
            index.remove(path);
        }
    }
    for (path, stamp, file) in prepared {
        index.upsert(&path, stamp, file);
    }
    true
}

#[cfg(test)]
mod tests {
    use rsemble_core::test_embedder;

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
