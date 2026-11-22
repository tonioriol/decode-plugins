# Code-search benchmark

Runs [MinishLab Semble](https://github.com/MinishLab/semble)'s own benchmark against Decode Semantic ([`engine/`](../../engine)) and Python Semble on the same corpus. Scoring and queries come unchanged from upstream commit `a1ba6c18`.

## Results

Full corpus, Semble 0.7.3, model `minishlab/potion-code-16M-v2`, same machine:

| | Decode Semantic | Decode Semantic, code only | Python Semble |
| --- | ---: | ---: | ---: |
| NDCG@10 (63 repos, 1,251 queries) | 0.8513 | 0.8553 | 0.8547 |
| Architecture / semantic / symbol | 0.804 / 0.845 / 0.951 | 0.807 / 0.848 / 0.954 | 0.808 / 0.847 / 0.954 |
| Cold index, mean ms (19 repos) | 110.8 | 123.6 | 658.5 |
| Warm query p50, ms | 0.32 | 0.34 | 1.92 |
| Tokens to first relevant hit | 358 | 353 | 353 |
| Recall at 500 / 1,000 / 2,000 tokens | 0.836 / 0.921 / 0.963 | 0.842 / 0.923 / 0.964 | 0.843 / 0.921 / 0.963 |

- Decode Semantic also indexes docs by default. The answers label code only, so a README ranked first costs NDCG; "code only" turns docs off.
- Run on a busy 14-core Apple laptop with 4 threads each. Compare engines only on one machine.
- Decode Semantic's cold index includes writing and reopening its index file (about 3.6 ms); Python Semble builds in memory.
- Timings call the engines in-process, without CLI or MCP overhead, after the model is loaded.

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
