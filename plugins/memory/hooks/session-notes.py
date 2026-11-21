#!/usr/bin/env python3
"""The notes in <workspace>/memory that best match this conversation, one line per note folder, best first.

    session-notes.py <workspace> prompt <text>
    session-notes.py <workspace> transcript <path>

A transcript (compact, resume) is searched by its latest user messages; a startup passes its opening text.
Searches through this plugin's own memory MCP server, so it reads the same index the search tool keeps.
"""
import json
import os
import pathlib
import re
import signal
import subprocess
import sys
import threading

from transcript import messages

LIMIT = 2000  # query characters
# Text the client wraps around a user's words, never written by the user.
WRAPPERS = re.compile(r'<(environment_details|environment_context|hook_context|system-reminder|event)\b[^>]*>.*?</\1>', re.S)
CLIENT_TEXT = ('# AGENTS.md instructions', '<environment_context>', '<user_instructions>', '<command-', '<local-command',
               '[TASK RESUMPTION]')


def user_texts(path):
    for message in messages(path):
        if message.get('role') != 'user':
            continue
        content = message.get('content', '')
        for text in [content] if isinstance(content, str) else [i.get('text', '') for i in content if i.get('type') == 'text']:
            text = re.sub(r'</?user_message>|^\[t=\d+\]\s*', '', WRAPPERS.sub('', text), flags=re.M).strip()
            if text and not text.startswith(CLIENT_TEXT):
                yield text


def query(kind, source):
    if kind == 'prompt':
        return source[:LIMIT]
    picked, size = [], 0
    for text in reversed(list(user_texts(source))):
        picked.append(text[:LIMIT - size])
        size += len(picked[-1])
        if size >= LIMIT:
            break
    return '\n'.join(reversed(picked))


def search(text, folder, workspace):
    root = pathlib.Path(__file__).resolve().parent.parent
    server = json.loads((root / '.mcp.json').read_text())['mcpServers']['memory']
    expand = lambda value: str(value).replace('${CLAUDE_PLUGIN_ROOT}', str(root))
    cmd = [expand(server['command']), *(expand(arg) for arg in server.get('args', ()))]
    env = dict(os.environ, CLAUDE_PROJECT_DIR=workspace)
    # Scope the SessionStart lookup to this workspace so broad scan roots never slow down session startup.
    env.pop('DECODE_PLUGINS_SCAN_ROOT', None)
    env.pop('DECODE_PLUGINS_SCAN_ROOT_2', None)
    for key, value in server.get('env', {}).items():
        expanded = expand(value)
        if '${' not in expanded:
            env[key] = expanded

    timeout = float(os.environ.get('MEMORY_SESSION_NOTES_TIMEOUT', '6'))
    proc = subprocess.Popen(cmd, cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True, start_new_session=True)

    def stop(sig=signal.SIGTERM):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            pass

    timer = threading.Timer(timeout, stop)
    timer.daemon = True
    timer.start()

    def call(id, method, params):
        proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params}) + '\n')
        proc.stdin.flush()
        for line in proc.stdout:
            reply = json.loads(line)
            if reply.get('id') == id:
                return reply['result']
        raise SystemExit('memory server exited')

    try:
        call(1, 'initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                               'clientInfo': {'name': 'memory-session-notes', 'version': '1'}})
        proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
        result = call(2, 'tools/call', {'name': 'search',
                                        'arguments': {'query': text, 'workspace': workspace, 'path': folder, 'limit': 6}})
    finally:
        timer.cancel()
        try:
            proc.stdin.close()
        except OSError:
            pass
        stop()
        try:
            proc.wait(timeout=1)
        except subprocess.TimeoutExpired:
            stop(signal.SIGKILL)
    body = ''.join(part.get('text', '') for part in result['content'])
    return re.findall(r'^\d+\. \[[^\]]*\] (/.+?):\d+-\d+ \(score', body, re.M)


workspace, kind, source = sys.argv[1:4]
text = query(kind, source)
if not text.strip():
    sys.exit()
memory = os.path.realpath(os.path.join(workspace, 'memory'))
seen = []
for hit in search(text, memory, workspace):
    rel = os.path.relpath(os.path.realpath(hit), memory).split(os.sep)
    if len(rel) < 2 or rel[0] in ('', '.', '..') or rel[0] in seen:
        continue
    note = os.path.join(memory, rel[0], 'context.md')
    if not os.path.isfile(note):
        continue
    seen.append(rel[0])
    match = re.search(r'^title:\s*(.+?)\s*$', pathlib.Path(note).read_text(), re.M)
    title = match.group(1).strip('"\' ') if match else ''
    print(f'- memory/{rel[0]}/context.md' + (f' ({title})' if title else ''))
    if len(seen) == 3:
        break
