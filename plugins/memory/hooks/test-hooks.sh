#!/usr/bin/env bash
# Fixture tests for flush-hooks.sh. Run: ./test-hooks.sh
set -euo pipefail

hook="$(cd "$(dirname "$0")" && pwd)/flush-hooks.sh"
fixtures_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel 2>/dev/null || pwd)/.agent-runs/memory-hook-fixtures"
mkdir -p "$fixtures_root"
tmp=$(mktemp -d "$fixtures_root/fixtures.XXXXXX")
T="$tmp/transcript.jsonl"
F=20261010080000-feature   # this chat's feature folder
G=20261010080000-other     # another agent's feature folder
E=1790000000               # base epoch seconds; fixture times are offsets from it
pass=0 fail=0 n=0 resets=0

# Workspace layouts: "split" = memory/ is its own repository (separate repository);
# "mono" = memory/ is tracked by the project repository (single repository).
layout() {
	ws="$tmp/ws-$1-$resets"
	mkdir -p "$ws/memory/$F" "$ws/memory/$G"
	if [[ $1 == split ]]; then git -C "$ws" init -q; git -C "$ws/memory" init -q; repo="$ws/memory"
	else git -C "$ws" init -q; repo="$ws"; fi
	echo seed >"$ws/memory/$F/context.md"; echo seed >"$ws/memory/$G/context.md"
	git_commit 0 memory
}
git_commit() { # offset-seconds path...
	local at=$(( E + $1 )); shift
	local paths=("$@")
	[[ $repo == "$ws/memory" ]] && paths=("${paths[@]/#memory\//}")
	for p in "${paths[@]}"; do [[ -d $repo/$p ]] || echo "$at" >>"$repo/$p"; done
	git -C "$repo" add -- "${paths[@]}"
	GIT_AUTHOR_DATE="@$at" GIT_COMMITTER_DATE="@$at" git -C "$repo" -c user.name=t -c user.email=t@t commit -qm "c$at" -- "${paths[@]}"
}
flush() { echo "$1" >>"$ws/memory/${2:-$F}/context.md"; git_commit "$1" "memory/${2:-$F}/context.md"; }

reset() { : >"$T"; resets=$((resets + 1)); export XDG_STATE_HOME="$tmp/state-$resets"; n=0; layout "${1:-split}"; }

# Transcript records in Decode's journal shape; ts in ms from offset seconds.
msg() { # role offset content-json
	jq -cn --arg r "$1" --argjson ts "$(( (E + $2) * 1000 ))" --argjson c "$3" '{v:1,type:"patch",ts:$ts,representation:"api",start:0,length:1,messages:[{role:$r,ts:$ts,content:$c}]}' >>"$T"
}
cmd() { # offset command [exit-code]
	n=$((n + 1))
	msg assistant "$1" "$(jq -cn --arg c "$2" --arg id "t$n" '[{type:"tool_use",id:$id,name:"execute_command",input:{command:$c}}]')"
	msg user "$1" "$(jq -cn --arg id "t$n" --arg x "${3:-0}" '[{type:"tool_result",tool_use_id:$id,content:("Command executed in terminal within working directory /w. Exit code: " + $x + "\nOutput:\n")}]')"
}
code_commit() { cmd "$1" 'git add a.ts && git commit --no-verify -m "fix: thing"'; }
# The flush as the agent issues it; the real commit is made by flush().
docs_cmd() { cmd "$1" "git commit -m notes -- memory/${2:-$F}/context.md"; flush "$1" "${2:-$F}"; }
edit() { # offset path [operation]
	n=$((n + 1))
	msg assistant "$1" "$(jq -cn --arg p "$2" --arg id "e$n" '[{type:"tool_use",id:$id,name:"edit",input:{file_path:$p}}]')"
	msg user "$1" "$(jq -cn --arg p "$2" --arg o "${3:-modified}" --arg id "e$n" '[{type:"tool_result",tool_use_id:$id,content:({path:$p,operation:$o}|tojson)}]')"
}
sub_result() { n=$((n + 1)); msg user "$1" "$(jq -cn --arg id "w$n" '[{type:"tool_result",tool_use_id:$id,content:"{\"completed\":[{\"agentId\":\"a\"}]}"}]')"; }
sub_event() { msg user "$1" '[{"type":"text","text":"<agent_event from=\"a\" name=\"x\" event=\"completed\">\nok\n</agent_event>"}]'; }

