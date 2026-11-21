#!/usr/bin/env bash
# UserPromptSubmit hook: on the first message of a top-level session, search memory/ for notes about it and
# name the best few, so the agent starts from the right notes. SessionStart can't: it runs before any message.
# Any failure (no binary yet, no model, timeout) prints nothing; the session goes on without the hint.
set -euo pipefail

input=$(cat)
[[ -n $(jq -r '.parent_session_id // empty' <<<"$input") ]] && exit 0
session=$(jq -r '.session_id // empty' <<<"$input")
[[ -n $session && $session == $(jq -r '.root_session_id // .session_id' <<<"$input") ]] || exit 0

cwd=$(jq -r '.cwd // empty' <<<"$input")
[[ -d $cwd/memory ]] || exit 0

# Only a new session's first message: SessionStart (startup) leaves a marker, the next prompt takes it. A resumed,
# condensed or older chat never got one, so its messages stay silent.
marks="${XDG_STATE_HOME:-$HOME/.local/state}/memory-hooks/first-prompt"
if [[ $(jq -r '.hook_event_name // empty' <<<"$input") == SessionStart ]]; then
  [[ $(jq -r '.source // "startup"' <<<"$input") == startup ]] && mkdir -p "$marks/$session"
  exit 0
fi
rmdir "$marks/$session" 2>/dev/null || exit 0

prompt=$(jq -r '.prompt // empty' <<<"$input" | head -c 2000)
[[ -n ${prompt//[[:space:]]/} ]] || exit 0

here=$(cd "$(dirname "$0")" && pwd)
data=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}
# The notes index the memory search tool keeps, so a warm search takes about 0.15 s.
hits=$("$here/../scripts/decode-semantic.sh" search "$prompt" --path "$cwd/memory" --docs-only --limit 6 --json \
  --storage "$data/memory" --index "$data/memory/index" 2>/dev/null) || exit 0

# One line per note folder, best first, at most 3, with its title.
lines=$(jq -r --arg root "$cwd/" '.[].file | ltrimstr($root)' <<<"$hits" 2>/dev/null | awk -F/ '
  $1 == "memory" && NF >= 3 && !seen[$2]++ { print $1 "/" $2 "/context.md" }' | head -n 3 |
  while IFS= read -r note; do
    [[ -f $cwd/$note ]] || continue
    title=$(sed -n 's/^title:[[:space:]]*//p' "$cwd/$note" | head -n 1 | tr -d '"')
    echo "- $note${title:+ ($title)}"
  done)
[[ -n $lines ]] || exit 0

jq -cn --arg c "Notes that may bear on this request, best match first (memory search on the first message). Read the matching context.md before relying on it; skip any that are unrelated.
$lines" '{hookSpecificOutput: {hookEventName: "UserPromptSubmit", additionalContext: $c}}'
