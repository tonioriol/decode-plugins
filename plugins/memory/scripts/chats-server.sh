#!/usr/bin/env bash
# Past-chat search: extracts this harness's own session transcripts (user and
# assistant prose) into a folder of markdown and serves it with the same engine
# as the notes search.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
harness=${DECODE_PLUGINS_HARNESS:-claude}
# Each harness keeps its own chats index; the model is shared with the notes search.
chats=${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/decode-plugins/memory}/chats
notes=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}/memory

corpus="$chats/corpus"
mkdir -p "$corpus"

# Reuse the embedding model the notes search already downloaded (the engine re-verifies its checksum).
for model in "$notes"/semble-model-*; do
  [[ -d $model && ! -e $chats/$(basename "$model") ]] && { cp -cR "$model" "$chats/" 2>/dev/null || cp -R "$model" "$chats/"; }
done

python3 "$here/chat-index.py" sync "$harness" "$corpus"
# The engine re-reads the folder on every search, so refreshing the extract keeps it current.
owner=$$
( while kill -0 "$owner" 2>/dev/null; do sleep 30; python3 "$here/chat-index.py" sync "$harness" "$corpus" 2>/dev/null || true; done ) &
refresher=$!
trap 'kill "$refresher" 2>/dev/null || true' EXIT

# Name the chats the way this harness's own UI does, so the agent connects the tool to "my earlier sessions".
case $harness in
  codex) chat_name="Codex sessions" ;;
  *) chat_name="Claude Code sessions (conversations)" ;;
esac

"$here/decode-semantic.sh" serve --storage "$chats" --scan "$corpus" --depth 1 --folder chats --docs-only --tool search_chats \
  --description "Search your past ${chat_name} with the user, by meaning. Use it when the user refers to earlier work (\"like last time\", \"we already fixed this\", \"what did we decide about X\") or you need what was tried or decided before and the memory notes don't say. Indexed: user and assistant text only (tool calls, tool output and reasoning are not). Each hit is a path:lines range of a text extract. The first lines of that file name the session and its source transcript; the '## mN role (line L, time)' headers give the message position, with L the line in the source transcript. To read around a hit, read a wider line range of the extract file; read the source transcript at line L when you need the tool calls. Pass project (the folder name shown in brackets) to filter, or workspace to rank a project first."
