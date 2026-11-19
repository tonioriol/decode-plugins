//! `rsemble serve`: a stdio MCP server with one search tool over the same-named folders of many projects (say every
//! project's `notes/`), searched together.
//!
//! Projects are the `--project` directories and the directories under each `--scan` root (to `--depth`, skipping
//! hidden and `--skip` names) that hold the first `--folder`; each project's `--folder`s are its sources, weighted by
//! their boost. The caller's project (the `workspace` argument, else the server's working directory) is boosted
//! further, and `project` restricts a search to one project. The folder `.` is the project itself, listed as a
//! project (its parents' ignore files apply); a named folder is listed on its own. Indexes are built at startup and
//! brought up to date before each search; the model is downloaded on first use.
//!
//! ```text
//! rsemble serve --storage <dir> [--index <dir>] [--tool search] [--description <text>] [--scan <dir>]...
//!     [--depth 3] [--skip <name>]... [--project <dir>]... --folder <name>[=<boost>]... [--docs-only]
//!     [--current-boost 1.5]
//! ```

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use rsemble_core::Embedder;
use serde_json::{Value, json};

use crate::engine::{Engine, Found, Source};
use crate::files::Ignores;
use crate::mcp::{DEFAULT_LIMIT, MAX_LIMIT, Notifier};
use crate::model::ensure_model;

/// How to call `rsemble serve`.
pub const USAGE: &str = "rsemble serve --storage <dir> [--index <dir>] [--tool <name>] [--description <text>] [--scan <dir>]... [--depth <n>] [--skip <name>]... [--project <dir>]... --folder <name>[=<boost>]... [--docs-only] [--current-boost <x>]";

/// One folder searched in every project, and how much its results weigh.
#[derive(Debug, Clone, PartialEq)]
struct Folder {
    name: String,
    boost: f64,
}

/// The server's arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    storage: PathBuf,
    index: PathBuf,
    tool: String,
    description: String,
    scan: Vec<PathBuf>,
    depth: usize,
    skip: Vec<String>,
    projects: Vec<PathBuf>,
    /// The first one marks a directory as a project.
    folders: Vec<Folder>,
    docs_only: bool,
    current_boost: f64,
}

