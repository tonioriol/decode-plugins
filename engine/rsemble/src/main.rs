//! `rsemble`: semantic code search from the command line, and as an MCP server.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use rsemble::default_storage;
use rsemble::engine::Engine;
use rsemble::mcp::{self, DEFAULT_LIMIT, MAX_LIMIT};
use rsemble::model::ensure_model;
use rsemble::serve;
use rsemble_core::Embedder;
use serde_json::json;

const USAGE: &str = "usage:
  rsemble search <query> [--path <dir>]... [--limit <n>] [--docs-only] [--json] [--storage <dir>] [--index <dir>]
  rsemble mcp [<dir>...] [--docs-only] [--storage <dir>] [--index <dir>]
  rsemble serve --storage <dir> [--index <dir>] [--tool <name>] [--description <text>] [--scan <dir>]...
      [--depth <n>] [--skip <name>]... [--project <dir>]... --folder <name>[=<boost>]... [--docs-only] [--current-boost <x>]
  rsemble model [--storage <dir>]

search   rank the chunks of the files under each --path (default: the current folder) for a query
mcp      serve a stdio MCP server with one `search` tool over the folders given (default: the current folder)
serve    serve a stdio MCP server with one search tool over the same-named --folders of many projects: the
         --project dirs and the dirs under each --scan root that hold the first --folder; the caller's project
         ranks first
model    download and verify the embedding model, then print where it is

Storage (the model) defaults to $SEMBLE_RS_HOME, else $XDG_CACHE_HOME/semble-rs, else ~/.cache/semble-rs. Indexes
go to --index, default <storage>/index (<storage>/docs-index with --docs-only for search and mcp). An index folder
holds one kind of listing; Decode names and writes its indexes the same way, so both can share one.";

#[derive(Debug, Default)]
struct Args {
    words: Vec<String>,
    paths: Vec<PathBuf>,
    limit: Option<usize>,
    docs_only: bool,
    json: bool,
    storage: Option<PathBuf>,
    index: Option<PathBuf>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--path" => parsed.paths.push(PathBuf::from(value("--path")?)),
            "--storage" => parsed.storage = Some(PathBuf::from(value("--storage")?)),
            "--index" => parsed.index = Some(PathBuf::from(value("--index")?)),
            "--limit" => {
                parsed.limit = Some(
                    value("--limit")?
                        .parse()
                        .map_err(|_| "--limit needs a whole number".to_owned())?,
                );
            }
            "--docs-only" => parsed.docs_only = true,
            "--json" => parsed.json = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => parsed.words.push(arg),
        }
    }
    Ok(parsed)
}

fn run() -> Result<(), String> {
    let mut raw = std::env::args().skip(1);
    let command = raw.next().ok_or("missing command")?;
    if command == "serve" {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let options = serve::parse_args(&raw.collect::<Vec<_>>(), &home)?;
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        serve::run(
            options,
            home,
            cwd,
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        );
        return Ok(());
    }
    let args = parse(raw)?;
    let storage = args
        .storage
        .clone()
        .or_else(default_storage)
        .ok_or("no storage folder: pass --storage or set SEMBLE_RS_HOME")?;
    let index = args
        .index
        .clone()
        .unwrap_or_else(|| storage.join(if args.docs_only { "docs-index" } else { "index" }));
    let cwd = || std::env::current_dir().map_err(|error| error.to_string());
    match command.as_str() {
        "model" => {
            println!("{}", ensure_model(&storage)?.display());
            Ok(())
        }
        "mcp" => {
            let roots = if args.words.is_empty() {
                vec![cwd()?]
            } else {
                args.words.iter().map(PathBuf::from).collect()
            };
            let config = mcp::Config {
                storage,
                index,
                roots,
                docs_only: args.docs_only,
            };
            mcp::serve(config, &mut io::stdin().lock(), &mut io::stdout().lock());
            Ok(())
        }
        "search" => {
            let query = args.words.join(" ");
            if query.trim().is_empty() {
                return Err("search needs a query".to_owned());
            }
            let roots = if args.paths.is_empty() {
                vec![cwd()?]
            } else {
                args.paths.clone()
            };
            let model = ensure_model(&storage)?;
            let engine = Engine::new(&index, Embedder::load(&model)?);
            let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
            let found = engine
                .search(&roots, &query, limit, args.docs_only, &|stage, done, total| {
                    if done == total || done == 0 {
                        eprintln!("rsemble: {stage} {done}/{total}");
                    }
                })
                .map_err(|error| error.to_string())?;
            if args.json {
                let hits: Vec<_> = found
                    .iter()
                    .map(|hit| json!({"file": hit.file, "startLine": hit.start_line, "endLine": hit.end_line, "score": hit.score, "content": hit.content}))
                    .collect();
                println!("{}", json!(hits));
            } else {
                println!("{}", mcp::render(&query, &found));
            }
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rsemble: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
