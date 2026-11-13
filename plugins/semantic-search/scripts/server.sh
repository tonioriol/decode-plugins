#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export CLAUDE_PLUGIN_DATA=${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/decode-plugins/semantic-search}
args=(--project "${CLAUDE_PROJECT_DIR:-$PWD}" --folder . --tool project_search --description 'Search source code in the current project by meaning. Optional configured scan roots add other projects.')
for root in "${DECODE_PLUGINS_SCAN_ROOT:-}" "${DECODE_PLUGINS_SCAN_ROOT_2:-}"; do
  [[ -z $root ]] || args+=(--scan "$root")
done
exec "$here/semantic-mcp.sh" "${args[@]}" "$@"
