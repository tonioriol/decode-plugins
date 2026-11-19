# rsemble

Semantic code search CLI and MCP server, part of **rsemble**, a Rust port of [MinishLab Semble](https://github.com/MinishLab/semble).

The `rsemble` binary offers `search`, `mcp`, `serve`, and `model` commands. Run `rsemble --help` for options.
`serve` searches the same-named folders (say `notes/`) of many projects at once, ranking the caller's project first.

The model lives in the storage folder: `--storage`, else `$SEMBLE_RS_HOME`, else `$XDG_CACHE_HOME/semble-rs`, else
`~/.cache/semble-rs`. Indexes live in `--index` (default `<storage>/index`), one file per folder named by the SHA-256
of its path: the layout and listing rules Decode uses, so Decode and rsemble pointed at the same folders share one
index.

Licensed under MIT; see [LICENSE](LICENSE) and [NOTICE](NOTICE) for attribution.
