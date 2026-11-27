//! Folders no index holds, shared by every listing so indexes written by one engine stay valid for the others.

use std::path::Path;

/// Dependency and build-output directories, never indexed at any depth: Semble `_DEFAULT_IGNORED_DIRS` without its
/// hidden folders (the hidden folder rule skips those), plus `vendor`, where Go, Composer and Bundler keep copies of
/// third-party code.
pub const SKIPPED_DIRS: &[&str] = &["__pycache__", "node_modules", "venv", "dist", "build", "vendor"];

/// The folder holding a project's feature notes.
const NOTES_DIR: &str = "memory";
/// The folders under [`NOTES_DIR`] holding a note's evidence (logs, data, scripts, copies of other repositories).
const NOTES_EVIDENCE_DIR: &str = "artifacts";

/// Whether `relative` (root-relative, `/`-separated) lies in one of [`SKIPPED_DIRS`] or a hidden folder.
pub fn in_skipped_dir(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| (part.starts_with('.') && part != ".") || SKIPPED_DIRS.contains(&part))
}

/// Whether `relative` (relative to `root`, `/`-separated) lies in an `artifacts` folder of the notes: `memory/**/artifacts`,
/// whether `root` is the project or its `memory` folder itself.
fn in_notes_evidence(root: &Path, relative: &str) -> bool {
    let beneath_notes = if root.file_name().is_some_and(|name| name == NOTES_DIR) {
        relative
    } else {
        match relative
            .strip_prefix(NOTES_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            Some(rest) => rest,
            None => return false,
        }
    };
    beneath_notes.split('/').any(|part| part == NOTES_EVIDENCE_DIR)
}

/// Whether no listing of `root` holds `relative` (root-relative, `/`-separated): a file or folder in a skipped or hidden
/// folder ([`in_skipped_dir`]) or in the notes' evidence folders.
pub fn is_unindexed(root: &Path, relative: &str) -> bool {
    in_skipped_dir(relative) || in_notes_evidence(root, relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_the_notes_evidence_folders_whether_the_root_is_the_project_or_its_notes() {
        let project = Path::new("/code/app");
        let notes = Path::new("/code/app/memory");
        assert!(is_unindexed(notes, "20260101-topic/artifacts/run/results.md"));
        assert!(is_unindexed(notes, "20260101-topic/artifacts"));
        assert!(is_unindexed(
            project,
            "memory/20260101-topic/artifacts/run/results.md"
        ));
        assert!(!is_unindexed(notes, "20260101-topic/context.md"));
        assert!(!is_unindexed(project, "memory/20260101-topic/context.md"));
        // `artifacts` outside the notes is an ordinary folder.
        assert!(!is_unindexed(project, "src/artifacts/store.rs"));
        assert!(!is_unindexed(project, "docs/memory-notes/artifacts/a.md"));
        assert!(!is_unindexed(project, "memory-bank/artifacts/a.md"));
        assert!(!is_unindexed(
            Path::new("/code/app/notes"),
            "topic/artifacts/a.md"
        ));
    }

    #[test]
    fn skips_agent_scratch_and_run_folders_wherever_they_are() {
        let project = Path::new("/code/app");
        for relative in [
            ".scratch/run/log.md",
            ".agent-runs/a/out.md",
            "memory/x/.scratch/a.md",
            "src/.agent-runs/a.md",
        ] {
            assert!(is_unindexed(project, relative), "{relative}");
        }
    }
}
