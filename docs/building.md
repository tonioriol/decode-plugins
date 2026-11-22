# Building and releasing the engine

The plugins run `decode-semantic`, built from [`engine/`](../engine):

- `decode-semantic-core`: chunking, embeddings, hybrid retrieval and ranking (a Rust port of [Semble](https://github.com/MinishLab/semble)).
- `decode-semantic`: file listing, persisted indexes, the CLI and the `serve` MCP server.

Both are MIT; see their `LICENSE` and `NOTICE`.

## Source

The crates live in the Decode repository and are copied here, with a lock file derived from Decode's:

```bash
python3 scripts/sync-engine.py --decode /path/to/decode --rev HEAD
(cd engine && cargo test --locked)
```

Decode and `decode-semantic` read and write the same index files.

## Build

```bash
cd engine
cargo build --locked --release --bin decode-semantic
```

## Release

1. Bump the version in both plugin manifests (`.claude-plugin` and `.codex-plugin`, including the Codex cache path), the marketplace `metadata.version`, and `release=` in [`scripts/decode-semantic.sh`](../scripts/decode-semantic.sh).
2. Run `python3 scripts/sync-launchers.py` and the tests.
3. Commit, tag `vX.Y.Z` and push the tag.

[`release.yml`](../.github/workflows/release.yml) then tests the engine, builds it for macOS (arm64, x86_64) and Linux (x86_64, aarch64), and publishes the binaries with `LICENSE`, `NOTICE`, `THIRD-PARTY-LICENSES.txt` and `SHA256SUMS`. Never replace the assets of a published tag; release a new version.

## Windows

Not supported; WSL works. Native support would need a Windows build job, PowerShell versions of the launchers, Windows data paths and a Windows test run.
