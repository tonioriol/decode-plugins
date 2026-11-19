# Decode plugins

Marketplace of Decode companion plugins, with Claude Code plugins and
thin Codex adapters. It currently contains two plugins:

- **semantic-search** exposes `project_search` for source code.
- **memory** adds a durable feature-note workflow, the `search` MCP tool over `memory/` and `docs/`, and SessionStart, Stop, and PreCompact hooks. It declares `dependencies: ["semantic-search"]`, so Claude installs both.

## Claude Code

Run these two commands in Claude Code:

```text
/plugin marketplace add tonioriol/decode-plugins
/plugin install memory@decode-plugins
```

Use a recent Claude Code with plugin dependency and `userConfig` support. Development validation uses Claude Code 2.1.316. The first search downloads the pinned `rsemble` release binary with `curl` (or `gh` when it is signed in). Install Bash, curl and Python 3; the memory hooks also require Git, jq, and ripgrep.

Release binaries cover **macOS** (Apple Silicon and Intel) and **Linux** (x86_64 and aarch64). Windows is not supported (WSL works); see [building](docs/building.md). The engine's source is in [engine/](engine), so you can also build it yourself.

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

Data is shared between Claude Code, Codex and Decode, so nothing is downloaded or indexed twice. It lives under `DECODE_PLUGINS_DATA` (default `~/.decode/plugin-data`):

- `bin/<release>/rsemble-<platform>`: the engine binary.
- `memory/`: the notes index and embedding model, the same folder Decode's own notes search uses.
- `semantic-search/`: the code index and model, used only when Decode is not installed. With Decode installed in VS Code, semantic-search reads and updates Decode's own semantic index in its VS Code global storage (`~/Library/Application Support/Code/User/globalStorage/ultra.decode` on macOS, `~/.config/Code/User/globalStorage/ultra.decode` on Linux), so a project Decode already indexed is ready at once. Setting `DECODE_PLUGINS_DATA` keeps everything under that folder instead.

The past-chat index below stays per harness, in the plugin's own data folder.

Indexes stay on this machine; no account, project notes, source files, or credentials are distributed with this repository. On first use, the engine downloads and verifies its pinned `minishlab/potion-code-16M-v2` embedding model from Hugging Face. Model and index directories include the model revision, so upgrading to v2 rebuilds indexes rather than mixing v1 and v2 embeddings. Release downloads pin a version and verify the release's SHA-256 checksum before execution. A checksum authenticates integrity relative to this repository's release, not a separate signing identity.

### Optional: search this harness's past chats

Off by default. Claude Code and Codex have no semantic search over their own past sessions; this option adds one. Turn it on and memory exposes a second tool, `search_chats`, over your own **past sessions of that harness**: Claude Code reads `~/.claude/projects`, Codex reads `~/.codex/sessions`. While it is off, the `chats` server starts with no tools and indexes nothing.

- **Claude Code:** set the memory option `index_chats` to true (`/plugin`, or `printf '%s\n' '{"index_chats":"true"}' | claude plugin configure memory@decode-plugins --values-stdin`).
- **Codex (and Claude):** create the empty file `~/.config/decode-plugins/index-chats` (under `$XDG_CONFIG_HOME` when set), or export `DECODE_PLUGINS_INDEX_CHATS=true` for the harness process. The file also works for a Codex started from the desktop app.

Only user and assistant prose is indexed. Tool calls and outputs, reasoning, and injected system or environment blocks are left out, and so is Claude's subagent traffic. The prose is extracted into plain markdown under the plugin data folder (`<data>/chats/corpus/<project>/chats/<session>--pNNN.md`, split below the engine's 1 MB file limit) and served by the same engine as the notes search, so chat text and its derived index stay on this machine. The extract is refreshed when the server starts and every 30 seconds while it runs, re-reading only transcripts whose size or mtime changed; the engine notices the changed files on the next search.

Each hit is `path:lines` of an extract. The file's first lines name the session id and its source transcript. The `## mN role (line L, time)` headers give the message position, where `L` is the line in the source JSONL. To read around a hit, read a wider line range of the extract; read the source transcript at line `L` to see the tool calls. `project` filters by the folder name in brackets and `workspace` ranks one project first.

The embedding model is cloned from the notes index copy where supported, otherwise copied. RAM and index size grow with the amount of extracted prose rather than the size of the raw transcripts.

### Workflow and hooks

Invoke `/memory:memory` to create, recall, or update feature notes. The workflow lives once in [the memory document](plugins/memory/skills/memory/SKILL.md). Existing notes are never migrated automatically.

Hooks activate only for root sessions in projects that already contain `memory/`:

- SessionStart injects a discovery rule and up to ten active/paused feature slugs. The rule and the skill tell agents to search the notes first and, when the notes do not fully answer, to search past chats with the harness's own search (or `search_chats` when the option above is on) before answering.
- Stop reminds once after confirmed work without a corresponding committed feature-note update.
- PreCompact blocks until the note is committed. Claude's recovery compaction can still expose the underlying context-limit error; the hook is not a way around an API limit.

The hooks require a Git repository containing the notes. Subagents report outcomes rather than creating competing note updates. The hooks do not create repositories, commit, push, or alter project files themselves.

## Codex CLI

Thin `.codex-plugin` manifests reuse the same MCP launchers, workflow, and hook scripts; there is no second workflow copy. The separate marketplace schema is in [.agents/plugins/marketplace.json](.agents/plugins/marketplace.json).

