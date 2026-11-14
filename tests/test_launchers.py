#!/usr/bin/env python3
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / '.agent-runs' / 'launcher-tests'
FIXTURES.mkdir(parents=True, exist_ok=True)
ASSET = 'semantic-mcp-aarch64-apple-darwin'
FAKE_BINARY = b'#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n'


class Launchers(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=FIXTURES)
        self.root = Path(self.temp.name)
        self.home = self.root / 'home with spaces'
        self.bin = self.root / 'bin'
        self.home.mkdir()
        self.bin.mkdir()
        self.project = self.root / 'project with spaces'
        self.project.mkdir()
        self.log = self.root / 'downloads'
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.bin) + ':' + os.environ['PATH'],
                        CLAUDE_PLUGIN_DATA=str(self.root / 'plugin data'), DOWNLOAD_LOG=str(self.log))
        self.write_executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.write_executable(self.bin / 'gh', '''#!/usr/bin/env python3
import hashlib, os, pathlib, sys
args=sys.argv[1:]
if args[:2] == ['auth','status']:
    raise SystemExit(1 if os.environ.get('AUTH_FAIL') else 0)
with open(os.environ['DOWNLOAD_LOG'],'a') as f: f.write(repr(args)+'\\n')
if os.environ.get('DOWNLOAD_FAIL'): raise SystemExit(1)
out=pathlib.Path(args[args.index('--dir')+1])
asset='semantic-mcp-aarch64-apple-darwin'
binary=b'#!/usr/bin/env python3\\nimport json,sys\\nprint(json.dumps(sys.argv[1:]))\\n'
(out/asset).write_bytes(binary)
digest='0'*64 if os.environ.get('BAD_CHECKSUM') else hashlib.sha256(binary).hexdigest()
(out/'SHA256SUMS').write_text(digest+'  '+asset+'\\n')
''')

    def tearDown(self):
        self.temp.cleanup()

    def write_executable(self, path, text):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o755)
        return path

    def run_server(self, plugin='semantic-search', **env):
        return subprocess.run([str(ROOT / 'plugins' / plugin / 'scripts/server.sh')], cwd=self.project,
                              env=dict(self.env, **env), text=True, capture_output=True)

    @property
    def shared(self):
        return self.home / '.local/share/decode-plugins/bin/semantic-mcp'

    def test_download_checksum_cache_and_project_scope(self):
        result = self.run_server()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        self.assertEqual(args[args.index('--project') + 1], str(self.project))
        self.assertEqual(args[args.index('--folder') + 1], '.')
        self.assertNotIn('--scan', args)
        self.assertEqual(self.shared.read_bytes(), FAKE_BINARY)
        self.assertEqual(list((Path(self.env['CLAUDE_PLUGIN_DATA']) / 'releases/v0.1.5').glob('download.*')), [])
        self.assertIn("'--repo', 'tonioriol/decode-plugins'", self.log.read_text())
        self.assertIn("'v0.1.5'", self.log.read_text())
        self.assertEqual(self.run_server(AUTH_FAIL='1', DOWNLOAD_FAIL='1').returncode, 0)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)

    def test_auth_failure_explains_private_access(self):
        result = self.run_server(AUTH_FAIL='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('gh auth login', result.stderr)
        self.assertFalse(self.shared.exists())
        self.assertFalse(self.log.exists())

    def test_missing_asset_is_actionable(self):
        result = self.run_server(DOWNLOAD_FAIL='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('available platform assets', result.stderr)
        self.assertFalse(self.shared.exists())

    def test_checksum_failure_never_publishes_binary(self):
        result = self.run_server(BAD_CHECKSUM='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertFalse(self.shared.exists())
        self.assertFalse((Path(self.env['CLAUDE_PLUGIN_DATA']) / 'releases/v0.1.5' / ASSET).exists())

    def test_memory_prefers_newest_decode_without_downloading(self):
        old = self.write_executable(self.home / '.vscode/extensions/ultra.decode-old/dist/bin/decode',
                                    '#!/bin/sh\necho old\n')
        new = self.write_executable(self.home / '.vscode/extensions/ultra.decode-new/dist/bin/decode',
                                    FAKE_BINARY.decode())
        os.utime(old, (1, 1))
        os.utime(new, (2, 2))
        self.write_executable(self.shared, '#!/bin/sh\necho shared\n')
        result = self.run_server('memory', AUTH_FAIL='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        self.assertEqual(args[0], 'semantic-mcp')
        self.assertIn('--docs-only', args)
        self.assertEqual([args[i+1] for i, arg in enumerate(args) if arg == '--folder'], ['memory', 'docs=0.5'])
        self.assertFalse(self.log.exists())

    def test_memory_uses_shared_standalone_without_gh(self):
        self.write_executable(self.shared, FAKE_BINARY.decode())
        result = self.run_server('memory', AUTH_FAIL='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        self.assertEqual(args[0], '--storage')
        self.assertFalse(self.log.exists())

    def test_scan_roots_are_single_arguments_without_shell_evaluation(self):
        roots = ['/projects/space here', '/projects/$(touch unwanted)']
        for plugin in ('semantic-search', 'memory'):
            result = self.run_server(plugin, DECODE_PLUGINS_SCAN_ROOT=roots[0], DECODE_PLUGINS_SCAN_ROOT_2=roots[1])
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(result.stdout)
            self.assertEqual([args[i+1] for i, arg in enumerate(args) if arg == '--scan'], roots)
        self.assertFalse((self.project / 'unwanted').exists())

    def test_explicit_project_directory_overrides_process_working_directory(self):
        project = self.root / 'explicit project'
        project.mkdir()
        result = self.run_server(CLAUDE_PROJECT_DIR=str(project))
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        self.assertEqual(args[args.index('--project') + 1], str(project))

    def test_codex_fallback_keeps_code_and_document_indexes_separate(self):
        env = dict(self.env)
        env.pop('CLAUDE_PLUGIN_DATA')
        stores = []
        for plugin in ('semantic-search', 'memory'):
            result = subprocess.run([str(ROOT / 'plugins' / plugin / 'scripts/server.sh')],
                                    cwd=self.project, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(result.stdout)
            stores.append(args[args.index('--storage') + 1])
        self.assertNotEqual(stores[0], stores[1])

    def test_codex_manifests_launch_from_installed_cache_and_preserve_project(self):
        codex_home = self.root / 'codex home with spaces'
        for plugin in ('semantic-search', 'memory'):
            source = ROOT / 'plugins' / plugin
            manifest = json.loads((source / '.codex-plugin/plugin.json').read_text())
            installed = codex_home / 'plugins/cache/decode-plugins' / plugin / manifest['version']
            shutil.copytree(source, installed)
            server = manifest['mcpServers'][plugin]
            transport_env = dict(self.env)
            transport_env.pop('CODEX_HOME', None)
            if 'CODEX_HOME' in server.get('env_vars', []):
                transport_env['CODEX_HOME'] = str(codex_home)
            result = subprocess.run([server['command'], *server.get('args', [])], cwd=self.project,
                                    env=transport_env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(result.stdout)
            self.assertEqual(args[args.index('--project') + 1], str(self.project))
            self.assertEqual(args[args.index('--tool') + 1], 'search' if plugin == 'memory' else 'project_search')

    def test_concurrent_downloads_publish_complete_binary(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: self.run_server(), range(2)))
        for result in results:
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('--project', json.loads(result.stdout))
        self.assertEqual(self.shared.read_bytes(), FAKE_BINARY)
        self.assertEqual(hashlib.sha256(self.shared.read_bytes()).digest(), hashlib.sha256(FAKE_BINARY).digest())


if __name__ == '__main__':
    unittest.main()
