#!/usr/bin/env bash
# Notes search over memory/ and docs/. The index and model live in the data folder Decode's own notes search
# uses, so Decode and every harness share one notes index.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}
args=(serve --storage "$data/memory" --project "${CLAUDE_PROJECT_DIR:-$PWD}" --folder memory --folder docs=0.5 --docs-only --tool search --description 'Search durable feature notes and documentation by meaning. Current project only unless additional scan roots are configured; pass workspace to rank it first, or project to filter. Covers only work that was written up: unless results fully answer the question asked, also search past chats (the harness'"'"'s own past-chat search, then its saved session transcripts on disk) before answering.')
for root in "${DECODE_PLUGINS_SCAN_ROOT:-}" "${DECODE_PLUGINS_SCAN_ROOT_2:-}"; do
  [[ -z $root ]] || args+=(--scan "$root")
done
exec "$here/rsemble.sh" "${args[@]}" "$@"