run() { # event [extra-json] [session] [root] [cwd]
	jq -cn --arg e "$1" --argjson x "${2:-{\}}" --arg s "${3:-root}" --arg r "${4:-${3:-root}}" --arg c "${5:-$ws}" --arg t "$T" \
		'{session_id:$s, root_session_id:$r, transcript_path:$t, cwd:$c, hook_event_name:$e} + $x' | "$hook"
}
stop() { run Stop '{"stop_hook_active":false}'; }
compact() { run PreCompact '{"trigger":"auto"}'; }
check() { # name actual expected-substring ("" = empty output)
	if [[ -z $3 && -z $2 ]] || [[ -n $3 && $2 == *"$3"* ]]; then pass=$((pass + 1)); echo "ok   $1"
	else fail=$((fail + 1)); echo "FAIL $1: got [${2:0:160}] want [${3:-<empty>}]"; fi
}
B='"decision":"block"'

for L in split mono; do
	reset $L; code_commit 10
	check "$L: subagent session skipped" "$(run Stop '{}' child root)" ""
	check "$L: parent_session_id skipped" "$(run PreCompact '{"parent_session_id":"p"}')" ""
	mkdir -p "$tmp/plain"; check "$L: no memory/ skipped" "$(run Stop '{}' root root "$tmp/plain")" ""

	reset $L
	check "$L: empty transcript passes" "$(compact)" ""
	reset $L; code_commit 10; docs_cmd 20
	check "$L: code then flush passes Stop" "$(stop)" ""
	check "$L: code then flush passes PreCompact" "$(compact)" ""
	reset $L; docs_cmd 10; code_commit 20
	check "$L: code after flush blocks Stop" "$(stop)" "$B"
	check "$L: Stop reminds once" "$(stop)" ""
	check "$L: PreCompact keeps blocking" "$(compact)" "$B"
	check "$L: PreCompact still blocks on retry" "$(compact)" "$B"
	docs_cmd 30
	check "$L: new flush releases PreCompact" "$(compact)" ""

	reset $L; code_commit 10; cmd 15 "git commit -m wip -- memory/$F/context.md"
	check "$L: commit command naming folder but no real commit blocks" "$(compact)" "$B"
	reset $L; code_commit 10; edit 15 "memory/$F/context.md"; flush 5
	check "$L: folder commit older than work blocks" "$(compact)" "$B"
	reset $L; code_commit 10; edit 15 "memory/$F/context.md"; flush 20 $G
	check "$L: another agent's folder commit does not release" "$(compact)" "$B"
	reset $L; code_commit 10; flush 20
	check "$L: flush of an unnamed folder does not release" "$(compact)" "$B"
	reset $L; code_commit 10; edit 15 "memory/$F/context.md"; flush 20
	check "$L: edit-tool flush then commit releases" "$(compact)" ""
done

