#!/usr/bin/env bash
# Notes search over memory/ and docs/. The index and model live in the data folder Decode's own notes search
# uses, so Decode and every harness share one notes index.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}
args=(serve --storage "$data/memory" --project "${CLAUDE_PROJECT_DIR:-$PWD}" --folder memory --folder docs=0.5 --docs-only --tool search --description 'Search feature notes (memory/) and project docs by meaning for what, why, and status questions. When you know the folder or file, pass path to rank only its contents. By default only the current project is indexed; with scan roots configured, pass workspace to boost its project or project to restrict the search. Frontmatter terms such as status: active or a tag influence ranking, not filtering. Ranked results give project and file:lines; use rg for exact patterns or complete lists. If the notes do not fully answer the question, also search past chats before answering.')
for root in "${DECODE_PLUGINS_SCAN_ROOT:-}" "${DECODE_PLUGINS_SCAN_ROOT_2:-}"; do
  [[ -z $root ]] || args+=(--scan "$root")
done
exec "$here/decode-semantic.sh" "${args[@]}" "$@"
