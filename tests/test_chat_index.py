#!/usr/bin/env python3
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'plugins' / 'memory' / 'scripts' / 'chat-index.py'
FIXTURES = ROOT / '.agent-runs' / 'launcher-tests'
FIXTURES.mkdir(parents=True, exist_ok=True)

spec = importlib.util.spec_from_file_location('chat_index', SCRIPT)
chat_index = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chat_index)


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(''.join(json.dumps(r) + '\n' for r in records))


def claude(role, content, **extra):
    return dict({'type': role, 'cwd': '/w/proj', 'timestamp': 't', 'message': {'role': role, 'content': content}}, **extra)


def codex_message(role, text):
    kind = 'output_text' if role == 'assistant' else 'input_text'
    return {'type': 'response_item', 'timestamp': 't',
            'payload': {'type': 'message', 'role': role, 'content': [{'type': kind, 'text': text}]}}


class ChatIndex(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=FIXTURES)
        self.home = Path(self.temp.name) / 'home'
        self.out = Path(self.temp.name) / 'out'

    def tearDown(self):
        self.temp.cleanup()

    def extracted(self):
        return {str(p.relative_to(self.out)): p.read_text() for p in sorted(self.out.rglob('*.md'))}

    def test_claude_keeps_only_prose(self):
        session = self.home / '.claude/projects/-w-proj/abc.jsonl'
        write_jsonl(session, [
            {'type': 'attachment', 'attachment': {'content': 'hook text'}},
            claude('user', 'why is the cache cold?'),
            claude('user', '<system-reminder>injected</system-reminder>\nkeep the real question'),
            claude('user', '<local-command-stdout>Login ok</local-command-stdout>'),
            claude('user', 'skill body', isMeta=True),
            claude('assistant', [{'type': 'thinking', 'thinking': 'secret'},
                                 {'type': 'text', 'text': 'Because the index was rebuilt.'},
                                 {'type': 'tool_use', 'name': 'Bash', 'input': {'command': 'ls'}}]),
            claude('user', [{'type': 'tool_result', 'content': 'tool output'}]),
            claude('assistant', 'subagent chatter', isSidechain=True),
        ])
        chat_index.sync('claude', self.out, self.home)
        text = self.extracted()['proj/chats/abc--p001.md']
        self.assertIn('# claude session abc', text)
        self.assertIn(f'source: {session}', text)
        self.assertIn('## m0 user (line 2, t)\nwhy is the cache cold?', text)
        self.assertIn('## m1 user (line 3, t)\nkeep the real question', text)
        self.assertIn('## m2 assistant (line 6, t)\nBecause the index was rebuilt.', text)
        for dropped in ('hook text', 'injected', 'Login ok', 'skill body', 'secret', 'Bash', 'tool output', 'subagent chatter'):
            self.assertNotIn(dropped, text)

    def test_codex_keeps_only_user_and_assistant_prose(self):
        session = self.home / '.codex/sessions/2026/11/15/rollout-x.jsonl'
        write_jsonl(session, [
            {'type': 'session_meta', 'payload': {'id': 'sid-1', 'cwd': '/w/Code/app/.agent-runs/task'}},
            codex_message('developer', 'permissions instructions'),
            codex_message('user', '<environment_context>\n<cwd>/w</cwd>\n</environment_context>'),
            codex_message('user', '# AGENTS.md instructions for /w\n\n<INSTRUCTIONS>rules</INSTRUCTIONS>'),
            codex_message('user', 'what backoff did we choose?'),
            codex_message('assistant', 'Six retries with full jitter.'),
            {'type': 'response_item', 'payload': {'type': 'function_call', 'name': 'exec', 'arguments': '{}'}},
            {'type': 'response_item', 'payload': {'type': 'reasoning', 'summary': []}},
        ])
        chat_index.sync('codex', self.out, self.home)
        text = self.extracted()['app/chats/sid-1--p001.md']
        self.assertIn('## m0 user (line 5, t)\nwhat backoff did we choose?', text)
        self.assertIn('## m1 assistant (line 6, t)\nSix retries with full jitter.', text)
        for dropped in ('permissions instructions', 'environment_context', 'AGENTS.md', 'exec'):
            self.assertNotIn(dropped, text)

    def test_large_chat_is_split_below_the_engine_file_limit(self):
        session = self.home / '.claude/projects/-w-proj/big.jsonl'
        write_jsonl(session, [claude('user' if i % 2 == 0 else 'assistant', f'message {i} ' + 'x' * 100_000) for i in range(30)])
        chat_index.sync('claude', self.out, self.home)
        parts = self.extracted()
        self.assertGreater(len(parts), 1)
        for name, text in parts.items():
            self.assertLessEqual(len(text.encode()), 1_000_000, name)
            self.assertTrue(text.startswith('# claude session big'), name)
        self.assertEqual(sum(t.count('\n## m') for t in parts.values()), 30)

    def test_sync_is_incremental_and_removes_stale_extracts(self):
        first = self.home / '.claude/projects/-w-proj/one.jsonl'
        second = self.home / '.claude/projects/-w-proj/two.jsonl'
        write_jsonl(first, [claude('user', 'first question')])
        write_jsonl(second, [claude('user', 'second question')])
        self.assertEqual(chat_index.sync('claude', self.out, self.home), (2, 2))
        self.assertEqual(chat_index.sync('claude', self.out, self.home), (2, 0))
        write_jsonl(first, [claude('user', 'first question'), claude('assistant', 'first answer')])
        self.assertEqual(chat_index.sync('claude', self.out, self.home), (2, 1))
        self.assertIn('first answer', self.extracted()['proj/chats/one--p001.md'])
        second.unlink()
        self.assertEqual(chat_index.sync('claude', self.out, self.home), (1, 1))
        self.assertEqual(list(self.extracted()), ['proj/chats/one--p001.md'])

    def test_session_without_prose_leaves_no_file(self):
        write_jsonl(self.home / '.claude/projects/-w-proj/empty.jsonl', [claude('user', '<local-command-stdout>x</local-command-stdout>')])
        chat_index.sync('claude', self.out, self.home)
        self.assertEqual(self.extracted(), {})

    def test_stub_serves_an_mcp_server_without_tools(self):
        requests = '\n'.join(json.dumps(r) for r in [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-06-18'}},
            {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'},
        ]) + '\n'
        out = subprocess.run(['python3', str(SCRIPT), 'stub'], input=requests, text=True, capture_output=True).stdout
        replies = [json.loads(line) for line in out.splitlines()]
        self.assertEqual([r['id'] for r in replies], [1, 2])
        self.assertEqual(replies[1]['result'], {'tools': []})


class ChatsServerSwitch(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=FIXTURES)
        self.home = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def tools(self, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith('DECODE_PLUGINS')}
        base.update(HOME=str(self.home), CLAUDE_PLUGIN_DATA=str(self.home / 'data'), XDG_CONFIG_HOME=str(self.home / 'config'))
        base.update(env)
        request = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list'}) + '\n'
        init = json.dumps({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {}}) + '\n'
        run = subprocess.run([str(ROOT / 'plugins/memory/scripts/chats-server.sh')], input=init + request, text=True,
                             capture_output=True, env=base, timeout=20)
        return run

    def test_off_by_default_and_when_false(self):
        for flag in ({}, {'DECODE_PLUGINS_INDEX_CHATS': 'false'}):
            run = self.tools(**flag)
            self.assertEqual(json.loads(run.stdout.splitlines()[1])['result'], {'tools': []})
            self.assertFalse((self.home / 'data').exists())


if __name__ == '__main__':
    unittest.main()