# The chat may be rooted above the repository holding its feature notes.
for L in split mono; do
	reset $L
	root_ws=$ws
	ws="$root_ws/app"
	mkdir -p "$ws/memory/$F" "$ws/memory/$G"
	git -C "$ws" init -q
	repo=$ws
	if [[ $L == split ]]; then git -C "$ws/memory" init -q; repo="$ws/memory"; fi
	echo seed >"$ws/memory/$F/context.md"; echo seed >"$ws/memory/$G/context.md"
	git_commit 0 memory
	code_commit 10; edit 15 "$ws/memory/$F/context.md"
	check "$L nested: uncommitted notes block" "$(run PreCompact '{}' root root "$root_ws")" "$B"
	flush 20 $G
	check "$L nested: different folder does not release" "$(run PreCompact '{}' root root "$root_ws")" "$B"
	flush 5
	check "$L nested: old matching commit does not release" "$(run PreCompact '{}' root root "$root_ws")" "$B"
	flush 20
	check "$L nested: absolute feature path releases" "$(run PreCompact '{}' root root "$root_ws")" ""
	: >"$T"
	code_commit 10
	msg assistant 20 "$(jq -cn --arg c "$ws" --arg p "memory/$F/context.md" '[{type:"tool_use",id:"nested",name:"execute_command",input:{cwd:$c,command:("git commit -m notes -- " + $p)}}]')"
	msg user 20 '[{"type":"tool_result","tool_use_id":"nested","content":"Command executed. Exit code: 0\n"}]'
	check "$L nested: relative feature path uses command cwd" "$(run PreCompact '{}' root root "$root_ws")" ""
	: >"$T"
	code_commit 10
	msg assistant 20 "$(jq -cn --arg c "$ws/memory" --arg p "$F/context.md" '[{type:"tool_use",id:"nested-memory",name:"execute_command",input:{cwd:$c,command:("git commit -m notes -- " + $p)}}]')"
	msg user 20 '[{"type":"tool_result","tool_use_id":"nested-memory","content":"Command executed. Exit code: 0\n"}]'
	check "$L nested: memory-relative feature path uses command cwd" "$(run PreCompact '{}' root root "$root_ws")" ""
	: >"$T"
	code_commit 10; edit 15 "app/memory/$F/context.md"
	check "$L nested: workspace-relative feature path releases" "$(run PreCompact '{}' root root "$root_ws")" ""
done

reset mono; code_commit 10; edit 15 "docs/feat/$F/context.md"; flush 20
check "obsolete docs/feat path no longer releases" "$(compact)" "$B"

reset; cmd 10 'git commit -m x' 1
check "failed commit is not work" "$(compact)" ""
reset; cmd 10 'ls .husky/pre-commit .git/hooks/commit-msg; A=$(rg -l commit src); echo `date`'
check "commit word without git commit is not work" "$(compact)" ""
reset; cmd 10 'cat <<EOF
$(weird
EOF'
check "unparseable shell without git commit is not work" "$(compact)" ""
reset; cmd 10 'echo "$(git commit -m code)"'
check "git commit inside substitution is work" "$(compact)" "$B"
reset; cmd 10 'cd /x && git -C repo commit -qm y'
check "git -C commit is work" "$(compact)" "$B"
reset; edit 10 src/a.ts
check "code edit is work" "$(compact)" "$B"
reset; edit 10 "memory/$F/context.md"
check "memory-only edit is not work" "$(compact)" ""
reset; edit 10 .agent-runs/triage/notes.md
check "agent working-file edit is not work" "$(compact)" ""
reset; edit 10 "$ws/tmp/run.log"
check "tmp working-file edit is not work" "$(compact)" ""
reset; edit 10 src/a.ts unchanged
check "unconfirmed edit is not work" "$(compact)" ""
reset; sub_result 10
check "wait_agent result is work" "$(compact)" "$B"
reset; sub_event 10
check "agent_event note is work" "$(compact)" "$B"
reset; edit 5 "memory/$F/context.md"; flush 6; sub_event 10; docs_cmd 20
check "subagent result then flush passes" "$(compact)" ""
reset; docs_cmd 10; code_commit 20
out=$(stop)
check "reason does not name the triggering commit" "$(grep -c -E 'a commit since|a file edit since|a subagent result since' <<<"$out" || true)" "0"
check "reason says commits and installs are not entries" "$out" "Commits, pushes and installs are not entries"
check "Stop reason allows stopping without an entry" "$out" "If nothing durable settled, just stop again"
check "PreCompact reason says condense runs after the commit" "$(compact)" "runs on its own at the next request"

config="$(dirname "$hook")/hooks.json"
check "config: PreCompact fails closed" "$(jq -r 'any(.hooks.PreCompact[]; .matcher == "manual|auto" and any(.hooks[]; (.command|rtrimstr("\"")|endswith("flush-hooks.sh")) and .onFailure == "block"))' "$config")" "true"
check "config: Stop fails open" "$(jq -r 'all(.hooks.Stop[].hooks[] | select(.command|rtrimstr("\"")|endswith("flush-hooks.sh")); .failClosed != true)' "$config")" "true"
check "config: SessionStart registered" "$(jq -r 'any(.hooks.SessionStart[].hooks[]; .command|rtrimstr("\"")|endswith("session-start-index.sh"))' "$config")" "true"

