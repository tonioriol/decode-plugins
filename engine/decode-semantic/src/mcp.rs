//! A stdio MCP server with one tool, `search`: natural-language or keyword code search over the folders it was
//! started with, or over one folder the call names. Indexes persist under the storage folder and are brought up
//! to date with their files before each search; the model is downloaded there on first use. While the first
//! search builds an index, a client that sent a progress token receives `notifications/progress`.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use decode_semantic_core::Embedder;
use serde_json::{Value, json};

use crate::engine::{Engine, Found};
use crate::model::ensure_model;

/// Results a search returns when the call gives no limit.
pub const DEFAULT_LIMIT: usize = 10;
/// The most results one search returns.
pub const MAX_LIMIT: usize = 50;

/// What the server searches and where it keeps its state.
#[derive(Debug, Clone)]
pub struct Config {
    /// Model folder.
    pub storage: PathBuf,
    /// Index folder (see [`Engine::new`]).
    pub index: PathBuf,
    /// Folders searched when a call names none.
    pub roots: Vec<PathBuf>,
    /// Index documentation files only.
    pub docs_only: bool,
}

#[derive(Debug)]
struct Server {
    config: Config,
    engine: Mutex<Option<Arc<Engine>>>,
}

/// Renders the results of one search as text.
pub fn render(query: &str, found: &[Found]) -> String {
    if found.is_empty() {
        return format!("No results for {query:?}.");
    }
    let mut out = format!("{} results for {query:?}:\n", found.len());
    for (rank, hit) in found.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {}:{}-{} (score {:.4})\n{}\n",
            rank + 1,
            hit.file,
            hit.start_line,
            hit.end_line,
            hit.score,
            hit.content.trim_end()
        ));
    }
    out
}

/// Emits `notifications/progress` for one tool call, when the client asked for them.
pub(crate) struct Notifier<'a> {
    token: Option<Value>,
    sequence: Cell<u64>,
    last_step: Cell<usize>,
    stdout: RefCell<&'a mut dyn Write>,
}

impl<'a> Notifier<'a> {
    /// The notifier of the call with `params`, writing to `stdout`.
    pub(crate) fn new(params: &Value, stdout: &'a mut dyn Write) -> Self {
        Self {
            token: params
                .pointer("/_meta/progressToken")
                .filter(|token| token.is_string() || token.is_number())
                .cloned(),
            sequence: Cell::new(0),
            last_step: Cell::new(0),
            stdout: RefCell::new(stdout),
        }
    }

    /// A notification per percent of `total` at most, and always the last one.
    pub(crate) fn report(&self, stage: &str, done: usize, total: usize) {
        let Some(token) = &self.token else { return };
        let step = (done * 100).checked_div(total).unwrap_or(0);
        if done != total && step == self.last_step.get() && self.sequence.get() > 0 {
            return;
        }
        self.last_step.set(step);
        let sequence = self.sequence.get();
        self.sequence.set(sequence + 1);
        let message = json!({
            "jsonrpc": "2.0", "method": "notifications/progress",
            "params": {"progressToken": token, "progress": sequence, "message": format!("{stage}: {done}/{total}")}
        });
        let mut stdout = self.stdout.borrow_mut();
        drop(writeln!(stdout, "{message}").and_then(|()| stdout.flush()));
    }
}

impl Server {
    /// The engine, with the model downloaded and verified on first use.
    fn engine(&self) -> Result<Arc<Engine>, String> {
        let mut held = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(engine) = held.as_ref() {
            return Ok(Arc::clone(engine));
        }
        let model = ensure_model(&self.config.storage)?;
        let engine = Arc::new(Engine::new(&self.config.index, Embedder::load(&model)?));
        *held = Some(Arc::clone(&engine));
        Ok(engine)
    }

