#!/usr/bin/env python3
"""Past-chat index for the memory plugin.

`sync` turns the harness's own session transcripts into plain markdown that the
semantic engine indexes like any other notes folder.

Only user and assistant prose is extracted. Tool calls and outputs, reasoning
and injected system/environment blocks are dropped. Output layout, one folder
per project so the engine's `project` filter works:

    <out>/<project>/chats/<session>--pNNN.md

Each part starts with a header naming the session and its source file; each
message gets `## m<n> <role> (line <L>)`, where L is the line in the source
JSONL, so the original (with tool calls) can be read at that line.
"""
import json, os, pathlib, re, sys, tempfile

PART_LIMIT = 900_000  # the engine silently skips files over 1,000,000 bytes
MANIFEST = '.manifest.json'
WRAPPED = re.compile(r'\s*<([A-Za-z][\w-]*)[\s>].*</\1>\s*\Z', re.S)
REMINDER = re.compile(r'<system-reminder>.*?</system-reminder>', re.S)
INJECTED_PREFIXES = ('# AGENTS.md instructions',)


def user_prose(text):
    text = REMINDER.sub('', text).strip()
    if not text or text.startswith(INJECTED_PREFIXES) or WRAPPED.match(text):
        return ''
    return text


def assistant_prose(text):
    return text.strip()


def claude_sessions(root):
    yield from sorted((root / 'projects').glob('*/*.jsonl'))


def codex_sessions(root):
    yield from sorted((root / 'sessions').rglob('*.jsonl'))


def parse_claude(path):
    """-> (session, cwd, [(line, role, timestamp, text)])"""
    cwd, messages = '', []
    with open(path, encoding='utf-8') as stream:
        for line_no, raw in enumerate(stream, 1):
            try:
                rec = json.loads(raw)
            except ValueError:
                continue
            cwd = cwd or rec.get('cwd') or ''
            role = rec.get('type')
            if role not in ('user', 'assistant') or rec.get('isMeta') or rec.get('isSidechain'):
                continue
            message = rec.get('message') or {}
            if message.get('model') == '<synthetic>':
                continue
            content = message.get('content')
            if isinstance(content, str):
                content = [{'type': 'text', 'text': content}]
            clean = user_prose if role == 'user' else assistant_prose
            texts = [clean(b.get('text') or '') for b in content or [] if isinstance(b, dict) and b.get('type') == 'text']
            text = '\n\n'.join(t for t in texts if t)
            if text:
                messages.append((line_no, role, rec.get('timestamp') or '', text))
    return path.stem, cwd, messages


def parse_codex(path):
    session, cwd, messages = path.stem, '', []
    with open(path, encoding='utf-8') as stream:
        for line_no, raw in enumerate(stream, 1):
            try:
                rec = json.loads(raw)
            except ValueError:
                continue
            payload = rec.get('payload') or {}
            if rec.get('type') == 'session_meta':
                session = payload.get('id') or session
                cwd = payload.get('cwd') or ''
                continue
            if rec.get('type') != 'response_item' or payload.get('type') != 'message':
                continue
            role = payload.get('role')
            if role not in ('user', 'assistant'):
                continue
            clean = user_prose if role == 'user' else assistant_prose
            texts = [clean(b.get('text') or '') for b in payload.get('content') or [] if isinstance(b, dict)]
            text = '\n\n'.join(t for t in texts if t)
            if text:
                messages.append((line_no, role, rec.get('timestamp') or '', text))
    return session, cwd, messages


# harness -> (session lister, parser, env var that relocates its data folder, default folder under home)
HARNESSES = {'claude': (claude_sessions, parse_claude, 'CLAUDE_CONFIG_DIR', '.claude'),
             'codex': (codex_sessions, parse_codex, 'CODEX_HOME', '.codex')}


def project_name(cwd):
    cwd = cwd.split('/.agent-runs/')[0].rstrip('/')
    name = re.sub(r'[^A-Za-z0-9._-]+', '-', os.path.basename(cwd)).strip('-.')
    return name or 'unknown'


def render_parts(harness, source, session, cwd, messages):
    header = f'# {harness} session {session}\nsource: {source}\ncwd: {cwd}\n\n'
    parts, current, size = [], [], len(header)
    for n, (line_no, role, ts, text) in enumerate(messages):
        head = f'## m{n} {role} (line {line_no}, {ts})\n'
        room = PART_LIMIT - len(header) - len(head) - 2
        # A single message larger than a part is cut into consecutive pieces.
        pieces = [text[i:i + room] for i in range(0, len(text), room)]
        for piece in pieces:
            block = f'{head}{piece}\n\n'
            if current and size + len(block) > PART_LIMIT:
                parts.append(current)
                current, size = [], len(header)
            current.append(block)
            size += len(block)
    if current:
        parts.append(current)
    return [header + ''.join(blocks) for blocks in parts]


def write_if_changed(path, text):
    data = text.encode('utf-8')
    if path.exists() and path.read_bytes() == data:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix='.tmp-')
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    os.replace(tmp, path)
    return True


def sync(harness, out, home=None):
    list_sources, parse, env_var, folder = HARNESSES[harness]
    root = pathlib.Path(home) / folder if home else pathlib.Path(os.environ.get(env_var) or pathlib.Path.home() / folder)
    out = pathlib.Path(out)
    manifest_path = out / MANIFEST
    old = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
    new, changed = {}, 0
    for source in list_sources(root):
        stat = source.stat()
        stamp = [stat.st_size, stat.st_mtime_ns]
        entry = old.get(str(source))
        if entry and entry['stamp'] == stamp:
            new[str(source)] = entry
            continue
        session, cwd, messages = parse(source)
        files = []
        if messages:
            folder = out / project_name(cwd) / 'chats'
            for number, text in enumerate(render_parts(harness, source, session, cwd, messages), 1):
                target = folder / f'{session}--p{number:03d}.md'
                write_if_changed(target, text)
                files.append(str(target.relative_to(out)))
        new[str(source)] = {'stamp': stamp, 'files': files}
        changed += 1
    changed += len(old.keys() - new.keys())
    # A file may now belong to another source (the same session reached through a moved folder), so keep every file still listed.
    kept = {name for entry in new.values() for name in entry['files']}
    for entry in old.values():
        for stale in set(entry['files']) - kept:
            (out / stale).unlink(missing_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    write_if_changed(manifest_path, json.dumps(new, indent=1, sort_keys=True))
    return len(new), changed


if __name__ == '__main__':
    command = sys.argv[1] if len(sys.argv) > 1 else ''
    if command == 'sync' and len(sys.argv) == 4 and sys.argv[2] in HARNESSES:
        sessions, changed = sync(sys.argv[2], sys.argv[3])
        print(f'{sessions} sessions, {changed} re-extracted', file=sys.stderr)
    else:
        raise SystemExit('usage: chat-index.py sync claude|codex <out-dir>')