/// `~` and `~/…` resolved in `home`.
fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// Parses the arguments after `serve`. The index folder defaults to `<storage>/index`.
pub fn parse_args(args: &[String], home: &Path) -> Result<Options, String> {
    let mut options = Options {
        storage: PathBuf::new(),
        index: PathBuf::new(),
        tool: "search".to_owned(),
        description: "Semantic search across projects.".to_owned(),
        scan: Vec::new(),
        depth: 3,
        skip: Vec::new(),
        projects: Vec::new(),
        folders: Vec::new(),
        docs_only: false,
        current_boost: 1.5,
    };
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let mut value = || {
            words
                .next()
                .cloned()
                .ok_or_else(|| format!("{word} needs a value"))
        };
        match word.as_str() {
            "--storage" => options.storage = expand_home(&value()?, home),
            "--index" => options.index = expand_home(&value()?, home),
            "--tool" => options.tool = value()?,
            "--description" => options.description = value()?,
            "--scan" => options.scan.push(expand_home(&value()?, home)),
            "--project" => options.projects.push(expand_home(&value()?, home)),
            "--skip" => options.skip.push(value()?),
            "--depth" => {
                options.depth = value()?
                    .parse()
                    .map_err(|_| "--depth needs a whole number".to_owned())?;
            }
            "--current-boost" => {
                options.current_boost = value()?
                    .parse()
                    .map_err(|_| "--current-boost needs a number".to_owned())?;
            }
            "--folder" => {
                let spec = value()?;
                let (name, boost) = match spec.split_once('=') {
                    Some((name, boost)) => (
                        name.to_owned(),
                        boost
                            .parse()
                            .map_err(|_| format!("--folder {spec}: the boost must be a number"))?,
                    ),
                    None => (spec.clone(), 1.0),
                };
                if name.is_empty() || name.contains('/') {
                    return Err(format!("--folder {spec}: give a folder name, not a path"));
                }
                options.folders.push(Folder { name, boost });
            }
            "--docs-only" => options.docs_only = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.storage.as_os_str().is_empty() {
        return Err("--storage is required".to_owned());
    }
    if options.index.as_os_str().is_empty() {
        options.index = options.storage.join("index");
    }
    if options.folders.is_empty() {
        return Err("at least one --folder is required".to_owned());
    }
    Ok(options)
}

/// A directory holding the marker folder, and the name its results carry.
#[derive(Debug, Clone, PartialEq)]
struct Project {
    name: String,
    dir: PathBuf,
    /// `dir` with symlinks resolved.
    real: PathBuf,
}

fn real(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every project: the `--project` directories, then the directories under each `--scan` root (never through a
/// symlink, nor into a hidden, skipped or searched folder), that hold the first folder. A scanned project is named by
/// its path from the scan root, a listed one by its folder name (`home` for the home directory); a name taken twice
/// falls back to the path from the home directory.
fn discover(options: &Options, home: &Path) -> Vec<Project> {
    let Some(marker) = options.folders.first() else {
        return Vec::new();
    };
    let mut candidates: Vec<(PathBuf, String)> = options
        .projects
        .iter()
        .map(|dir| {
            let name = if dir == home {
                "home".to_owned()
            } else {
                dir.file_name().map_or_else(
                    || dir.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            };
            (dir.clone(), name)
        })
        .collect();
    for root in &options.scan {
        let mut pending = vec![(root.clone(), 0_usize)];
        let mut found = Vec::new();
        while let Some((dir, depth)) = pending.pop() {
            if depth > 0 {
                let name = dir.strip_prefix(root).map_or_else(
                    |_| dir.display().to_string(),
                    |relative| relative.to_string_lossy().into_owned(),
                );
                found.push((dir.clone(), name));
            }
            if depth >= options.depth {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let searched = options.folders.iter().any(|folder| folder.name == name);
                if name.starts_with('.') || searched || options.skip.contains(&name) {
                    continue;
                }
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
        found.sort();
        candidates.extend(found);
    }
    let mut projects: Vec<Project> = Vec::new();
    for (dir, name) in candidates {
        if !dir.join(&marker.name).is_dir() || projects.iter().any(|known| known.dir == dir) {
            continue;
        }
        let name = if projects.iter().any(|known| known.name == name) {
            dir.strip_prefix(home).map_or_else(
                |_| dir.display().to_string(),
                |relative| relative.to_string_lossy().into_owned(),
            )
        } else {
            name
        };
        let real = real(&dir);
        projects.push(Project { name, dir, real });
    }
    projects
}

/// The project holding `path` (the innermost when they nest).
fn project_of(projects: &[Project], path: &Path) -> Option<usize> {
    let path = real(path);
    projects
        .iter()
        .enumerate()
        .filter(|(_, project)| path.starts_with(&project.real))
        .max_by_key(|(_, project)| project.real.components().count())
        .map(|(position, _)| position)
}

/// The projects `filter` names: a name (case-insensitive, or its last path components, so `demo` finds `tools/demo`),
/// or a path inside one.
fn filtered(projects: &[Project], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    if Path::new(filter).is_absolute() {
        return project_of(projects, Path::new(filter)).into_iter().collect();
    }
    let wanted = filter.trim_matches('/').to_lowercase();
    projects
        .iter()
        .enumerate()
        .filter(|(_, project)| {
            let name = project.name.to_lowercase();
            name == wanted || name.ends_with(&format!("/{wanted}"))
        })
        .map(|(position, _)| position)
        .collect()
}

/// One search.
#[derive(Debug, Default)]
struct Request {
    query: String,
    project: Option<String>,
    workspace: Option<PathBuf>,
    limit: usize,
}

/// One result and the project it is from.
#[derive(Debug, Clone, PartialEq)]
struct Hit {
    project: String,
    found: Found,
}

/// A progress report: stage, done, total.
type Step = (String, usize, usize);

/// The startup indexing: its latest progress, and whether it is over.
#[derive(Debug, Default)]
struct Warmup {
    state: Mutex<(Option<Step>, bool)>,
    changed: Condvar,
}

impl Warmup {
    fn publish(&self, stage: &str, done: usize, total: usize) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 = Some((stage.to_owned(), done, total));
        self.changed.notify_all();
    }

    fn finish(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.changed.notify_all();
    }

    /// Blocks until the startup indexing is over, passing on its progress.
    fn wait(&self, relay: &dyn Fn(&str, usize, usize)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut seen = None;
        loop {
            if state.0 != seen {
                seen.clone_from(&state.0);
                if let Some((stage, done, total)) = &seen {
                    relay(stage, *done, *total);
                }
            }
            if state.1 {
                return;
            }
            state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

#[derive(Debug)]
struct Server {
    options: Options,
    home: PathBuf,
    cwd: PathBuf,
    engine: Mutex<Option<Arc<Engine>>>,
    warmup: Warmup,
}

impl Server {
    fn new(options: Options, home: PathBuf, cwd: PathBuf, engine: Option<Engine>) -> Self {
        Self {
            options,
            home,
            cwd,
            engine: Mutex::new(engine.map(Arc::new)),
            warmup: Warmup::default(),
        }
    }

    /// The engine, with the model downloaded and verified on first use.
    fn engine(&self) -> Result<Arc<Engine>, String> {
        let mut held = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(engine) = held.as_ref() {
            return Ok(Arc::clone(engine));
        }
        let model = ensure_model(&self.options.storage)?;
        let engine = Arc::new(Engine::new(&self.options.index, Embedder::load(&model)?));
        *held = Some(Arc::clone(&engine));
        Ok(engine)
    }

    /// The best `request.limit` chunks across the projects' folders (only the filtered project's when one is named),
    /// the caller's project boosted.
    fn search(
        &self,
        engine: &Engine,
        request: &Request,
        progress: &dyn Fn(&str, usize, usize),
    ) -> Result<Vec<Hit>, String> {
        let projects = discover(&self.options, &self.home);
        let chosen: Vec<usize> = match request.project.as_deref() {
            Some(filter) => {
                let chosen = filtered(&projects, filter);
                if chosen.is_empty() {
                    let names: Vec<&str> = projects.iter().map(|project| project.name.as_str()).collect();
                    return Err(format!(
                        "No project matches {filter:?}. Projects: {}",
                        names.join(", ")
                    ));
                }
                chosen
            }
            None => (0..projects.len()).collect(),
        };
        let current = project_of(&projects, request.workspace.as_deref().unwrap_or(&self.cwd));
        let mut sources = Vec::new();
        let mut owners = Vec::new();
        for position in chosen {
            let Some(project) = projects.get(position) else {
                continue;
            };
            let weight = if current == Some(position) {
                self.options.current_boost
            } else {
                1.0
            };
            for folder in &self.options.folders {
                let root = project.dir.join(&folder.name);
                if !root.is_dir() {
                    continue;
                }
                sources.push(Source {
                    root,
                    boost: folder.boost * weight,
                    ignores: if folder.name == "." {
                        Ignores::WithParents
                    } else {
                        Ignores::Own
                    },
                });
                owners.push(project.name.clone());
            }
        }
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let limit = request.limit.clamp(1, MAX_LIMIT);
        let found = engine
            .search_sources(&sources, &request.query, limit, self.options.docs_only, progress)
            .map_err(|error| error.to_string())?;
        Ok(found
            .into_iter()
            .map(|found| Hit {
                project: owners.get(found.source).cloned().unwrap_or_default(),
                found,
            })
            .collect())
    }

    /// Builds or refreshes every index before the first search asks.
    fn warm(&self) {
        let request = Request {
            query: "index".to_owned(),
            limit: 1,
            ..Request::default()
        };
        let outcome = self.engine().and_then(|engine| {
            self.search(&engine, &request, &|stage, done, total| {
                self.warmup.publish(stage, done, total);
            })
        });
        self.warmup.finish();
        if let Err(error) = outcome {
            eprintln!("rsemble serve: warming the index failed: {error}");
        }
    }

    fn tool(&self) -> Value {
        json!({
            "name": self.options.tool,
            "description": self.options.description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to find, in natural language or keywords."},
                    "project": {"type": "string", "description": "Only search this project: its name as results show it, or a path inside it."},
                    "workspace": {"type": "string", "description": "Absolute path of your current workspace; its project ranks higher."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": format!("Results to return (default {DEFAULT_LIMIT}).")}
                },
                "required": ["query"]
            }
        })
    }

    /// `tools/call`: the search's results as text, or the failure with `isError`.
    fn call(&self, params: &Value, stdout: &mut dyn Write) -> Value {
        let notifier = Notifier::new(params, stdout);
        let report = |stage: &str, done: usize, total: usize| notifier.report(stage, done, total);
        let arguments = params.get("arguments").cloned().unwrap_or_default();
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let outcome = if params.get("name").and_then(Value::as_str) != Some(self.options.tool.as_str()) {
            Err(format!(
                "Unknown tool; this server has only {}",
                self.options.tool
            ))
        } else if let Some(query) = text("query") {
            let request = Request {
                query,
                project: text("project"),
                workspace: text("workspace").map(PathBuf::from),
                limit: arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_LIMIT, |limit| usize::try_from(limit).unwrap_or(MAX_LIMIT)),
            };
            self.warmup.wait(&report);
            self.engine()
                .and_then(|engine| self.search(&engine, &request, &report))
                .map(|hits| render(&request.query, &hits))
        } else {
            Err("query is required".to_owned())
        };
        let (text, is_error) = match outcome {
            Ok(text) => (text, false),
            Err(error) => (error, true),
        };
        json!({"content": [{"type": "text", "text": text}], "isError": is_error})
    }

    /// The reply to one JSON-RPC message; `None` for a notification.
    fn handle(&self, message: &Value, stdout: &mut dyn Write) -> Option<Value> {
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or_default();
        let result = match message.get("method").and_then(Value::as_str).unwrap_or_default() {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or_else(|| json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "rsemble", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": [self.tool()]})),
            "tools/call" => Ok(self.call(&params, stdout)),
            method => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        })
    }
}

fn render(query: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!("No results for {query:?}.");
    }
    let mut out = format!("{} results for {query:?}:\n", hits.len());
    for (rank, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. [{}] {}:{}-{} (score {:.4})\n{}\n",
            rank + 1,
            hit.project,
            hit.found.file,
            hit.found.start_line,
            hit.found.end_line,
            hit.found.score,
            hit.found.content.trim_end()
        ));
    }
    out
}

/// Serves MCP over `stdin`/`stdout` until `stdin` closes, building the indexes alongside.
pub fn run(options: Options, home: PathBuf, cwd: PathBuf, stdin: &mut dyn BufRead, stdout: &mut dyn Write) {
    serve(&Server::new(options, home, cwd, None), stdin, stdout);
}

fn serve(server: &Server, stdin: &mut dyn BufRead, stdout: &mut dyn Write) {
    std::thread::scope(|scope| {
        scope.spawn(|| server.warm());
        for line in stdin.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(message) => server.handle(&message, stdout),
                Err(error) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": format!("Parse error: {error}")}
                })),
            };
            if let Some(reply) = reply
                && writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err()
            {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use rsemble_core::test_embedder;

    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    fn options(home: &Path, extra: &[&str]) -> Options {
        let mut words = args(&[
            "--storage",
            "~/data",
            "--scan",
            "~/code",
            "--skip",
            "vendor",
            "--project",
            "~",
            "--folder",
            "notes",
            "--folder",
            "docs=0.5",
            "--docs-only",
        ]);
        words.extend(args(extra));
        parse_args(&words, home).unwrap()
    }

    fn engine(storage: &Path) -> Engine {
        Engine::new(
            &storage.join("index"),
            test_embedder(&["deploy", "pipeline", "uploads", "bundle", "bucket"]).unwrap(),
        )
    }

    /// Projects are found at any depth up to the limit, nested ones too, never inside hidden, skipped or searched
    /// folders or past the depth; names come from the scan root, and the home directory is `home`.
    #[test]
    fn projects_are_the_directories_holding_the_first_folder() {
        let home = tempfile::tempdir().unwrap();
        for file in [
            "code/alpha/notes/a.md",
            "code/group/beta/notes/b.md",
            "code/group/notes/g.md",
            "code/.hidden/x/notes/h.md",
            "code/vendor/v/notes/v.md",
            "code/alpha/notes/inner/notes/i.md",
            "code/a/b/c/deep/notes/d.md",
            "code/docs-only/docs/d.md",
            "notes/home.md",
        ] {
            write(home.path(), file, "x");
        }
        let names: Vec<String> = discover(&options(home.path(), &[]), home.path())
            .into_iter()
            .map(|project| project.name)
            .collect();
        assert_eq!(names, ["home", "alpha", "group", "group/beta"]);
    }

    #[test]
    fn a_filter_names_a_project_or_a_path_inside_it() {
        let project = |name: &str, dir: &str| Project {
            name: name.to_owned(),
            dir: PathBuf::from(dir),
            real: PathBuf::from(dir),
        };
        let projects = [
            project("tools", "/c/tools"),
            project("tools/demo", "/c/tools/demo"),
            project("app", "/c/app"),
        ];
        assert_eq!(filtered(&projects, "DEMO"), [1]);
        assert_eq!(filtered(&projects, "tools"), [0]);
        assert_eq!(filtered(&projects, "/c/tools/demo/src/main.rs"), [1]);
        assert_eq!(filtered(&projects, "/c/tools/other"), [0]);
        assert_eq!(filtered(&projects, "ools"), Vec::<usize>::new());
        assert_eq!(project_of(&projects, Path::new("/elsewhere")), None);
    }

    /// The same note in two projects: the caller's ranks first, a filter keeps only the named project, and a
    /// lower-weighted folder ranks below the marker folder; non-documentation files are left out.
    #[test]
    fn the_callers_project_ranks_first_and_a_filter_restricts() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let note = "# Deploy\n\nThe deploy pipeline uploads the bundle to the bucket.\n";
        write(&root, "code/alpha/notes/deploy.md", note);
        write(&root, "code/beta/notes/deploy.md", note);
        write(&root, "code/beta/docs/deploy.md", note);
        write(
            &root,
            "code/beta/docs/deploy.rs",
            "fn deploy_pipeline_bundle_bucket() {}\n",
        );
        let server = Server::new(options(&root, &[]), root.clone(), root.clone(), None);
        let engine = engine(&root.join("data"));
        let search = |workspace: &str, project: Option<&str>| {
            let request = Request {
                query: "deploy pipeline bucket".to_owned(),
                project: project.map(str::to_owned),
                workspace: Some(root.join(workspace)),
                limit: 10,
            };
            server
                .search(&engine, &request, &|_, _, _| {})
                .unwrap()
                .into_iter()
                .map(|hit| {
                    let file = hit.found.file[root.to_string_lossy().len() + 1..].to_owned();
                    (hit.project, file)
                })
                .collect::<Vec<_>>()
        };
        let pair = |project: &str, file: &str| (project.to_owned(), file.to_owned());
        assert_eq!(
            search("code/alpha/src", None),
            [
                pair("alpha", "code/alpha/notes/deploy.md"),
                pair("beta", "code/beta/notes/deploy.md"),
                pair("beta", "code/beta/docs/deploy.md"),
            ]
        );
        assert_eq!(
            search("code/beta", None).first(),
            Some(&pair("beta", "code/beta/notes/deploy.md"))
        );
        assert_eq!(
            search("code/beta", Some("alpha")),
            [pair("alpha", "code/alpha/notes/deploy.md")]
        );
        assert!(
            server
                .search(
                    &engine,
                    &Request {
                        query: "x".to_owned(),
                        project: Some("nope".to_owned()),
                        ..Request::default()
                    },
                    &|_, _, _| {}
                )
                .is_err()
        );
    }

    /// A call that arrives while the startup indexing runs gets its progress, under the call's own token, before the
    /// result; a call without a token gets none.
    #[test]
    fn calls_wait_for_the_startup_index_and_relay_its_progress() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        write(
            &root,
            "code/alpha/notes/deploy.md",
            "# Deploy\n\nDeploy the bundle to the bucket.\n",
        );
        let server = Server::new(
            options(&root, &["--tool", "find"]),
            root.clone(),
            root.clone(),
            Some(engine(&root.join("data"))),
        );
        let mut input = Vec::new();
        for (id, token) in [(1, Some(json!("first"))), (2, None)] {
            let mut params = json!({"name": "find", "arguments": {"query": "deploy bundle"}});
            if let Some(token) = token {
                params["_meta"] = json!({"progressToken": token});
            }
            writeln!(
                input,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
            )
            .unwrap();
        }
        writeln!(
            input,
            "{}",
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
        )
        .unwrap();
        let mut output = Vec::new();
        serve(&server, &mut input.as_slice(), &mut output);
        let messages: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let first = messages.iter().position(|message| message["id"] == 1).unwrap();
        let second = messages.iter().position(|message| message["id"] == 2).unwrap();
        let progress = |range: &[Value]| -> Vec<String> {
            range
                .iter()
                .filter(|message| message["method"] == "notifications/progress")
                .map(|message| {
                    assert_eq!(message["params"]["progressToken"], "first");
                    message["params"]["message"].as_str().unwrap().to_owned()
                })
                .collect()
        };
        assert!(progress(&messages[..first]).contains(&"saving indexes: 1/1".to_owned()));
        assert!(progress(&messages[first..second]).is_empty());
        for id in [1, 2] {
            let reply = messages.iter().find(|message| message["id"] == id).unwrap();
            assert_eq!(reply["result"]["isError"], false);
            let text = reply["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("[alpha]") && text.contains("deploy.md:1-"),
                "{text}"
            );
        }
        let tools = messages.iter().find(|message| message["id"] == 3).unwrap();
        assert_eq!(tools["result"]["tools"][0]["name"], "find");
    }

    #[test]
    fn arguments_are_validated() {
        let home = Path::new("/h");
        let parsed = parse_args(&args(&["--storage", "~/s", "--folder", "notes=2"]), home).unwrap();
        assert_eq!(parsed.storage, PathBuf::from("/h/s"));
        assert_eq!(parsed.index, PathBuf::from("/h/s/index"));
        assert_eq!(
            parsed.folders,
            [Folder {
                name: "notes".to_owned(),
                boost: 2.0
            }]
        );
        let indexed = parse_args(
            &args(&["--storage", "s", "--index", "~/i", "--folder", "."]),
            home,
        )
        .unwrap();
        assert_eq!(indexed.index, PathBuf::from("/h/i"));
        assert!(parse_args(&args(&["--folder", "notes"]), home).is_err());
        assert!(parse_args(&args(&["--storage", "s"]), home).is_err());
        assert!(parse_args(&args(&["--storage", "s", "--folder", "a/b"]), home).is_err());
        assert!(parse_args(&args(&["--storage", "s", "--folder", "a=x"]), home).is_err());
    }
}
