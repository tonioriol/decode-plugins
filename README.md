# Decode plugins

Marketplace for Decode companion plugins, with Claude Code plugins and
thin Codex adapters. It currently contains two plugins:

- **semantic-search** exposes `project_search` for source code.
- **memory** adds a durable feature-note workflow, the `search` MCP tool over `memory/` and `docs/`, and SessionStart, Stop, and PreCompact hooks. It declares `dependencies: ["semantic-search"]`, so Claude installs both.

## Claude Code

To install, run these two commands in Claude Code:

```text
/plugin marketplace add tonioriol/decode-plugins
/plugin install memory@decode-plugins
```

Use a recent Claude Code with plugin dependency and `userConfig` support. The historical launcher uses GitHub CLI to download its release binary. Install Bash and Python 3; the memory hooks also require Git, jq, and ripgrep.

The trial release provides **macOS Apple Silicon** only. Other systems receive a clear missing-asset error until their binaries are published; see [building](docs/building.md). The launcher does not fetch source code or build it on the user's computer.

### Scope and configuration

Both MCP servers search **only the current project by default**. Memory searches its `memory/` and `docs/` folders; semantic-search indexes source code. The engine applies ignore rules, including `.rooignore`, rejects symlinks, and limits indexed file size.

Each plugin exposes optional directory options `scan_root` and `scan_root_2` under `/config`. Leave them empty for project-only scope. Configure each plugin separately; configuring memory does not change semantic-search. These are local user settings, never repository defaults.

- **semantic-search: broad scans index ALL SOURCE CODE** under the selected folder across its projects, subject to the engine's ignore and size rules. A cold search can take **minutes** while that code is embedded and indexed. Do not point it at a broad projects folder when you only want cross-project notes.
- **memory: cheap cross-project NOTES scans** read only project `memory/` and `docs/` folders, not all source code. Parent directories containing many projects are suitable for this notes-only recall.

The engine still warms its index eagerly. While a search waits, MCP clients that send a progress token receive project-folder and file indexing counts plus **Saving N/M project indexes** during persistence rather than a silent wait. Whether those messages are shown depends on the client; progress does not make a broad cold scan cheap.

Broad scans over large code trees can take minutes and use substantial disk space. Leave semantic-search scan roots empty unless that scope is intentional; use notes-only scans for cross-project recall.

From a shell, the equivalent configuration interface is:

```sh
printf '%s\n' '{"scan_root":"/path/to/projects"}' | claude plugin configure memory@decode-plugins --values-stdin
```

Downloaded binaries and indexes live in `CLAUDE_PLUGIN_DATA`, which survives plugin updates. The verified standalone executable is also atomically published at `~/.local/share/decode-plugins/bin/semantic-mcp` for sharing between plugins. Memory first detects the newest executable in a standard Decode VS Code extension installation; otherwise it uses the shared standalone executable, downloading the pinned release if needed. Semantic-search always uses the standalone release. `DECODE_PLUGINS_STANDALONE=1` disables Decode detection for isolated verification.

The index is local to each plugin's data directory; no account, project notes, source files, or credentials are distributed with this repository. On first use, the engine may download its embedding model. Release downloads use `gh release download`, pin a version, and verify the release's SHA-256 checksum before execution. A checksum authenticates integrity relative to this release, not a separate signing identity.

### Workflow and hooks

Invoke `/memory:memory` to create, recall, or update feature notes. The workflow lives once in [the memory document](plugins/memory/skills/memory/SKILL.md). Existing notes are never migrated automatically.

Hooks activate only for root sessions in projects that already contain `memory/`:

- SessionStart injects a discovery rule and up to ten active/paused feature slugs.
- Stop reminds once after confirmed work without a corresponding committed feature-note update.
- PreCompact blocks until the note is committed. Claude's recovery compaction can still expose the underlying context-limit error; the hook is not a way around an API limit.

The hooks require a Git repository containing the notes. Subagents report outcomes rather than creating competing note updates. The hooks do not create repositories, commit, push, or alter project files themselves.

## Codex CLI

Thin `.codex-plugin` manifests reuse the same MCP launchers, workflow, and hook scripts; there is no second workflow copy. The separate marketplace schema is in [.agents/plugins/marketplace.json](.agents/plugins/marketplace.json).

Codex CLI 0.163.0 does not expand plugin-root placeholders in native MCP
declarations. Its thin adapter therefore invokes the exact versioned copy under
`CODEX_HOME/plugins/cache/decode-plugins/<plugin>/<version>/` (default
`~/.codex`). This documented cache layout keeps the server's working directory
at the current project rather than changing it to the plugin installation.

```sh
codex plugin marketplace add tonioriol/decode-plugins
codex plugin add semantic-search@decode-plugins
codex plugin add memory@decode-plugins
```

Codex does not use the Claude dependency declaration, so install both explicitly. Set `DECODE_PLUGINS_SCAN_ROOT` and `DECODE_PLUGINS_SCAN_ROOT_2` in the Codex process environment for optional additional roots; the default is still current-project only. `CLAUDE_PLUGIN_DATA` is optional outside Claude; the launcher then uses the standard user cache directory. The same scope warning applies: semantic-search scans ALL SOURCE CODE beneath those roots; broad scans may take minutes and use substantial disk space. Memory performs cheap NOTES-only scans. Codex uses these environment variables rather than Claude's `userConfig` fields.

Codex requires review of each exact hook definition. Do not copy trust hashes between machines or permanently bypass hook trust. The adapter uses the native compatibility manifest and retains Codex's hook-trust checks. Validate native hook loading with the installed Codex version before relying on compaction blocking. Codex PreCompact uses `continue: false` and `stopReason`, stopping the turn before compaction instead of Claude's `decision: block` response.

## Development and verification

```sh
python3 scripts/sync-launchers.py --check
python3 scripts/audit.py
python3 tests/test_launchers.py -v
bash plugins/memory/hooks/test-hooks.sh
bash plugins/memory/hooks/test-native-hooks.sh
claude plugin validate . --strict
claude plugin validate plugins/memory --strict
claude plugin validate plugins/semantic-search --strict
```

[scripts/semantic-mcp.sh](scripts/semantic-mcp.sh) is the canonical release bootstrap. After editing it, run `python3 scripts/sync-launchers.py` to package identical copies into the independently cached plugin roots. The checker rejects stale copies. Launcher fixtures exercise authenticated download, caching, checksum rejection, concurrent startup, Decode preference, shared standalone fallback, and safe path/scan arguments. Hook fixtures use real Git commits and native transcript shapes.

For isolated installation, set `CLAUDE_CONFIG_DIR` and `CODEX_HOME` to disposable directories. Codex also discovers home-wide instructions independently of `CODEX_HOME`; use a disposable home when verifying that only packaged content loads. Never replace a real user configuration with a test fixture. [Release and source access](docs/building.md) documents the private-source build options and unsupported platforms. [Trial verification](docs/verification.md) records the tested versions, real runtime evidence, and remaining boundaries.

References: [Claude dependencies](https://code.claude.com/docs/en/plugins/dependencies), [Claude manifest reference](https://code.claude.com/docs/en/plugins-reference), [Codex plugin packaging](https://developers.openai.com/plugins/build/plugins).
