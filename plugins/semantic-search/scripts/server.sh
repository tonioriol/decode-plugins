#!/usr/bin/env bash
# Code search. When Decode is installed in VS Code, its semantic index and model are used in place, so a project
# Decode already indexed is not indexed again; otherwise they live in the shared data folder.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}
case "$(uname -s)" in
  Darwin) decode="$HOME/Library/Application Support/Code/User/globalStorage/ultra.decode" ;;
  *) decode="${XDG_CONFIG_HOME:-$HOME/.config}/Code/User/globalStorage/ultra.decode" ;;
esac
if [[ -z ${DECODE_PLUGINS_DATA:-} && -d $decode ]]; then
  storage=(--storage "$decode" --index "$decode/semantic-index")
else
  storage=(--storage "$data/semantic-search")
fi
args=(serve "${storage[@]}" --project "${CLAUDE_PROJECT_DIR:-$PWD}" --folder . --tool project_search --description 'Search source code in the current project by meaning. Optional configured scan roots add other projects.')
for root in "${DECODE_PLUGINS_SCAN_ROOT:-}" "${DECODE_PLUGINS_SCAN_ROOT_2:-}"; do
  [[ -z $root ]] || args+=(--scan "$root")
done
exec "$here/rsemble.sh" "${args[@]}" "$@"
