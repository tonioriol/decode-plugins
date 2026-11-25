//! An empty kernel event queue is a synchronous freshness check, unlike an empty queue fed by an async watcher.
//!
//! Watch the directories the ignore walker visits (for membership changes), candidate files (for in-place writes),
//! and the rules they depend on. Check rules outside those directories separately, rather than watching unrelated
//! trees. After arming, take the actual listing under the watches and verify that they cover that listing.

use std::collections::BTreeMap;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use ignore::gitignore::gitconfig_excludes_path;

use crate::files::{self, CUSTOM_IGNORES, Ignores};
use crate::{Exclusions, FileStamp};

type Identity = (u64, u64);
type Listing = BTreeMap<String, FileStamp>;

fn identity(meta: &Metadata) -> Identity {
    (meta.dev(), meta.ino())
}

/// Ignore inputs outside watched directories. Files are small control inputs, not the corpus: compare their text
/// as well as their identity so a same-size save on a filesystem with coarse timestamps is still seen.
#[derive(Debug, PartialEq, Eq)]
enum Rule {
    Missing,
    File(Identity, Vec<u8>),
    Other(Identity),
}

impl Rule {
    fn read(path: &Path) -> Self {
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => Self::File(identity(&meta), fs::read(path).unwrap_or_default()),
            Ok(meta) => Self::Other(identity(&meta)),
            Err(_) => Self::Missing,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Rules {
    global: Option<PathBuf>,
    files: BTreeMap<PathBuf, Rule>,
}

impl Rules {
    fn new() -> Self {
        let global = gitconfig_excludes_path();
        let files = global
            .iter()
            .map(|path| (path.clone(), Rule::read(path)))
            .collect();
        Self { global, files }
    }

    fn unchanged(&self) -> bool {
        self.global == gitconfig_excludes_path()
            && self.files.iter().all(|(path, rule)| Rule::read(path) == *rule)
    }

    fn add(&mut self, path: PathBuf) -> &Rule {
        self.files
            .entry(path.clone())
            .or_insert_with(|| Rule::read(&path))
    }

    /// The gitignore library also reads Git's common directory for a worktree's `info/exclude`.
    fn git(&mut self, marker: PathBuf) {
        let common = match self.add(marker.clone()) {
            Rule::Missing => return,
            Rule::Other(_) => marker,
            Rule::File(_, text) => {
                let Some(dir) = std::str::from_utf8(text)
                    .ok()
                    .and_then(|text| text.lines().next())
                    .and_then(|line| line.strip_prefix("gitdir: "))
                    .map(PathBuf::from)
                else {
                    return;
                };
                let Rule::File(_, common) = self.add(dir.join("commondir")) else {
                    return;
                };
                let Some(common) = std::str::from_utf8(common)
                    .ok()
                    .and_then(|text| text.lines().next())
                else {
                    return;
                };
                if common.starts_with('.') {
                    dir.join(common)
                } else {
                    PathBuf::from(common)
                }
            }
        };
        self.add(common.join("info/exclude"));
    }
}

fn ignore_names() -> impl Iterator<Item = &'static str> {
    [".ignore", ".gitignore"].into_iter().chain(CUSTOM_IGNORES)
}

#[derive(Debug, PartialEq, Eq)]
struct Coverage {
    paths: BTreeMap<PathBuf, Identity>,
    rules: Rules,
    complete: bool,
}

impl Coverage {
    fn new(root: &Path, ignores: Ignores) -> io::Result<Self> {
        let mut rules = Rules::new();
        let real = root.canonicalize()?;
        for dir in real.ancestors().skip(1) {
            if ignores == Ignores::WithParents {
                for name in ignore_names() {
                    rules.add(dir.join(name));
                }
                rules.git(dir.join(".git"));
            } else {
                // Even without parent ignore rules, a parent VCS marker enables Git ignores in this root.
                rules.add(dir.join(".git"));
            }
            rules.add(dir.join(".jj"));
        }
        Ok(Self {
            paths: BTreeMap::new(),
            rules,
            complete: true,
        })
    }

    fn path(&mut self, path: &Path, meta: &Metadata) {
        self.paths.insert(path.to_path_buf(), identity(meta));
    }

