#!/usr/bin/env bash
# Root-session memory gates (Stop, PreCompact).
#
# Work: a successful `git commit` command, a confirmed file edit outside memory/
# and docs/, or a completed subagent result in this chat's transcript.
# Flushed: git shows a commit touching a memory/<folder> this chat wrote to
# (named in its commands or edit paths), made at or after the latest work.
# Git is the source of truth, so it works whether memory/ is its own repository
# or part of the project's, and no shell command needs to be understood.
#
# Stop reminds once per work item; PreCompact blocks until flushed.
set -euo pipefail
trap 'printf "%s\n" "memory hook: failed; continuing without gating" >&2; exit 0' ERR

state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/memory-hooks"
input=$(cat)
IFS=$'\t' read -r session root cwd transcript stop_active event parent < <(jq -r '[
 .session_id, (.root_session_id // .session_id), .cwd,
 (.transcript_path // ""), (.stop_hook_active // false),
 (.hook_event_name // "Stop"), (.parent_session_id // "")
] | map(tostring | if . == "" then "-" else . end) | @tsv' <<<"$input")

[[ $session == "$root" && $parent == "-" ]] || exit 0
[[ $event == Stop || $event == PreCompact ]] || exit 0
[[ -d "$cwd/memory" && -f $transcript ]] || exit 0
git -C "$cwd/memory" rev-parse --git-dir >/dev/null 2>&1 || exit 0

# Prints "<kind>:<id>" for unflushed work, nothing when flushed or idle.
fp=$(python3 - "$transcript" "$cwd" "$(dirname "$0")" <<'PYTHON'
import json, os, re, subprocess, sys

transcript, workspace, hooks_dir = sys.argv[1:]
sys.path.insert(0, hooks_dir)
from transcript import messages as transcript_messages, result_text
memory = os.path.normpath(os.path.join(workspace, "memory"))
EDIT_TOOLS = {"apply_diff", "apply_patch", "write_to_file", "edit", "search_replace", "search_and_replace", "edit_file", "Edit", "Write", "MultiEdit"}
COMMAND_TOOLS = {"execute_command", "Bash"}
# `git ... commit` within one simple command; `pre-commit`, `commit-msg` and
# paths do not count.
GIT_COMMIT = re.compile(r"(?<![\w./-])git(?![\w./-])[^\n;&|]*?(?<![\w./-])commit(?![\w./-])")
FOLDER = re.compile(r"/?(?:[A-Za-z0-9_.-]+/)*memory/\d{14}-[A-Za-z0-9-]+")
# A bare folder name, from a command run inside memory/.
MEMORY_RELATIVE_FOLDER = re.compile(r"(?<![\w./-])\d{14}-[A-Za-z0-9-]+")

messages = transcript_messages(transcript)

# Feature memory, other docs and agent working files (.agent-runs/, tmp/) are not work.
NOT_WORK = [os.path.join(workspace, folder) for folder in ("memory", "docs", ".agent-runs", "tmp")]

def is_work(path):
    path = os.path.normpath(os.path.join(workspace, path))
    return not any(path == root or path.startswith(root + os.sep) for root in NOT_WORK)

work, folders, commits, edits = None, set(), {}, {}
for message in messages:
    content = message.get("content", [])
    if not isinstance(content, list):
        continue
    ts = message.get("ts", 0)
    for item in content:
        kind = item.get("type")
        if kind == "tool_use":
            name, params = item.get("name"), item.get("input") or {}
            if name in COMMAND_TOOLS or name in EDIT_TOOLS:
                base = os.path.join(workspace, params.get("cwd") or workspace) if name in COMMAND_TOOLS else workspace
                folders.update(os.path.normpath(os.path.join(base, path))
                               for path in FOLDER.findall(json.dumps(params)))
                if os.path.basename(os.path.normpath(base)) == "memory":
                    folders.update(os.path.normpath(os.path.join(base, path))
                                   for path in MEMORY_RELATIVE_FOLDER.findall(json.dumps(params)))
            if name in COMMAND_TOOLS and GIT_COMMIT.search(params.get("command", "")):
                # The commit runs after the call is made: date it from the call.
                commits[item.get("id", "")] = ts
            elif name in EDIT_TOOLS:
                edits[item.get("id", "")] = (params, message.get("native"))
        elif kind == "tool_result":
            call, text = item.get("tool_use_id", ""), result_text(item)
            if item.get("is_error"):
                continue
            if call in commits and (
                message.get("native") == "claude"
                or (message.get("native") == "codex" and re.search(r"Process exited with code 0", text))
                or re.search(r"\bExit code: 0\b", text.split("\n", 1)[0])):
                work = (commits[call], "commit:" + call)
            elif call in edits:
                # Decode reports saved edits as {"path", "operation"} JSON;
                # denials, errors and unchanged files do not.
                paths = item.get("confirmed_paths", [])
                params, native = edits[call]
                if native == "claude":
                    paths = [params["file_path"]] if "file_path" in params else []
                elif native == "codex" and text.startswith("Success. Updated the following files:"):
                    paths = re.findall(r"^[AMD] (.+)$", text, re.MULTILINE)
                for match in re.finditer(r"(?:^|\n)\s*(?=\{)", text):
                    try:
                        result, _ = json.JSONDecoder().raw_decode(text[match.end():])
                    except json.JSONDecodeError:
                        continue
                    if isinstance(result, dict) and result.get("operation") in ("created", "modified") and isinstance(result.get("path"), str):
                        paths.append(result["path"])
                deleted = re.search(r"(?:^|\n)Successfully deleted (.+)", text)
                if deleted:
                    paths.append(deleted.group(1))
                if any(is_work(path) for path in paths):
                    work = (ts, "edit:" + call)
            elif text.startswith('{"completed":[{'):
                work = (ts, "sub:" + call)
        elif kind == "text" and re.match(r'\s*<agent_event [^>]*event="completed"', item.get("text", "")):
            work = (ts, "sub:" + str(ts))

if work is None:
    sys.exit()
for folder in sorted(folders):
    if not os.path.isdir(folder):
        continue
    last = subprocess.run(["git", "-C", folder, "log", "-1", "--format=%ct", "--", "."],
                          capture_output=True, text=True).stdout.strip()
    if last and int(last) >= work[0] // 1000:
        sys.exit()
print(work[1])
PYTHON
)
[[ -n $fp ]] || exit 0

state_file="$state_dir/$session.json"
mkdir -p "$state_dir"
state=$(cat "$state_file" 2>/dev/null || echo '{}')
save() {
 state=$(jq -c "$@" <<<"$state")
 printf '%s\n' "$state" >"$state_file.tmp" && mv "$state_file.tmp" "$state_file"
}
[[ $(jq -r '.fp // empty' <<<"$state") == "$fp" ]] || save -n --arg fp "$fp" '{fp: $fp}'

if [[ $event == Stop ]]; then
 [[ $(jq -r '.gated // empty' <<<"$state") == true || $stop_active == true ]] && exit 0
 save '.gated = true'
fi

# The reason names no triggering commit or edit: naming it made agents log the
# commit or install itself instead of the result it shipped.
reason="Memory check: work happened since this chat last committed its feature memory. If a durable result settled (diagnosis, decision, verified fix, measurement), record it in the feature's memory/<folder>/context.md via the memory skill and commit it (git must show a commit touching that folder). Commits, pushes and installs are not entries; cite their hashes inside the result they ship."
if [[ $event == PreCompact ]]; then
 reason="$reason Do it now, before other work: compaction is postponed, and it runs on its own at the next request once that commit exists."
else
 reason="$reason If nothing durable settled, just stop again (complete or ask); Stop reminds once per new item."
fi
if [[ $event == PreCompact && $(jq -r '.turn_id // empty' <<<"$input") != "" ]]; then
 jq -cn --arg r "$reason" '{continue: false, stopReason: $r}'
else
 jq -cn --arg r "$reason" '{decision: "block", reason: $r}'
fi
