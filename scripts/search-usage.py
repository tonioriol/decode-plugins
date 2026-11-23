#!/usr/bin/env python3
"""How often agents call the plugins' search tools, read from this machine's Claude Code and Codex transcripts.

usage: search-usage.py [days]   (default 7)

Per harness and day: sessions, then for each tool the sessions that called it and the total calls.
A Codex code-mode call is counted from the tool name inside the script it ran."""
import datetime
import json
import os
import pathlib
import re
import sys
from collections import defaultdict

# A tool identifier followed by a call or the end of the name (a Claude tool name, a Codex tool name or a
# `tools.<name>(` call in a code-mode script), never a mention in prose or a string.
CALLED = r'(?=\s*\(|\s|$)'
TOOLS = {'search_chats': re.compile(r'(?:^|\btools\.|__)search_chats' + CALLED),
         'memory search': re.compile(r'memory__search' + CALLED),
         'project_search': re.compile(r'(?:^|\btools\.|__)project_search' + CALLED)}


def claude_calls(path):
    with open(path, encoding='utf-8') as stream:
        for line in stream:
            if '"tool_use"' not in line:
                continue
            content = json.loads(line).get('message', {}).get('content')
            for block in content if isinstance(content, list) else []:
                if block.get('type') == 'tool_use':
                    yield block.get('name', '')


def codex_calls(path):
    with open(path, encoding='utf-8') as stream:
        for line in stream:
            if '"response_item"' not in line:
                continue
            payload = json.loads(line).get('payload', {})
            if payload.get('type') in ('function_call', 'custom_tool_call', 'mcp_tool_call'):
                yield ' '.join(str(payload.get(key, '')) for key in ('name', 'server', 'tool', 'input', 'arguments'))


def main():
    days = int(sys.argv[1]) if len(sys.argv) > 1 else 7
    since = datetime.datetime.now().timestamp() - days * 86400
    claude = pathlib.Path(os.environ.get('CLAUDE_CONFIG_DIR') or pathlib.Path.home() / '.claude') / 'projects'
    codex = pathlib.Path(os.environ.get('CODEX_HOME') or pathlib.Path.home() / '.codex') / 'sessions'
    table = defaultdict(lambda: {'sessions': 0, **{tool: [0, 0] for tool in TOOLS}})
    for harness, files, calls in (('claude', claude.glob('*/*.jsonl'), claude_calls), ('codex', codex.rglob('*.jsonl'), codex_calls)):
        for path in files:
            mtime = path.stat().st_mtime
            if mtime < since:
                continue
            row = table[(harness, datetime.date.fromtimestamp(mtime).isoformat())]
            row['sessions'] += 1
            counts = defaultdict(int)
            for call in calls(path):
                for tool, pattern in TOOLS.items():
                    counts[tool] += len(pattern.findall(call))
            for tool, count in counts.items():
                if count:
                    row[tool][0] += 1
                    row[tool][1] += count
    print('harness  day         sessions  ' + '  '.join(f'{tool} (sessions/calls)' for tool in TOOLS))
    for (harness, day), row in sorted(table.items()):
        print(f'{harness:8} {day}  {row["sessions"]:8}  ' + '  '.join(f'{"%d/%d" % tuple(row[tool]):>{len(tool) + 17}}' for tool in TOOLS))


if __name__ == '__main__':
    main()
