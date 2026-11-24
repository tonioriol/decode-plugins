# Search benchmarks

Decode Semantic started as a Rust port of [MinishLab Semble](https://github.com/MinishLab/semble) ([`engine/`](../../engine)) and has been improved since. This page has three measurements, each tagged with the engine version it comes from:

- speed and memory on real data, against Python Semble 0.7.3;
- ranking quality on Semble's own benchmark, and on questions about notes;
- an earlier in-process run of Semble's benchmark, kept as a dated record.

## Real data: speed and memory

Python Semble 0.7.3 against Decode Semantic, 2026-11-24. Both run as their own stdio MCP server, driven by the same client, with the same model (`minishlab/potion-code-16M-v2`), on one machine. Engine: a build from just before v0.1.2. Each cell reads Python → Decode Semantic (how many times better).

| | Code: 6,968 files, 118 MB | Notes: a 206 MB notes folder | Past chats: 14,562 sessions, 429 MB of text |
| --- | ---: | ---: | ---: |
| Warm search, median ms | 4.70 → 1.43 (3.3×) | 2.24 → 0.67 (3.3×) | 10.44 → 2.61 (4.0×) |
| Cold index and first answer, s | 11.5 → 5.1 (2.3×) | 6.1 → 2.6 (2.3×) | 141.8 → 56.1 (2.5×) |
| Peak memory, MB | 1,033 → 462 (2.2×) | 723 → 313 (2.3×) | 9,810 → 4,747 (2.1×) |
| Idle memory after 150 searches, MB | 839 → 105 (8.0×) | 616 → 82 (7.6×) | 5,185 → 550 (9.4×) |
| Index on disk, MB | 153 → 84 (1.8×) | 75 → 46 (1.6×) | 1,467 → 971 (1.5×) |

- The data is the author's own: a code repository, his project notes, and his past agent chats exported as text (user and assistant text only). The queries are 50 real past searches per corpus, a seeded sample.
- Code and notes: median of 3 runs. Past chats: 1 run.
- Apple M4 Pro, 14 cores, 24 GB, shared with other work (load 7–22 during the runs), 4 compute threads and `nice -n 10` for both engines, one process at a time.
- Cold: empty index folder, spawn the server, wall time until the first answer. Warm: 100 searches after a warm-up pass. Memory is the macOS physical footprint (Activity Monitor's "Memory") summed over the process tree. Idle is measured 3 s after the last search.
- Python was run as `semble[mcp]==0.7.3` with `--content code docs config` on code and `--content docs` on notes and chats.
- Warm search favours Python slightly: Decode Semantic checks the folder for changes on every search, inside its timings, while Python's server skips revalidation for a while.
- This driver reads private data and is not in this folder. Speed was not re-measured on v0.1.3, which changes ranking only; the speed check made while developing it, on two corpora, moved warm search from 0.851 to 0.863 ms (notes) and from 0.502 to 0.523 ms (abseil), within run-to-run noise.

## Code quality: Semble's benchmark

[Semble's own benchmark](https://github.com/MinishLab/semble/tree/a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d/benchmarks), 63 repositories, 1,251 queries, 19 languages. Scoring and queries come unchanged from upstream commit `a1ba6c18`.

| NDCG@10 | Score |
| --- | ---: |
| Python Semble 0.7.3 | 0.8547 |
| Decode Semantic v0.1.3 | 0.8539 |
| Decode Semantic v0.1.2 | 0.8514 |
| Decode Semantic, engine of 2026-11-19 | 0.8513 |

- Decode Semantic indexes docs by default. The answers label code only, so a README ranked first costs NDCG; with docs turned off it scored 0.8553 on the engine of 2026-11-19 (above Semble's 0.8547). That code-only score was not re-measured on v0.1.3.
- Python Semble's score is from the run of 2026-11-19; the corpus, queries and Semble version are unchanged.

## Notes ranking

Questions about the author's own notes, each labelled with the note (and, where known, the lines) that answers it, scored with span-aware NDCG@10. 117 development queries, used while tuning, and 83 blind holdout queries, held back from it. The notes and questions are private.

| NDCG@10 | Decode Semantic v0.1.2 | Decode Semantic v0.1.3 |
| --- | ---: | ---: |
| 117 development queries | 0.68 | 0.80 |
| 83 blind holdout queries | 0.78 | 0.86 |

v0.1.3 adds each Markdown chunk's heading chain to what is embedded and keyword-indexed, weighs docs a little below code when both are indexed, and tunes ranking for folders that hold only notes.

## Earlier run: in-process harness, engine of 2026-11-19

Full corpus, Semble 0.7.3, same model, same machine. Not re-run on v0.1.3: the full corpus needs about 3 GB of disk; the commands below repeat it. Where the sections above measure the same thing (NDCG@10, speed), they supersede this table.

| | Decode Semantic | Decode Semantic, code only | Python Semble |
| --- | ---: | ---: | ---: |
| NDCG@10 (63 repos, 1,251 queries) | 0.8513 | 0.8553 | 0.8547 |
| Architecture / semantic / symbol | 0.804 / 0.845 / 0.951 | 0.807 / 0.848 / 0.954 | 0.808 / 0.847 / 0.954 |
| Cold index, mean ms (19 repos) | 110.8 | 123.6 | 658.5 |
| Warm query p50, ms | 0.32 | 0.34 | 1.92 |
| Tokens to first relevant hit | 358 | 353 | 353 |
| Recall at 500 / 1,000 / 2,000 tokens | 0.836 / 0.921 / 0.963 | 0.842 / 0.923 / 0.964 | 0.843 / 0.921 / 0.963 |

- Run on a busy 14-core Apple laptop with 4 threads each. Compare engines only on one machine.
- Decode Semantic's cold index includes writing and reopening its index file (about 3.6 ms); Python Semble builds in memory.
- Timings call the engines in-process, without CLI or MCP overhead, after the model is loaded, on small repositories. The real-data table above measures the servers as agents use them.

## Modes

- `quality`: NDCG@10 over all queries, per language and category.
- `speed`: cold index time and warm query p50 on 19 repos, CPU only.
- `token`: tokens read before the first relevant hit, and recall at token budgets.
- `ablations`: Python Semble only.

## Run

Needs Rust 1.89+, Python 3.10–3.13, Git, and about 3 GB for the full corpus. From this folder:

```bash
git init upstream
git -C upstream remote add origin https://github.com/MinishLab/semble.git
git -C upstream fetch --depth 1 origin a1ba6c18a4b0a58a3d1a15217d64bc92678d1d9d
git -C upstream checkout --detach FETCH_HEAD

cargo build --release --manifest-path harness/Cargo.toml
./harness/target/release/decode-semantic-bench model ./model   # downloads and checksums the model

python3 -m venv .venv && .venv/bin/pip install -r requirements.txt
python3 fetch_corpus.py                      # or --repo axios --repo flask --repo rack for a quick run

export SEMBLE_INDEX_WORKERS=4 RAYON_NUM_THREADS=4 OMP_NUM_THREADS=4 OPENBLAS_NUM_THREADS=4 \
  MKL_NUM_THREADS=4 VECLIB_MAXIMUM_THREADS=4 TOKENIZERS_PARALLELISM=false MPLBACKEND=Agg
for engine in decode-semantic decode-semantic-code python; do
  for mode in quality speed token; do .venv/bin/python bench.py "$engine" "$mode"; done
done
```

Results go to `results/<engine>-<mode>.json`. Runs with `--repo` write `-subset` files. `python token` also needs `rg`. Delete `harness/target/`, `cache/`, `model/`, `.venv/` and `upstream/` afterwards to free the space.
