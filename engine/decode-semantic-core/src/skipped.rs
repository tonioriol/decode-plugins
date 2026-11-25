//! Folders no index holds, shared by every listing so indexes written by one engine stay valid for the others.

/// Dependency and build-output directories, never indexed at any depth: Semble `_DEFAULT_IGNORED_DIRS` without its
/// hidden folders (the hidden folder rule skips those), plus `vendor`, where Go, Composer and Bundler keep copies of
/// third-party code.
pub const SKIPPED_DIRS: &[&str] = &["__pycache__", "node_modules", "venv", "dist", "build", "vendor"];

/// Whether `relative` (root-relative, `/`-separated) lies in one of [`SKIPPED_DIRS`] or a hidden folder.
pub fn in_skipped_dir(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| (part.starts_with('.') && part != ".") || SKIPPED_DIRS.contains(&part))
}
