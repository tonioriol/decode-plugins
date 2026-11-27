//! Which files of a folder the index holds.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;
use std::{fs, io};

use ignore::WalkBuilder;

use crate::{Exclusions, FileStamp, detect_language, is_doc_language, is_unindexed};

/// Semble `MAX_FILE_BYTES`: a larger file is never indexed.
pub const MAX_FILE_BYTES: u64 = 1_000_000;
/// The most files one folder's index holds.
pub const MAX_FILES: usize = 50_000;
pub(crate) const CUSTOM_IGNORES: [&str; 3] = [".rgignore", ".rooignore", ".decodeignore"];

/// Which ignore files a listing applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignores {
    /// The folder's own ignore files and those of its parents, as `rg --files` applies them to a project.
    WithParents,
    /// Only the folder's own ignore files, as `rg --files --no-ignore-parent` lists a folder on its own.
    Own,
}

/// The files of `root` the index may hold, by root-relative `/`-separated path, with their stamps: files of a
/// language Semble reads, at most [`MAX_FILE_BYTES`] long, outside hidden folders and [`crate::SKIPPED_DIRS`], not ignored
/// (`.gitignore` within the nearest Git repository; `.ignore`, `.rgignore`, `.rooignore`, `.decodeignore`; from
/// parents too with [`Ignores::WithParents`]) and not matching `exclusions`. Only documentation files when
/// `docs_only`. Symbolic links are not followed.
pub fn list(
    root: &Path,
    exclusions: &Exclusions,
    docs_only: bool,
    ignores: Ignores,
) -> io::Result<BTreeMap<String, FileStamp>> {
    list_observing(root, exclusions, docs_only, ignores, |_, _| {})
}

/// Reports directories visited and potential source files, including files large enough to be omitted now.
pub(crate) fn list_observing(
    root: &Path,
    exclusions: &Exclusions,
    docs_only: bool,
    ignores: Ignores,
    mut observe: impl FnMut(&Path, Option<&fs::Metadata>),
) -> io::Result<BTreeMap<String, FileStamp>> {
    let mut builder = WalkBuilder::new(root);
    // The walker's default Git-aware mode stops .gitignore lookup at a nested repository or worktree.
    builder.parents(ignores == Ignores::WithParents);
    for name in CUSTOM_IGNORES {
        builder.add_custom_ignore_filename(name);
    }
    // A folder no index holds is neither read nor watched.
    let walked = root.to_path_buf();
    builder.filter_entry(move |entry| {
        entry.path().strip_prefix(&walked).map_or(true, |relative| {
            !is_unindexed(&walked, &relative.to_string_lossy().replace('\\', "/"))
        })
    });
    let walker = builder.build();
    let mut files = BTreeMap::new();
    for entry in walker {
        let entry = entry.map_err(io::Error::other)?;
        if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
            observe(entry.path(), None);
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let Some(language) = detect_language(&relative) else {
            continue;
        };
        if (docs_only && !is_doc_language(language)) || exclusions.excludes(&relative) {
            continue;
        }
        let meta = entry.metadata();
        observe(entry.path(), meta.as_ref().ok());
        let Ok(meta) = meta else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
            .unwrap_or(0);
        files.insert(
            relative,
            FileStamp {
                mtime_ns,
                size: meta.len(),
            },
        );
        if files.len() >= MAX_FILES {
            break;
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn lists_source_files_and_skips_ignored_hidden_dependency_big_and_unknown_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        write(root, "src/lib.rs", "fn a() {}\n");
        write(root, "docs/guide.md", "# Guide\n");
        write(root, "node_modules/x/index.js", "x\n");
        write(root, ".hidden/secret.rs", "fn s() {}\n");
        write(root, "gen/out.rs", "fn g() {}\n");
        write(root, ".gitignore", "gen/\n");
        write(root, "data.json", "{}\n");
        write(
            root,
            "big.rs",
            &"x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() + 1),
        );
        write(root, "src/build/gen.rs", "fn b() {}\n");
        write(root, "vendor/v.rs", "fn v() {}\n");
        write(root, "out/o.rs", "fn o() {}\n");
        write(root, "private/p.rs", "fn p() {}\n");
        write(root, ".rooignore", "private/\n");
        let none = Exclusions::new(&[]).unwrap();
        let keys = |exclusions: &Exclusions, docs_only: bool| -> Vec<String> {
            list(root, exclusions, docs_only, Ignores::Own)
                .unwrap()
                .into_keys()
                .collect()
        };

        assert_eq!(keys(&none, false), ["docs/guide.md", "out/o.rs", "src/lib.rs"]);
        assert_eq!(keys(&none, true), ["docs/guide.md"]);
        let excluded = Exclusions::new(&["docs".to_owned()]).unwrap();
        assert_eq!(keys(&excluded, false), ["out/o.rs", "src/lib.rs"]);
    }

    /// A project listing applies its parents' ignore files; a folder listed on its own applies only its own.
    #[test]
    fn parent_ignore_files_apply_only_to_project_listings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        write(root, ".gitignore", "notes/parent-ignored.md\n");
        write(root, "notes/.gitignore", "own-ignored.md\n");
        for file in ["notes/a.md", "notes/own-ignored.md", "notes/parent-ignored.md"] {
            write(root, file, "# x\n");
        }
        let none = Exclusions::new(&[]).unwrap();
        let keys = |ignores| -> Vec<String> {
            list(&root.join("notes"), &none, true, ignores)
                .unwrap()
                .into_keys()
                .collect()
        };
        assert_eq!(keys(Ignores::Own), ["a.md", "parent-ignored.md"]);
        assert_eq!(keys(Ignores::WithParents), ["a.md"]);
    }

    #[test]
    fn nested_repository_stops_outer_gitignore_rules() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        fs::create_dir(outer.join(".git")).unwrap();
        write(outer, ".gitignore", "mock/\n");
        fs::create_dir_all(outer.join("checkout/.git")).unwrap();
        write(outer, "worktree/.git", "gitdir: ../.git/worktrees/worktree\n");
        for name in ["checkout", "worktree"] {
            let repo = outer.join(name);
            write(&repo, ".gitignore", "generated/\n");
            write(&repo, "src/mock/timers.ts", "export const timers = [];\n");
            write(&repo, "src/generated/types.ts", "export type Id = string;\n");
            let files = list(
                &repo.join("src"),
                &Exclusions::new(&[]).unwrap(),
                false,
                Ignores::WithParents,
            )
            .unwrap()
            .into_keys()
            .collect::<Vec<_>>();
            assert_eq!(files, ["mock/timers.ts"], "{name}");
        }
    }
}
