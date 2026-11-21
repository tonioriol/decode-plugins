#!/usr/bin/env python3
"""Run MinishLab Semble's benchmark suite with Decode Semantic or stock Python Semble."""
import argparse
from collections import Counter
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
UPSTREAM = ROOT / "upstream"
DEFAULT_MODEL_REV = "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b"
DEFAULT_MODEL_DIR = ROOT / "model" / f"semble-model-{DEFAULT_MODEL_REV}"
DEFAULT_HARNESS = ROOT / "harness/target/release/decode-semantic-bench"

sys.path.insert(0, str(UPSTREAM))
sys.path.insert(0, str(UPSTREAM / "src"))

from benchmarks import data  # noqa: E402

data.BENCH_ROOT = ROOT / "cache"

ENGINE = ""
MODE = ""
MODEL_DIR: Path | None = None
HARNESS_BIN = DEFAULT_HARNESS
SUBSET = False
EVIDENCE = None


def event(**fields) -> None:
    if EVIDENCE is not None:
        print(json.dumps(fields), file=EVIDENCE, flush=True)


def save_results(_method: str, payload: dict) -> Path:
    payload = dict(payload)
    payload["upstream_commit"] = data.current_sha()
    payload["engine"] = ENGINE
    payload["model"] = "minishlab/potion-code-16M-v2"
    if MODE == "speed":
        for row in payload["results"]:
            row["tool"] = ENGINE
        payload["summary"][ENGINE] = payload["summary"].pop("semble")
    if MODE == "token" and ENGINE != "python":
        for levels in (payload["recall_at_budget"], payload["plot"]["recall"], payload["first_hit_reduction"]):
            levels[ENGINE] = levels.pop("semble")
    suffix = "-subset" if SUBSET else ""
    path = ROOT / "results" / f"{ENGINE}-{MODE}{suffix}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return path