# SessionStart stays a compact discovery index, not a progress report.
cat >"$ws/memory/$F/context.md" <<'EOF'
---
title: "A deliberately long feature title that must be trimmed without exposing its progress details"
status: active
---
**Cursor:** SECRET progress and hashes
EOF
index_hook="$(dirname "$hook")/session-start-index.sh"
index=$(jq -cn --arg c "$ws" '{session_id:"root",cwd:$c}' | "$index_hook")
check "index: short heading" "$index" "Recent open work:"
check "index: rule does not tie flushing to file changes" "$(grep -c 'changes files' <<<"$index" || true)" "0"
check "index: rule says commits and installs are not entries" "$index" "Commits, pushes and installs alone are not entries"
check "index: slug-only rows" "$(printf '%s' "$index" | python3 -c 'import sys; s=sys.stdin.read(); rows=s.split("Recent open work:\n",1)[1].splitlines(); print(bool(rows) and all(r == "- feature" for r in rows))')" "True"
check "index: title and cursor omitted" "$(printf '%s' "$index" | grep -c -E 'deliberately|SECRET|20261010080000' || true)" "0"
check "index: subagents skipped" "$(jq -cn --arg c "$ws" '{session_id:"child",parent_session_id:"parent",cwd:$c}' | "$index_hook")" ""

# SessionStart matching notes: runs from a copy whose .mcp.json uses ${CLAUDE_PLUGIN_ROOT},
# omits args, and carries unresolved ${user_config.*} env placeholders (matching the packaged manifest).
plug="$tmp/plugin"; mkdir -p "$plug/hooks" "$plug/scripts"
cp "$(dirname "$hook")/session-start-index.sh" "$(dirname "$hook")/session-notes.py" "$(dirname "$hook")/transcript.py" "$plug/hooks/"
H=20261010080000-third; I=20261010080000-fourth
mkdir -p "$ws/memory/$H" "$ws/memory/$I"
printf -- '---\ntitle: "Feature title"\nstatus: active\n---\n' >"$ws/memory/$F/context.md"
printf -- '---\nstatus: done\n---\n' >"$ws/memory/$G/context.md"
printf -- '---\ntitle: "Third title"\nstatus: done\n---\n' >"$ws/memory/$H/context.md"
printf -- '---\ntitle: "Fourth title"\nstatus: done\n---\n' >"$ws/memory/$I/context.md"
cat >"$plug/scripts/server.sh" <<EOF
#!/usr/bin/env python3
import json, os, sys
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m: continue
    if m['method'] == 'tools/call':
        if os.path.exists('$tmp/engine-fails'): sys.exit(1)
        a = dict(m['params']['arguments'])
        a['_env_project'] = os.environ.get('CLAUDE_PROJECT_DIR', '')
        a['_env_scan'] = os.environ.get('DECODE_PLUGINS_SCAN_ROOT', '')
        open('$tmp/engine-args', 'a').write(json.dumps(a) + '\n')
        hits = ['$ws/memory/$F/plan.md', '$ws/memory/$F/context.md', '$ws/memory/$G/context.md', '$ws/docs/x.md', '$ws/memory/gone/context.md', '$ws/memory/$H/context.md', '$ws/memory/$I/context.md']
        text = f'{len(hits)} results:\n\n' + ''.join(f'{i}. [proj] {h}:1-9 (score 0.02)\nbody\n\n' for i, h in enumerate(hits, 1))
        result = {'content': [{'type': 'text', 'text': text}]}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': m['id'], 'result': result}), flush=True)
