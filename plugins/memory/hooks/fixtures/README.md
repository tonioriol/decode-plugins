# Codex transcript fixtures

These synthetic JSONL fixtures model three Codex CLI 0.169 cases: a successful
Git commit, a refused Git commit, and a file addition. They preserve tool call,
output and completed event shapes without real identifiers or timing data. The
tests replace the working directory and timestamps to match each test repository.
