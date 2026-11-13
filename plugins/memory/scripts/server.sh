#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export AGENT_MEMORY_PREFER_DECODE=1
export CLAUDE_PLUGIN_DATA=${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/agent-memory/memory}
args=(--project "${CLAUDE_PROJECT_DIR:-$PWD}" --folder memory --folder docs=0.5 --docs-only --tool search --description 'Search durable feature notes and documentation by meaning. Current project only unless additional scan roots are configured; pass workspace to rank it first, or project to filter.')
for root in "${AGENT_MEMORY_SCAN_ROOT:-}" "${AGENT_MEMORY_SCAN_ROOT_2:-}"; do
  [[ -z $root ]] || args+=(--scan "$root")
done
exec "$here/semantic-mcp.sh" "${args[@]}" "$@"
