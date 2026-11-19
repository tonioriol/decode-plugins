//! Keeping a source index in step with a file listing: which files to (re)index or drop, and which the user's
//! exclude globs leave out.

use std::collections::BTreeMap;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::index::{FileStamp, SourceIndex};

/// Glob patterns excluded from indexing, gitignore-like globs: matched against the path relative to the
/// source root and against each of its folders, so `**/vendor` drops everything under any `vendor` folder.
#[derive(Debug, Clone)]
pub struct Exclusions {
    set: GlobSet,
}

impl Exclusions {
    /// Compiles `patterns`; an invalid pattern is an error naming it.
    pub fn new(patterns: &[String]) -> Result<Self, String> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let pattern = pattern.trim().trim_start_matches("./").trim_end_matches('/');
            if pattern.is_empty() {
                continue;
            }
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|error| format!("invalid exclude pattern {pattern:?}: {error}"))?;
            builder.add(glob);
        }
        let set = builder.build().map_err(|error| error.to_string())?;
        Ok(Self { set })
    }

    /// Whether `path` (relative, `/`-separated) or one of its folders matches a pattern.
    pub fn excludes(&self, path: &str) -> bool {
        if self.set.is_empty() {
            return false;
        }
        path.match_indices('/')
            .map(|(at, _)| path.get(..at).unwrap_or(path))
            .chain([path])
            .any(|candidate| self.set.is_match(candidate))
    }
}

/// What an index must do to match a listing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// New or changed files to (re)index, with their stamps.
    pub upsert: Vec<(String, FileStamp)>,
    /// Indexed files no longer listed.
    pub remove: Vec<String>,
}

/// Compares `listing` (path to stamp) with what `index` holds.
pub fn changes(index: &SourceIndex, listing: &BTreeMap<String, FileStamp>) -> Changes {
    let upsert = listing
        .iter()
        .filter(|(path, stamp)| index.stamp(path) != Some(**stamp))
        .map(|(path, stamp)| (path.clone(), *stamp))
        .collect();
    let remove = index
        .files()
        .filter(|(path, _)| !listing.contains_key(*path))
        .map(|(path, _)| path.to_owned())
        .collect();
    Changes { upsert, remove }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Exclusions, changes};
    use crate::embed::test_embedder;
    use crate::index::{FileStamp, PreparedFile, SourceIndex};

    #[test]
    fn excludes_matching_files_and_folders() {
        let exclusions =
            Exclusions::new(&["**/vendor".into(), "*.min.js".into(), "./generated/".into()]).unwrap();
        assert!(exclusions.excludes("a/vendor/lib.rs"));
        assert!(exclusions.excludes("vendor/lib.rs"));
        assert!(exclusions.excludes("app.min.js"));
        assert!(!exclusions.excludes("web/app.min.js"));
        assert!(exclusions.excludes("generated/x.ts"));
        assert!(!exclusions.excludes("src/generated.ts"));
        assert!(!exclusions.excludes("src/vendors/lib.rs"));
        assert!(!Exclusions::new(&[]).unwrap().excludes("anything.rs"));
        assert!(Exclusions::new(&["a[".into()]).is_err());
    }

    #[test]
    fn diffs_a_listing_against_the_index() {
        let embedder = test_embedder(&["x"]).unwrap();
        let mut index = SourceIndex::new(1);
        let old = FileStamp { mtime_ns: 1, size: 3 };
        for path in ["kept.rs", "changed.rs", "gone.rs"] {
            index.upsert(path, old, PreparedFile::new(&embedder, path, "fn x() {}\n"));
        }
        let new = FileStamp { mtime_ns: 2, size: 3 };
        let listing = BTreeMap::from([
            ("kept.rs".to_owned(), old),
            ("changed.rs".to_owned(), new),
            ("added.rs".to_owned(), new),
        ]);
        let diff = changes(&index, &listing);
        assert_eq!(
            diff.upsert,
            [("added.rs".to_owned(), new), ("changed.rs".to_owned(), new)]
        );
        assert_eq!(diff.remove, ["gone.rs"]);
    }
}