EOF
chmod 755 "$plug/scripts/server.sh"
cat >"$plug/.mcp.json" <<'EOF'
{
  "mcpServers": {
    "memory": {
      "type": "stdio",
      "command": "${CLAUDE_PLUGIN_ROOT}/scripts/server.sh",
      "env": {
        "DECODE_PLUGINS_SCAN_ROOT": "${user_config.scan_root}",
        "DECODE_PLUGINS_SCAN_ROOT_2": "${user_config.scan_root_2}"
      }
    }
  }
}
EOF
sindex="$plug/hooks/session-start-index.sh"
last_arg() { tail -n 1 "$tmp/engine-args"; }
ws_mem_real=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$ws/memory")

# Startup with prompt
: >"$tmp/engine-args"
out=$(DECODE_PLUGINS_SCAN_ROOT=/ignored-root jq -cn --arg c "$ws" '{session_id:"root",root_session_id:"root",cwd:$c,source:"startup",prompt:"why is it slow"}' | "$sindex")
check "notes startup: heading present" "$out" "Notes the memory search ranks best for this conversation"
check "notes startup: up to 3 folders, deduplicated, with titles" "$(sed -n '/^Notes the memory search/,$p' <<<"$out" | grep '^- ')" "- memory/$F/context.md (Feature title)
- memory/$G/context.md
- memory/$H/context.md (Third title)"
check "notes startup: no docs, plans, gone or 4th note" "$(sed -n '/^Notes the memory search/,$p' <<<"$out" | grep -c -E "gone|docs/|plan\.md|$I" || true)" "0"
check "notes startup: query, workspace and path sent to server" "$(jq -r '[.query, .workspace, .path] | join(" | ")' <<<"$(last_arg)")" "why is it slow | $ws | $ws_mem_real"
check "notes startup: plugin root expanded, project dir set, scan root stripped" "$(jq -r '[._env_project, ._env_scan] | join("|")' <<<"$(last_arg)")" "$ws|"

# Startup without prompt (or blank prompt): open work stays, no notes, server not called
: >"$tmp/engine-args"
out_nop=$(jq -cn --arg c "$ws" --arg t "$T" '{session_id:"root",root_session_id:"root",cwd:$c,source:"startup",transcript_path:$t}' | "$sindex")
out_blank=$(jq -cn --arg c "$ws" '{session_id:"root",root_session_id:"root",cwd:$c,source:"startup",prompt:"   "}' | "$sindex")
check "notes startup no prompt: open work stays" "$out_nop" "Recent open work:"
check "notes startup no prompt: no notes section" "$(grep -c 'Notes the memory search' <<<"$out_nop$out_blank" || true)" "0"
check "notes startup no prompt: server not called" "$(wc -l <"$tmp/engine-args" | tr -d ' ')" "0"

# Compact: Decode transcript (strips wrappers, ignores ui patch)
TD="$tmp/decode.jsonl"
jq -cn --argjson ts $(( (E+1)*1000 )) '{v:1,type:"patch",ts:$ts,representation:"ui",messages:[{role:"user",content:"UI-NOISE"}]}' >"$TD"
jq -cn --argjson ts $(( (E+2)*1000 )) '{v:1,type:"patch",ts:$ts,representation:"api",messages:[{role:"user",ts:$ts,content:[{type:"text",text:"<user_message>\nDecode search query\n</user_message>"},{type:"text",text:"<hook_context event=\"SessionStart\" source=\"x\">HOOK-NOISE</hook_context>"},{type:"text",text:"<environment_details>ENV-NOISE</environment_details>"}]}]}' >>"$TD"
out_d=$(jq -cn --arg c "$ws" --arg t "$TD" '{session_id:"root",root_session_id:"root",cwd:$c,source:"compact",transcript_path:$t}' | "$sindex")
check "notes compact decode: emits notes" "$out_d" "- memory/$F/context.md (Feature title)"
check "notes compact decode: clean query" "$(jq -r .query <<<"$(last_arg)")" "Decode search query"
check "notes compact decode: strips wrappers and ui" "$(jq -r .query <<<"$(last_arg)" | grep -c -E 'HOOK-NOISE|ENV-NOISE|UI-NOISE' || true)" "0"