    fn observe(&mut self, path: &Path, meta: Option<&Metadata>) {
        if let Some(meta) = meta {
            self.path(path, meta);
            return;
        }
        let Ok(meta) = fs::metadata(path) else {
            self.complete = false;
            return;
        };
        if !meta.is_dir() {
            self.complete = false;
            return;
        }
        self.path(path, &meta);
        for name in ignore_names() {
            let rule = path.join(name);
            if let Ok(meta) = fs::symlink_metadata(&rule) {
                if meta.is_file() {
                    self.path(&rule, &meta);
                } else if meta.is_symlink() {
                    self.rules.add(rule);
                }
            }
        }
        let jj = path.join(".jj");
        if fs::symlink_metadata(&jj).is_ok_and(|meta| meta.is_symlink()) {
            self.rules.add(jj);
        }
        let marker = path.join(".git");
        if fs::symlink_metadata(&marker).is_ok() {
            self.rules.git(marker);
        }
    }
}

/// Proof that a folder's listing is unchanged: a kernel queue on everything the listing depends on.
#[derive(Debug)]
pub struct Watch {
    queue: platform::Queue,
    root: Identity,
    rules: Rules,
}

impl Watch {
    /// Whether nothing the listing depends on changed since it was taken; `meta` is the root's current metadata.
    pub fn unchanged(&mut self, meta: &Metadata) -> bool {
        self.root == identity(meta) && self.rules.unchanged() && self.queue.empty()
    }

    /// [`files::list`] of `root`, with a watch when every path it depends on could be armed and the listing taken
    /// under the armed watches matches the one before.
    pub fn list(
        root: &Path,
        exclusions: &Exclusions,
        docs_only: bool,
        ignores: Ignores,
    ) -> io::Result<(Listing, Option<Self>)> {
        let observe = || {
            let mut coverage = Coverage::new(root, ignores)?;
            let listing = files::list_observing(root, exclusions, docs_only, ignores, |path, meta| {
                coverage.observe(path, meta);
            })?;
            Ok::<_, io::Error>((listing, coverage))
        };
        let (listing, before) = observe()?;
        let Some(&root_id) = before.paths.get(root).filter(|_| before.complete) else {
            return Ok((listing, None));
        };
        // Refused watches (e.g. descriptor limits) give no proof of freshness. Keep listing that source.
        let Ok(queue) = platform::Queue::arm(&before.paths) else {
            return Ok((listing, None));
        };
        let (listing, after) = observe()?;
        let watch = (before == after).then_some(Self {
            queue,
            root: root_id,
            rules: after.rules,
        });
        Ok((listing, watch))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{BTreeMap, Identity, PathBuf, identity, io};
    use std::fs::File;

    use kqueue::{EventFilter::EVFILT_VNODE, FilterFlag, Watcher};

    #[derive(Debug)]
    pub(super) struct Queue {
        watcher: Watcher,
        // The kernel watches vnodes through these open descriptors.
        _files: Vec<File>,
    }

    impl Queue {
        pub(super) fn arm(paths: &BTreeMap<PathBuf, Identity>) -> io::Result<Self> {
            let mut watcher = Watcher::new()?;
            let mut files = Vec::with_capacity(paths.len());
            for (path, expected) in paths {
                let file = File::open(path)?;
                if identity(&file.metadata()?) != *expected {
                    return Err(io::Error::other("vnode replaced while arming"));
                }
                watcher.add_file(
                    &file,
                    EVFILT_VNODE,
                    FilterFlag::NOTE_WRITE
                        | FilterFlag::NOTE_DELETE
                        | FilterFlag::NOTE_RENAME
                        | FilterFlag::NOTE_ATTRIB
                        | FilterFlag::NOTE_REVOKE
                        | FilterFlag::NOTE_EXTEND
                        | FilterFlag::NOTE_LINK,
                )?;
                files.push(file);
            }
            watcher.watch()?;
            Ok(Self {
                watcher,
                _files: files,
            })
        }

        pub(super) fn empty(&self) -> bool {
            self.watcher.poll(None).is_none()
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{BTreeMap, Identity, PathBuf, fs, identity, io};
    use inotify::{Inotify, WatchMask};

    #[derive(Debug)]
    pub(super) struct Queue(Inotify);

    impl Queue {
        pub(super) fn arm(paths: &BTreeMap<PathBuf, Identity>) -> io::Result<Self> {
            let queue = Inotify::init()?;
            for (path, expected) in paths {
                queue.watches().add(
                    path,
                    WatchMask::CREATE
                        | WatchMask::DELETE
                        | WatchMask::MOVE
                        | WatchMask::MOVE_SELF
                        | WatchMask::MODIFY
                        | WatchMask::ATTRIB
                        | WatchMask::DELETE_SELF,
                )?;
                if identity(&fs::metadata(path)?) != *expected {
                    return Err(io::Error::other("inode replaced while arming"));
                }
            }
            Ok(Self(queue))
        }

        pub(super) fn empty(&mut self) -> bool {
            self.0
                .read_events(&mut [0; 4096])
                .is_err_and(|err| err.kind() == io::ErrorKind::WouldBlock)
        }
    }
}
