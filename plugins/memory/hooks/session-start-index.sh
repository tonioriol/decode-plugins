#!/usr/bin/env bash
# SessionStart hook: the memory rule plus the open features (active/paused),
# most recently committed first, so the agent can resume the right one.
# Top-level sessions in workspaces with memory/ only.
set -euo pipefail

input=$(cat)
[[ -n $(jq -r '.parent_session_id // empty' <<<"$input") ]] && exit 0
[[ $(jq -r '.session_id' <<<"$input") == $(jq -r '.root_session_id // .session_id' <<<"$input") ]] || exit 0

cwd=$(jq -r '.cwd // empty' <<<"$input")
feat="$cwd/memory"
[[ -d $feat ]] || exit 0

echo "Feature memory lives in memory/ (memory skill): read the matching context.md before resuming related work; the memory search tool finds notes in the current project and any configured scan roots. To recall earlier work, search the notes first; unless they fully answer the question asked (a related note is not an answer), search past chats before answering: the harness's own past-chat search, and if it has none, errors or finds nothing, grep its saved session transcripts on disk (often git-ignored: use grep -r or rg --no-ignore --hidden). When you resume a feature, set its status to active; when you stop, leave it active only if work continues, otherwise paused or done. When a durable result settles (diagnosis, decision, verified fix, measurement), update the feature's context.md and commit it. Commits, pushes and installs alone are not entries. Subagents report instead of flushing."

limit=10
open=$(rg -l --max-depth 2 '^status:[[:space:]]*(active|paused)\b' "$feat" -g 'context.md' 2>/dev/null || true)
[[ -n $open ]] || exit 0

# Last commit date per open folder in one pass: newest commits first, keep
# each folder's first sighting.
dates=$(cd "$feat" && git log --format='@%cs' --name-only --relative -- . 2>/dev/null | awk '
 /^@/ { when = substr($0, 2); next }
 NF { split($0, p, "/"); if (!(p[1] in s)) { s[p[1]] = 1; print p[1] "\t" when } }')

echo
echo "Recent open work:"
while IFS= read -r ctx; do
 dir=${ctx%/context.md}; dir=${dir##*/}
 when=$(awk -F'\t' -v d="$dir" '$1 == d {print $2; exit}' <<<"$dates")
 printf '%s\t- %s\n' "${when:-0000-00-00}" "${dir#*-}"
done <<<"$open" | sort -r | head -n $limit | cut -f2-