    fn tool() -> Value {
        json!({
            "name": "search",
            "description": "Semantic code search: find code by what it does, in natural language or keywords or a symbol name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to find."},
                    "path": {"type": "string", "description": "Absolute path of one folder to search instead of the server's folders."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": format!("Results to return (default {DEFAULT_LIMIT}).")}
                },
                "required": ["query"]
            }
        })
    }

    fn call(&self, params: &Value, stdout: &mut dyn Write) -> Value {
        let notifier = Notifier::new(params, stdout);
        let arguments = params.get("arguments").cloned().unwrap_or_default();
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let outcome = if params.get("name").and_then(Value::as_str) != Some("search") {
            Err("Unknown tool; this server has only search".to_owned())
        } else if let Some(query) = text("query") {
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_LIMIT, |limit| usize::try_from(limit).unwrap_or(MAX_LIMIT))
                .clamp(1, MAX_LIMIT);
            let roots =
                text("path").map_or_else(|| self.config.roots.clone(), |path| vec![PathBuf::from(path)]);
            self.engine().and_then(|engine| {
                engine
                    .search(
                        &roots,
                        &query,
                        limit,
                        self.config.docs_only,
                        &|stage, done, total| {
                            notifier.report(stage, done, total);
                        },
                    )
                    .map_err(|error| error.to_string())
                    .map(|found| render(&query, &found))
            })
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
                "serverInfo": {"name": "decode-semantic", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": [Self::tool()]})),
            "tools/call" => Ok(self.call(&params, stdout)),
            method => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        })
    }
}

/// Serves MCP over `stdin`/`stdout` (one JSON message per line) until `stdin` closes.
pub fn serve(config: Config, stdin: &mut dyn BufRead, stdout: &mut dyn Write) {
    serve_with(
        Server {
            config,
            engine: Mutex::default(),
        },
        stdin,
        stdout,
    );
}

fn serve_with(server: Server, stdin: &mut dyn BufRead, stdout: &mut dyn Write) {
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
}

/// A server whose model is already loaded, for tests.
#[cfg(test)]
fn serve_with_engine(config: Config, engine: Engine, input: &str) -> Vec<Value> {
    let server = Server {
        config,
        engine: Mutex::new(Some(Arc::new(engine))),
    };
    let mut output = Vec::new();
    serve_with(server, &mut std::io::Cursor::new(input.to_owned()), &mut output);
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use decode_semantic_core::test_embedder;

    use super::*;

    #[test]
    fn answers_initialize_lists_the_tool_and_searches_with_progress() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("deploy.md"),
            "# Deploy\n\nDeploy the bundle to the bucket.\n",
        )
        .unwrap();
        let config = Config {
            storage: storage.path().to_owned(),
            index: storage.path().join("index"),
            roots: vec![project.path().to_owned()],
            docs_only: false,
        };
        let engine = Engine::new(
            storage.path(),
            test_embedder(&["deploy", "bundle", "bucket"]).unwrap(),
        );
        let input = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "search", "arguments": {"query": "deploy bundle bucket"}, "_meta": {"progressToken": "p"}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "search", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}),
        ]
        .iter()
        .map(|message| format!("{message}\n"))
        .collect::<String>();

        let replies = serve_with_engine(config, engine, &input);

        let by_id = |id: u64| replies.iter().find(|reply| reply["id"] == id).unwrap();
        assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "decode-semantic");
        assert_eq!(by_id(2)["result"]["tools"][0]["name"], "search");
        let text = by_id(3)["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("deploy.md:1-"), "{text}");
        assert_eq!(by_id(3)["result"]["isError"], false);
        let progress: Vec<&Value> = replies
            .iter()
            .filter(|reply| reply["method"] == "notifications/progress")
            .collect();
        assert!(
            progress
                .last()
                .is_some_and(|last| last["params"]["message"] == "saving indexes: 1/1"),
            "{progress:?}"
        );
        assert_eq!(by_id(4)["result"]["isError"], true);
        assert_eq!(by_id(5)["error"]["code"], -32601);
    }
}