class RustIndex:
    """Adapter exposing the decode-semantic-bench child process through SembleIndex's interface."""

    current = None

    def __init__(self, path: Path) -> None:
        self.path = path
        tmp_dir = ROOT / "tmp"
        tmp_dir.mkdir(parents=True, exist_ok=True)
        self.persisted = tmp_dir / "bench-index.bin"
        self.child = subprocess.Popen(
            [str(HARNESS_BIN), str(path), str(MODEL_DIR), str(self.persisted)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            self.stats = self.recv()
        except BaseException:
            self.close()
            raise
        self.chunks = range(self.stats["chunks"])
        event(kind="index", repo=path.name, **self.stats)

    @classmethod
    def from_path(cls, path: Path, **_kwargs) -> "RustIndex":
        cls.clear()
        cls.current = cls(path)
        return cls.current

    @classmethod
    def clear(cls) -> None:
        if cls.current is not None:
            cls.current.close()
            cls.current = None

    def recv(self) -> dict:
        assert self.child.stdout is not None
        line = self.child.stdout.readline()
        if not line:
            raise RuntimeError(f"decode-semantic-bench exited with status {self.child.wait()} indexing {self.path}")
        return json.loads(line)

    def close(self) -> None:
        if self.child.stdin is not None:
            self.child.stdin.close()
        if self.child.stdout is not None:
            self.child.stdout.close()
        rc = self.child.wait()
        self.persisted.unlink(missing_ok=True)
        if rc:
            raise RuntimeError(f"decode-semantic-bench exited with status {rc} for {self.path}")

    def retrieve(self, query: str, top_k: int, repeats: int) -> dict:
        assert self.child.stdin is not None
        self.child.stdin.write(json.dumps({"query": query, "limit": top_k, "repeats": repeats}) + "\n")
        self.child.stdin.flush()
        result = self.recv()
        event(kind="query", repo=self.path.name, query=query, top_k=top_k, timings=result["timings"])
        return result

    def search(self, query: str, *, top_k: int, alpha=None, rerank: bool = True):
        from semble.types import Chunk, SearchResult

        if alpha is not None or not rerank:
            raise ValueError("decode-semantic only exposes adaptive hybrid search")
        return [
            SearchResult(Chunk(**{k: v for k, v in row.items() if k != "score"}), row["score"])
            for row in self.retrieve(query, top_k, 1)["hits"]
        ]


def rust_speed(spec, tasks):
    idx = RustIndex.from_path(spec.benchmark_dir)
    measures = [t["ms"] for task in tasks for t in idx.retrieve(task.query, 10, 5)["timings"]]
    return idx.stats["index_ms"], idx, tuple(measures)


def load_speed():
    # Upstream imports sentence_transformers at module load even for --tools semble.
    # Keep its logic intact and defer that optional import to the CodeRankEmbed branch.
    path = UPSTREAM / "benchmarks/speed_benchmark.py"
    spec = importlib.util.spec_from_file_location("benchmarks.speed_benchmark", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    source = path.read_text(encoding="utf-8")
    target_import = "from sentence_transformers import SentenceTransformer\n"
    anchor = '    if "coderankembed" in tools:'
    assert source.count(target_import) == 1 and source.count(anchor) == 1
    patched = (
        "from __future__ import annotations\n"
        + source.replace(target_import, "").replace(
            anchor, anchor + "\n        from sentence_transformers import SentenceTransformer"
        )
    )
    exec(compile(patched, str(path), "exec"), module.__dict__)
    return module


def main() -> None:
    global ENGINE, MODE, MODEL_DIR, HARNESS_BIN, SUBSET, EVIDENCE
    parser = argparse.ArgumentParser(description="Run Semble benchmarks for Decode Semantic or Python Semble.")
    parser.add_argument("engine", choices=["decode-semantic", "decode-semantic-code", "python"])
    parser.add_argument("mode", choices=["quality", "speed", "token", "ablations"])
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR, help="Path to the verified model folder.")
    parser.add_argument("--harness", type=Path, default=DEFAULT_HARNESS, help="Path to decode-semantic-bench binary.")
    parser.add_argument("--repo", action="append", default=[], help="Limit to one or more repo names.")
    parser.add_argument("--verbose", action="store_true", help="Print per-query scores in quality mode.")
    args = parser.parse_args()

    ENGINE = args.engine
    MODE = args.mode
    MODEL_DIR = args.model_dir.resolve()
    HARNESS_BIN = args.harness.resolve()
    SUBSET = bool(args.repo)

    if ENGINE != "python":
        if MODE == "ablations":
            raise SystemExit("ablations mode is only supported for python (decode-semantic has no alpha/rerank switch).")
        if not HARNESS_BIN.is_file():
            raise SystemExit(f"Missing harness binary at {HARNESS_BIN}; build it first (see README.md).")
        if not MODEL_DIR.is_dir():
            raise SystemExit(f"Missing model directory at {MODEL_DIR}; download it with decode-semantic model first.")
        if ENGINE == "decode-semantic-code":
            os.environ["DECODE_SEMANTIC_BENCH_CODE_ONLY"] = "1"
        else:
            os.environ.pop("DECODE_SEMANTIC_BENCH_CODE_ONLY", None)

    os.chdir(UPSTREAM)
    specs = data.available_repo_specs()
    tasks = data.load_tasks(specs)
    if not SUBSET and (len(specs) != 63 or len(tasks) != 1251):
        raise SystemExit(
            f"Expected 63 repos and 1,251 queries in {data.BENCH_ROOT}, found {len(specs)} repos and {len(tasks)} queries. "
            "Run fetch_corpus.py for the full corpus or pass --repo <name> for a subset."
        )

    repo_flags: list[str] = []
    for name in args.repo:
        repo_flags.extend(["--repo", name])

    suffix = "-subset" if SUBSET else ""
    evidence_path = ROOT / "results" / f"{ENGINE}-{MODE}{suffix}-evidence.jsonl"
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    with evidence_path.open("w", encoding="utf-8") as EVIDENCE:
        selected_tasks = [t for t in tasks if not args.repo or t.repo in args.repo]
        event(
            kind="corpus",
            engine=ENGINE,
            mode=MODE,
            n_repos=len({t.repo for t in selected_tasks}),
            n_queries=len(selected_tasks),
            by_language=Counter(t.language for t in selected_tasks),
            by_category=Counter(t.category for t in selected_tasks),
            started_unix=time.time(),
        )
        try:
            if MODE == "quality":
                from benchmarks import run_benchmark as module

                argv = [*repo_flags, *(["--verbose"] if args.verbose else [])]
            elif MODE == "ablations":
                from benchmarks.baselines import ablations as module

                argv = [*repo_flags]
            elif MODE == "speed":
                module = load_speed()
                if ENGINE != "python":
                    module._bench_semble = rust_speed
                    module.load_model = lambda *_a, **_kw: None
                argv = ["--tools", "semble", *repo_flags]
            else:
                from benchmarks import token_efficiency as module

                if ENGINE != "python":
                    module.load_model = lambda *_a, **_kw: None
                    module._retrieval_units_for_task = lambda index, task, repo_dir: [
                        ("semble", module._semble_units(index, task.query))
                    ]
                argv = ["recall", "--no-plot", *repo_flags]

            module.save_results = save_results
            if ENGINE != "python":
                module.SembleIndex = RustIndex
            sys.argv = [sys.argv[0], *argv]
            module.main()
        finally:
            RustIndex.clear()
            event(kind="end", finished_unix=time.time())


if __name__ == "__main__":
    main()