# Compact: Claude transcript (skips isMeta, command tags, tool_result)
TC="$tmp/claude.jsonl"
cat >"$TC" <<'EOF'
{"type":"user","timestamp":"2026-11-21T10:00:00Z","isMeta":true,"message":{"role":"user","content":"META-NOISE"}}
{"type":"user","timestamp":"2026-11-21T10:00:01Z","message":{"role":"user","content":"<command-name>/compact</command-name>"}}
{"type":"user","timestamp":"2026-11-21T10:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"TOOL-NOISE"}]}}
{"type":"user","timestamp":"2026-11-21T10:00:03Z","message":{"role":"user","content":"Claude search query"}}
EOF
out_c=$(jq -cn --arg c "$ws" --arg t "$TC" '{session_id:"root",root_session_id:"root",cwd:$c,source:"compact",transcript_path:$t}' | "$sindex")
check "notes compact claude: emits notes" "$out_c" "- memory/$F/context.md (Feature title)"
check "notes compact claude: clean query" "$(jq -r .query <<<"$(last_arg)")" "Claude search query"
check "notes compact claude: skips meta and tool noise" "$(jq -r .query <<<"$(last_arg)" | grep -c -E 'META-NOISE|TOOL-NOISE|command-name' || true)" "0"

# Compact: Codex transcript (response_item message input_text, skips AGENTS.md and environment_context)
TX="$tmp/codex.jsonl"
cat >"$TX" <<'EOF'
{"type":"response_item","timestamp":"2026-11-21T10:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /w\n<INSTRUCTIONS>AGENTS-NOISE</INSTRUCTIONS>"}]}}
{"type":"response_item","timestamp":"2026-11-21T10:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context><cwd>/w</cwd></environment_context>"}]}}
{"type":"response_item","timestamp":"2026-11-21T10:00:02Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Codex search query"}]}}
EOF
out_x=$(jq -cn --arg c "$ws" --arg t "$TX" '{session_id:"root",root_session_id:"root",cwd:$c,source:"compact",transcript_path:$t}' | "$sindex")
check "notes compact codex: emits notes" "$out_x" "- memory/$F/context.md (Feature title)"
check "notes compact codex: clean query" "$(jq -r .query <<<"$(last_arg)")" "Codex search query"
check "notes compact codex: skips agents and env noise" "$(jq -r .query <<<"$(last_arg)" | grep -c -E 'AGENTS-NOISE|environment_context' || true)" "0"

# Resume uses transcript too
out_r=$(jq -cn --arg c "$ws" --arg t "$TC" '{session_id:"root",root_session_id:"root",cwd:$c,source:"resume",transcript_path:$t}' | "$sindex")
check "notes resume: emits notes" "$out_r" "- memory/$F/context.md (Feature title)"

# Subagents skipped even with prompt
: >"$tmp/engine-args"
check "notes subagent parent_session_id skipped" "$(jq -cn --arg c "$ws" '{session_id:"child",parent_session_id:"parent",cwd:$c,source:"startup",prompt:"x"}' | "$sindex")" ""
check "notes subagent root_session_id skipped" "$(jq -cn --arg c "$ws" '{session_id:"child",root_session_id:"root",cwd:$c,source:"startup",prompt:"x"}' | "$sindex")" ""
check "notes subagent did not call server" "$(wc -l <"$tmp/engine-args" | tr -d ' ')" "0"

# Engine failure prints the rest silently and exits 0
touch "$tmp/engine-fails"
out_f=$(jq -cn --arg c "$ws" '{session_id:"root",root_session_id:"root",cwd:$c,source:"startup",prompt:"why"}' | "$sindex"; echo "rc=$?")
check "notes failure: exits 0" "$out_f" "rc=0"
check "notes failure: keeps open work" "$out_f" "Recent open work:"
check "notes failure: omits notes heading" "$(grep -c 'Notes the memory search' <<<"$out_f" || true)" "0"

rm -rf "$tmp"
echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
