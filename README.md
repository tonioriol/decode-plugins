# Decode plugins

Two plugins for Claude Code, Codex, OpenCode and Cursor:

- **memory**: feature notes in `memory/`, a `search` tool over `memory/` and `docs/`, and hooks that show matching notes at session start and remind the agent to update them.
- **semantic-search**: a `project_search` tool over source code.

Both run [Decode Semantic](https://tonioriol.github.io/decode-semantic/#search), a Rust port of [MinishLab Semble](https://github.com/MinishLab/semble). Overview: [tonioriol.github.io/decode-semantic](https://tonioriol.github.io/decode-semantic/). Benchmarks: [memory recall](bench/memory), [code search](bench/semantic).

## Requirements

- macOS (Apple Silicon, Intel) or Linux (x86_64, aarch64). Windows only through WSL.
- Bash, curl and Python 3. The memory hooks also need Git, jq and ripgrep.

The first search downloads the pinned engine release and checks its SHA-256, then downloads the `minishlab/potion-code-16M-v2` model from Hugging Face. Indexes stay on your machine.

## Claude Code

```text
/plugin marketplace add tonioriol/decode-plugins
/plugin install memory@decode-plugins
```

`memory` depends on `semantic-search`, so both get installed.

## Codex

```sh
codex plugin marketplace add tonioriol/decode-plugins
codex plugin add semantic-search@decode-plugins
codex plugin add memory@decode-plugins
```

Codex ignores plugin dependencies, so install both. The search tools are pre-approved. The hooks need trust: run `/hooks`, review the memory handlers for SessionStart, Stop and PreCompact, and press `t` on each.

## OpenCode

```sh
git clone --branch v0.3.1 https://github.com/tonioriol/decode-plugins.git "$HOME/decode-plugins"
```

1. Merge [opencode-v1.json](clients/opencode/opencode-v1.json) (OpenCode 1.x) or [opencode-v2.json](clients/opencode/opencode-v2.json) (2.x) into `~/.config/opencode/opencode.json`, or `opencode.json` in the project. Replace `/absolute/path/to/decode-plugins` with the clone path.
2. Copy the skill:
   ```sh
   dest=.opencode   # or "${XDG_CONFIG_HOME:-$HOME/.config}/opencode" for all projects
   mkdir -p "$dest/skills/memory"
   cp "$HOME/decode-plugins/plugins/memory/skills/memory/SKILL.md" "$dest/skills/memory/"
   ```
3. Restart OpenCode. `opencode mcp list` shows both servers.

On 1.x a small plugin adds the session-start notes on the first message of a new session. 2.x gets the tools and the skill only. Neither gets the Stop or PreCompact reminders.

## Cursor

Use the same clone. Pick `~/.cursor/` for all projects or `.cursor/` in one project:

1. Merge [mcp.json](clients/cursor/mcp.json) into `mcp.json` there. Replace the clone path; keep `${workspaceFolder}`.
2. Merge [hooks.json](clients/cursor/hooks.json) into `hooks.json` there. Replace the clone path.
3. Copy `plugins/memory/skills/memory/SKILL.md` to `skills/memory/SKILL.md` there.
4. Reload Cursor and check both servers under **Customize → MCP**.

Only the session-start hook is wired up; Cursor's stop and compaction hooks can't run the reminders. Use a single-folder workspace.

## Configuration

**Scope.** Both tools search the current project only. To search more, set up to two extra roots: `scan_root` and `scan_root_2` in `/plugin` config on Claude Code, or `DECODE_PLUGINS_SCAN_ROOT` and `DECODE_PLUGINS_SCAN_ROOT_2` in the environment elsewhere. Set them per plugin:

- memory reads only `memory/` and `docs/` folders, so a parent folder of all your projects is cheap.
- semantic-search indexes all source code under the root. A big folder takes minutes and disk space the first time.

**Past chats.** Off by default. Turn it on and memory adds `search_chats` over this harness's own past sessions (`~/.claude/projects` or `~/.codex/sessions`): set `index_chats` to true on Claude Code, or create the empty file `~/.config/decode-plugins/index-chats`. Only user and assistant text is indexed.

**Data** lives in `~/.decode/plugin-data` (override with `DECODE_PLUGINS_DATA`): the engine binary, the notes index and, without Decode installed, the code index. With Decode installed, semantic-search shares Decode's own code index.

## Hooks

Hooks run only in top-level sessions of projects that have a `memory/` folder in a Git repository:

- **SessionStart** (startup, resume, after compaction): the note rule, up to ten open features and up to three notes matching the opening prompt or the latest messages.
- **Stop**: one reminder when work was done but no note was committed.
- **PreCompact**: holds compaction until the note is committed.

Invoke `/memory:memory` to create, find or update notes. The workflow is in [SKILL.md](plugins/memory/skills/memory/SKILL.md).

## Development

```sh
python3 scripts/sync-launchers.py --check
python3 scripts/audit.py
python3 -m unittest discover -s tests
bash plugins/memory/hooks/test-hooks.sh
bash plugins/memory/hooks/test-native-hooks.sh
claude plugin validate . --strict
```

[scripts/decode-semantic.sh](scripts/decode-semantic.sh) is the launcher source; run `python3 scripts/sync-launchers.py` after editing it. Engine source, builds and releases: [docs/building.md](docs/building.md).
