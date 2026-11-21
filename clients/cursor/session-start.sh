#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# A user hook starts in ~/.cursor, not in the project. Do not guess which
# project owns the chat if Cursor does not report exactly one workspace root.
payload=$(jq -c '
  select((.workspace_roots | length) == 1)
  | select((.workspace_roots[0] | type) == "string" and (.workspace_roots[0] | startswith("/")))
  | (.session_id // .conversation_id) as $id
  | select(($id | type) == "string" and ($id | length) > 0)
  | {cwd: .workspace_roots[0], session_id: $id, parent_session_id: .parent_conversation_id}
')
[[ -n $payload ]] || exit 0
context=$(bash "$here/../../plugins/memory/hooks/session-start-index.sh" <<<"$payload")
[[ -n $context ]] || exit 0
jq -cn --arg context "$context" '{additional_context: $context}'
