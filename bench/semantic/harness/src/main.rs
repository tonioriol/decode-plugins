use decode_semantic::files::{self, Ignores};
use decode_semantic_core::{
    Embedder, Exclusions, FileStamp, PreparedFile, SearchSource, SourceIndex, search,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

fn worker_count() -> usize {
    std::env::var("SEMBLE_INDEX_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 3 && args[1] == "model" {
        let dir = decode_semantic::model::ensure_model(Path::new(&args[2]))
            .map_err(|err| io::Error::other(err))?;
        println!("{}", dir.display());
        return Ok(());
    }
    if args.len() != 4 {
        return Err(
            "usage:\n  decode-semantic-bench model <storage-dir>\n  decode-semantic-bench <repo-dir> <model-dir> <index-file>"
                .into(),
        );
    }
    let root = Path::new(&args[1]);
    let embedder = Embedder::load(Path::new(&args[2]))?;
    let persisted = Path::new(&args[3]);
    let code_only = std::env::var_os("DECODE_SEMANTIC_BENCH_CODE_ONLY")
        .is_some_and(|v| !v.is_empty() && v != "0");
    let workers = worker_count();

    let started = Instant::now();
    let listing: Vec<(String, FileStamp)> =
        files::list(root, &Exclusions::new(&[])?, false, Ignores::WithParents)?
            .into_iter()
            .filter(|(path, _)| {
                !code_only
                    || !decode_semantic_core::detect_language(path)
                        .is_some_and(decode_semantic_core::is_doc_language)
            })
            .collect();

    let mut coverage = BTreeMap::new();
    let mut index = SourceIndex::new(embedder.dim());
    // Preserve sorted file order while preparing files across up to `workers` threads.
    for batch in listing.chunks(1024) {
        let chunk_size = batch.len().div_ceil(workers).max(1);
        let ready = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .chunks(chunk_size)
                .map(|slice| {
                    let embedder = &embedder;
                    scope.spawn(move || {
                        slice
                            .iter()
                            .map(|(path, _)| {
                                fs::read(root.join(path)).map(|bytes| {
                                    PreparedFile::new(
                                        embedder,
                                        path,
                                        &String::from_utf8_lossy(&bytes),
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("prepare worker"))
                .collect::<Result<Vec<_>, _>>()
        })?;
        for ((path, stamp), prepared) in batch.iter().zip(ready.into_iter().flatten()) {
            coverage.insert(path, prepared.chunk_count());
            index.upsert(path, *stamp, prepared);
        }
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;

    let persist_started = Instant::now();
    let mut writer = BufWriter::new(File::create(persisted)?);
    index.write_to(&mut writer)?;
    writer.flush()?;
    drop(writer);
    index = SourceIndex::open(Arc::new(File::open(persisted)?), embedder.dim())?;
    let persist_ms = persist_started.elapsed().as_secs_f64() * 1000.0;

    let source = SearchSource {
        name: "repo",
        index: &index,
        boost: 1.0,
        path_prefix: None,
    };
    let mut out = BufWriter::new(io::stdout().lock());
    writeln!(
        out,
        "{}",
        json!({
            "build_ms": build_ms,
            "persist_ms": persist_ms,
            "index_ms": build_ms + persist_ms,
            "chunks": index.chunk_count(),
            "files": coverage,
        })
    )?;
    out.flush()?;

    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let query = request["query"].as_str().ok_or("missing query")?;
        let top_k = usize::try_from(request["limit"].as_u64().ok_or("missing limit")?)?;
        let repeats = request["repeats"].as_u64().ok_or("missing repeats")?;
        let mut timings = Vec::new();
        let mut hits = Vec::new();
        for _ in 0..repeats {
            let started = Instant::now();
            let current = search(&embedder, std::slice::from_ref(&source), query, top_k)?;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            timings.push(json!({ "ms": ms }));
            hits = current;
        }
        let hits: Vec<_> = hits
            .into_iter()
            .map(|h| {
                json!({
                    "file_path": h.path,
                    "start_line": h.start_line,
                    "end_line": h.end_line,
                    "content": h.content,
                    "score": h.score,
                })
            })
            .collect();
        writeln!(out, "{}", json!({ "hits": hits, "timings": timings }))?;
        out.flush()?;
    }
    Ok(())
}