Codex CLI 0.169.0 is the tested native client. Native MCP declarations do not
expand plugin-root placeholders. Its thin adapter therefore invokes the exact versioned copy under
`CODEX_HOME/plugins/cache/decode-plugins/<plugin>/<version>/` (default
`~/.codex`). This documented cache layout keeps the server's working directory
at the current project rather than changing it to the plugin installation.

```sh
codex plugin marketplace add tonioriol/decode-plugins
codex plugin add semantic-search@decode-plugins
codex plugin add memory@decode-plugins
```

Codex does not use the Claude dependency declaration, so install both explicitly. Set `DECODE_PLUGINS_SCAN_ROOT` and `DECODE_PLUGINS_SCAN_ROOT_2` in the Codex process environment for optional additional roots; the default is still current-project only. `CLAUDE_PLUGIN_DATA` is optional outside Claude; the past-chat index then lives in the standard user cache directory. The same scope warning applies: semantic-search scans ALL SOURCE CODE beneath those roots; broad scans may take minutes and use substantial disk space. Memory performs cheap NOTES-only scans. Codex uses these environment variables rather than Claude's `userConfig` fields.

### MCP approval

The native manifests enable only the read-only `search` and `project_search` tools
and ship `default_tools_approval_mode: "approve"`. In Codex 0.169 this means no
approval prompt is required, so `codex exec` can call them even when its approval
policy is `never`. The defaults belong to each server inside the native
`mcpServers` declaration, not Claude's MCP configuration. Existing per-plugin
user policy overrides these defaults.

For an existing installation that needs an explicit local policy, the equivalent
Codex configuration is:

```toml
[plugins."memory@decode-plugins".mcp_servers.memory]
enabled = true
enabled_tools = ["search"]
default_tools_approval_mode = "approve"

[plugins."semantic-search@decode-plugins".mcp_servers.semantic-search]
enabled = true
enabled_tools = ["project_search"]
default_tools_approval_mode = "approve"
```

### Hook trust

Codex requires review of each exact hook definition after installation or an
update. In interactive Codex 0.169, run **`/hooks`**, select an event such as
**SessionStart**, press **Enter**, select and review the memory plugin handler,
then press **`t`** to trust it. Repeat for **Stop** and **PreCompact**. On the
event-list screen, **`t` trusts all hooks needing review**; use that only after
reviewing all of them. Trust preserves enabled/disabled state, so a disabled
handler must also be enabled with Space or Enter. At startup, Codex may instead
offer **Review hooks**, **Trust all and continue**, or **Continue without trusting**.

Do not copy trust hashes between machines or permanently bypass hook trust.
`--dangerously-bypass-hook-trust` is only for vetted, bounded automation; it is
not the installation procedure. The adapter retains Codex's native trust checks.
Codex PreCompact uses `continue: false` and `stopReason`, stopping the turn before
compaction instead of Claude's `decision: block` response. Malformed hook input or
transcripts fail open and emit a diagnostic on stderr rather than silently losing
the reminder.

Pinned implementation references: [native MCP fields](https://github.com/openai/codex/blob/rust-v0.169.0/codex-rs/codex-mcp/src/plugin_config.rs#L155-L197),
[approval meaning](https://github.com/openai/codex/blob/rust-v0.169.0/codex-rs/core/src/mcp_tool_call.rs#L2772-L2784),
and [`/hooks` dispatch](https://github.com/openai/codex/blob/rust-v0.169.0/codex-rs/tui/src/chatwidget/slash_dispatch.rs#L537-L539).

## Development and testing

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

[scripts/rsemble.sh](scripts/rsemble.sh) is the canonical release bootstrap. After editing it, run `python3 scripts/sync-launchers.py` to package identical copies into the independently cached plugin roots. The checker rejects stale copies. Launcher fixtures exercise `gh` and public download, caching, checksum rejection, concurrent startup, the shared data layout, Decode's index, and safe path/scan arguments. Hook fixtures use real Git commits and native transcript shapes.

[The packaged memory hook](plugins/memory/hooks/flush-hooks.sh) is the canonical
hook source. If it is also deployed into another local plugin, check that copy
with `python3 scripts/check-memory-hook.py /path/to/deployed/flush-hooks.sh`.
The check fails on byte drift; the native hook tests also accept
`MEMORY_HOOK_UNDER_TEST` to exercise a deployed copy against the same rollout
fixtures. Codex 0.169 code-mode work is detected from completed command and
file-change events, never by executing or interpreting transcript JavaScript.

For isolated installation, set `CLAUDE_CONFIG_DIR` and `CODEX_HOME` to disposable directories. Codex also discovers home-wide instructions independently of `CODEX_HOME`; use a disposable home when verifying that only packaged content loads. Never replace a real user configuration with a test fixture. [Building and releasing](docs/building.md) documents the engine source, the release workflow and Windows.

Before a release, the plugins are also checked end to end: a clean marketplace install in a disposable `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, a cold download of the pinned release, and real `search` and `project_search` calls from a model-driven session.

References: [Claude dependencies](https://code.claude.com/docs/en/plugins/dependencies), [Claude manifest reference](https://code.claude.com/docs/en/plugins-reference), [Codex plugin packaging](https://developers.openai.com/plugins/build/plugins).
