# Semantic code-search benchmark

A harness and scripts to run [MinishLab Semble](https://github.com/MinishLab/semble)'s official code-search benchmark against Decode Semantic ([`engine/`](../../engine)) and stock Python Semble on the same pinned corpus.

## What is measured

All scoring and task definitions come directly from upstream Semble at commit `a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d`.

- **Quality (`quality`):** 1,251 annotated queries across 63 pinned repositories and 19 languages, split into `architecture`, `semantic`, and `symbol` queries. Scored by upstream `benchmarks/run_benchmark.py` using top-10 span-aware NDCG, averaged per repository and then averaged across repositories overall, by language, and by category.
- **Speed (`speed`):** upstream's 19 representative repositories (one per language), CPU only. Measures cold index build time (no prior index; OS page cache is not flushed) and warm query p50 pooled across 5 consecutive runs per query. Model loading is outside the timer for both engines. Decode Semantic's cold index time includes serializing and reopening the index file; Python Semble builds in memory.
- **Token efficiency (`token`):** top-50 retrieval across all 1,251 queries scored with `cl100k_base` tokenization, whole chunks, span overlap, and upstream's 32,000-token cap when no relevant hit is found. Reports expected tokens to the first relevant hit and recall at fixed token budgets (500 to 32,000).
- **File scope (`decode-semantic` vs `decode-semantic-code`):** Decode Semantic indexes documentation files (such as Markdown) alongside source code by default, while Python Semble indexes source code only. Because the benchmark annotations label code spans, a documentation chunk ranked above code lowers NDCG even when it helps answer the query. `bench.py` supports both `decode-semantic` (production default, with docs) and `decode-semantic-code` (code-only control, excluding doc languages, with no model or ranking changes).

## What is not measured

- **End-to-end CLI or MCP overhead:** the Rust harness ([`harness/src/main.rs`](harness/src/main.rs)) links `decode-semantic` and `decode-semantic-core` directly and keeps the opened index in memory across queries, so warm query timings exclude process startup, directory rescans, and JSON-RPC framing.
- **Model download and load time:** both engines load the embedding model before speed timings start.
- **Documentation usefulness:** ground-truth annotations label code files and line spans only.
- **Retrieval ablations on Decode Semantic:** Decode Semantic exposes adaptive hybrid search only (no public `alpha` or rerank toggle), so `ablations` mode runs on `python` only.
- **Third-party baselines:** other search tools from upstream's README (CodeRankEmbed, ColGREP, BM25-only tools, ripgrep, etc.) are not installed or run by this kit.
- **Cross-host performance:** index and query latencies depend on hardware, thread limits, and background load; compare engines only on the same host under the same settings.

## Prerequisites

- Rust toolchain supporting [`engine/`](../../engine) (edition 2024, Rust 1.89+)
- Python 3.10–3.13 (tested with Python 3.12) and Git
- Network access on first setup to clone upstream Semble, fetch the pinned repositories (~2–3 GB shallow checkouts for all 63 repos), and download the verified `minishlab/potion-code-16M-v2` model (~32 MB)
- Optional: `ripgrep` (`rg`) if you run `python token` (upstream's `token_efficiency.py` runs its `grep-kw+read` baseline alongside Python Semble)

## Set up and run

All commands below run from `bench/semantic/`.

### 1. Clone upstream Semble at the pinned commit

```bash
git init upstream
git -C upstream remote add origin https://github.com/MinishLab/semble.git
git -C upstream fetch --depth 1 origin a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d
git -C upstream checkout --detach a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d
```

### 2. Build the Rust harness and fetch the embedding model

```bash
cargo build --release --manifest-path harness/Cargo.toml
./harness/target/release/decode-semantic-bench model ./model
```

The `model` subcommand downloads `minishlab/potion-code-16M-v2` at revision `e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b` into `model/semble-model-e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b` and verifies the SHA-256 checksum of every file (`model.safetensors`: `75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c`).

### 3. Create the Python environment

```bash
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

### 4. Fetch the pinned corpus

For a quick smoke check on three small repositories:

```bash
python3 fetch_corpus.py --repo axios --repo flask --repo rack
```

For the full 63-repository corpus (1,251 queries):

```bash
python3 fetch_corpus.py
python3 fetch_corpus.py --check
```

### 5. Run the benchmarks

Pin worker and thread counts so both engines share the same parallelism budget:

```bash
export SEMBLE_INDEX_WORKERS=4 RAYON_NUM_THREADS=4 TOKENIZERS_PARALLELISM=false MPLBACKEND=Agg
export OPENBLAS_NUM_THREADS=4 OMP_NUM_THREADS=4 MKL_NUM_THREADS=4 VECLIB_MAXIMUM_THREADS=4
```

Smoke run on the three-repo subset:

```bash
.venv/bin/python bench.py decode-semantic quality --repo axios --repo flask --repo rack
.venv/bin/python bench.py decode-semantic-code quality --repo axios --repo flask --repo rack
.venv/bin/python bench.py decode-semantic speed --repo axios --repo flask --repo rack
.venv/bin/python bench.py decode-semantic token --repo axios --repo flask --repo rack
.venv/bin/python bench.py python quality --repo axios --repo flask --repo rack
.venv/bin/python bench.py python speed --repo axios --repo flask --repo rack
```

Full 63-repo suite:

```bash
for engine in decode-semantic decode-semantic-code python; do
  for mode in quality speed token; do
    .venv/bin/python bench.py "$engine" "$mode"
  done
done
.venv/bin/python bench.py python ablations
```

Full runs write `results/<engine>-<mode>.json` and per-repo/per-query timing logs in `results/<engine>-<mode>-evidence.jsonl`. Subset runs (`--repo`) print summaries to stderr/stdout and write `-subset` files so they never overwrite full-corpus results.

To free disk space after a run, remove `harness/target/`, `cache/`, `model/`, `.venv/`, and `upstream/` (all ignored by [`.gitignore`](.gitignore)).

## Reference results

Full-corpus run against upstream MinishLab Semble `0.7.3` (`a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d`) and `minishlab/potion-code-16M-v2` (`e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b`):

| Metric | Decode Semantic (default, with docs) | Decode Semantic (code only) | Python Semble (same host) |
| --- | ---: | ---: | ---: |
| NDCG@10 (63 repos, 1,251 queries) | 0.8513 | 0.8553 | 0.8547 |
| Architecture / Semantic / Symbol NDCG@10 | 0.804 / 0.845 / 0.951 | 0.807 / 0.848 / 0.954 | 0.808 / 0.847 / 0.954 |
| Mean cold index (ms, 19 repos) | 110.8 | 123.6 | 658.5 |
| Warm query p50 (ms, 19 repos) | 0.32 | 0.34 | 1.92 |
| Expected tokens to first hit (32k cap) | 358 | 353 | 353 |
| Recall @ 500 tokens | 0.836 | 0.842 | 0.843 |
| Recall @ 1,000 tokens | 0.921 | 0.923 | 0.921 |
| Recall @ 2,000 tokens | 0.963 | 0.964 | 0.963 |

Caveats:

- **Host and load:** the reference run executed sequentially on a shared, busy 14-core ARM laptop (macOS) under `nice -n 10` with 4 index workers and a 4-thread native pool cap (1-minute load averages were ~7–18 during the speed passes). On a quieter machine both engines are faster (upstream's published Python Semble speed figures on its own host are 240.3 ms cold index and 0.69 ms warm p50, with 0.8546 NDCG@10). Treat latencies and speedups as same-host observations, not cross-host guarantees.
- **Persistence in cold index:** Decode Semantic's cold index timing includes writing and reopening the persisted index file (~3.6 ms mean in the reference run); Python Semble's upstream speed runner builds in memory.
- **Docs vs code-only:** the 0.0040 NDCG@10 difference between `decode-semantic` (0.8513) and `decode-semantic-code` (0.8553) comes from documentation files (such as READMEs) ranking above annotated code spans on code-only ground truth.
