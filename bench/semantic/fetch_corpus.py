#!/usr/bin/env python3
"""Fetch or verify the pinned repositories from MinishLab Semble's benchmark manifest."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "upstream/benchmarks/repos.json"
CACHE = ROOT / "cache"


def run(*args: str, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def head_sha(repo_dir: Path) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo_dir), "rev-parse", "--verify", "HEAD"],
        capture_output=True,
        text=True,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def main() -> None:
    parser = argparse.ArgumentParser(description="Fetch pinned benchmark repositories into cache/.")
    parser.add_argument("--repo", action="append", default=[], help="Limit to one or more repo names.")
    parser.add_argument("--check", action="store_true", help="Verify local checkouts without fetching.")
    args = parser.parse_args()

    if not MANIFEST.is_file():
        raise SystemExit(f"Missing {MANIFEST}; clone upstream Semble first (see README.md).")

    repos = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if args.repo:
        wanted = set(args.repo)
        known = {r["name"] for r in repos}
        unknown = sorted(wanted - known)
        if unknown:
            raise SystemExit(f"Unknown repo(s): {', '.join(unknown)}")
        repos = [r for r in repos if r["name"] in wanted]

    if args.check:
        problems = []
        for r in repos:
            d = CACHE / r["name"]
            if not (d / ".git").exists():
                problems.append(f"{r['name']}: missing checkout at {d}")
            elif (got := head_sha(d)) != r["revision"]:
                problems.append(f"{r['name']}: expected {r['revision']}, found {got}")
        if problems:
            for p in problems:
                print(p, file=sys.stderr)
            raise SystemExit(1)
        print(f"Verified {len(repos)} pinned repositories.")
        return

    for r in repos:
        name, url, revision = r["name"], r["url"], r["revision"]
        d = CACHE / name
        d.mkdir(parents=True, exist_ok=True)
        if not (d / ".git").exists():
            run("git", "-C", str(d), "init", "-q")
            run("git", "-C", str(d), "remote", "add", "origin", url)
        if head_sha(d) != revision:
            print(f"Fetching {name} @ {revision[:12]}", flush=True)
            run("git", "-C", str(d), "-c", "pack.threads=2", "fetch", "--no-tags", "--depth", "1", "origin", revision)
            run("git", "-C", str(d), "checkout", "-q", "--detach", revision)
        if head_sha(d) != revision:
            raise SystemExit(f"{name}: revision mismatch after checkout")
    print(f"Verified {len(repos)} pinned repositories.")


if __name__ == "__main__":
    main()
