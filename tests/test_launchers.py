#!/usr/bin/env python3
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / '.agent-runs' / 'launcher-tests'
FIXTURES.mkdir(parents=True, exist_ok=True)
RELEASE = 'v0.1.2'
ASSET = 'decode-semantic-aarch64-apple-darwin'
FAKE_BINARY = b'#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n'
# Writes the release files the way gh (into --dir) or curl (to -o) would, logging each call.
FAKE_DOWNLOAD = '''#!/usr/bin/env python3
import hashlib, os, pathlib, sys
tool, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
if args[:2] == ['auth', 'status']:
    raise SystemExit(1 if os.environ.get('AUTH_FAIL') else 0)
with open(os.environ['DOWNLOAD_LOG'], 'a') as f: f.write(tool + ' ' + repr(args) + '\\n')
if os.environ.get('DOWNLOAD_FAIL'): raise SystemExit(1)
if os.environ.get('DOWNLOAD_HANG'): __import__('time').sleep(60)
binary = %r
digest = '0' * 64 if os.environ.get('BAD_CHECKSUM') else hashlib.sha256(binary).hexdigest()
asset = 'decode-semantic-' + {'Darwin': 'aarch64-apple-darwin', 'Linux': 'aarch64-unknown-linux-gnu'}[os.popen('uname -s').read().strip()]
files = {asset: binary, 'SHA256SUMS': (digest + '  ' + asset + '\\n').encode()}
if tool == 'gh':
    out = pathlib.Path(args[args.index('--dir') + 1])
    for name, body in files.items(): (out / name).write_bytes(body)
else:
    pathlib.Path(args[args.index('-o') + 1]).write_bytes(files[args[-1].rsplit('/', 1)[1]])
''' % FAKE_BINARY


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
                        CLAUDE_PLUGIN_DATA=str(self.root / 'plugin data'), DOWNLOAD_LOG=str(self.log),
                        TMPDIR=str(self.root / 'tmp'))
        (self.root / 'tmp').mkdir()
        for name in ('DECODE_PLUGINS_DATA', 'XDG_CONFIG_HOME', 'CLAUDE_PROJECT_DIR'):
            self.env.pop(name, None)
        self.write_executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.write_executable(self.bin / 'gh', FAKE_DOWNLOAD)
        self.write_executable(self.bin / 'curl', FAKE_DOWNLOAD)

    def tearDown(self):
        self.temp.cleanup()

    def write_executable(self, path, text):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o755)
        return path

    def run_server(self, plugin='semantic-search', script='server.sh', **env):
        return subprocess.run([str(ROOT / 'plugins' / plugin / 'scripts' / script)], cwd=self.project,
                              env=dict(self.env, **env), text=True, capture_output=True)

    def args(self, plugin='semantic-search', **env):
        result = self.run_server(plugin, **env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    @staticmethod
    def values(args, flag):
        return [args[i + 1] for i, arg in enumerate(args) if arg == flag]

    @property
    def data(self):
        return self.home / '.decode/plugin-data'

    @property
    def installed(self):
        return self.data / 'bin' / RELEASE / ASSET

    def test_download_checksum_cache_and_project_scope(self):
        args = self.args()
        self.assertEqual(args[0], 'serve')
        self.assertEqual(self.values(args, '--project'), [str(self.project)])
        self.assertEqual(self.values(args, '--folder'), ['.'])
        self.assertNotIn('--scan', args)
        self.assertEqual(self.installed.read_bytes(), FAKE_BINARY)
        self.assertEqual(list(self.installed.parent.iterdir()), [self.installed])
        self.assertEqual(list((self.root / 'tmp').iterdir()), [])
        self.assertIn(f"gh ['release', 'download', '{RELEASE}', '--repo', 'tonioriol/decode-plugins'", self.log.read_text())
        self.assertEqual(self.run_server(AUTH_FAIL='1', DOWNLOAD_FAIL='1').returncode, 0)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)

    def test_without_github_auth_downloads_the_public_release(self):
        self.args(AUTH_FAIL='1')
        self.assertEqual(self.installed.read_bytes(), FAKE_BINARY)
        urls = [line for line in self.log.read_text().splitlines() if line.startswith('curl')]
        base = f'https://github.com/tonioriol/decode-plugins/releases/download/{RELEASE}/'
        self.assertEqual([url.split("'")[-2] for url in urls], [base + ASSET, base + 'SHA256SUMS'])

    def test_failed_download_is_actionable(self):
        result = self.run_server(DOWNLOAD_FAIL='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f'could not download {ASSET}', result.stderr)
        self.assertFalse(self.installed.exists())

    def test_checksum_failure_never_installs_the_binary(self):
        result = self.run_server(BAD_CHECKSUM='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertEqual(list(self.installed.parent.iterdir()), [])
        self.assertEqual(list((self.root / 'tmp').iterdir()), [])

    def test_a_server_killed_mid_download_leaves_nothing_in_the_data_folder(self):
        server = subprocess.Popen([str(ROOT / 'plugins/memory/scripts/server.sh')], cwd=self.project,
                                  env=dict(self.env, DOWNLOAD_HANG='1'), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        while not self.log.exists():
            time.sleep(0.05)
        os.killpg(server.pid, signal.SIGKILL)
        server.wait()
        self.assertEqual(list(self.installed.parent.iterdir()), [])

    def test_notes_index_is_shared_with_decode_and_every_harness(self):
        args = self.args('memory')
        self.assertEqual(self.values(args, '--storage'), [str(self.data / 'memory')])
        self.assertNotIn('--index', args)
        self.assertIn('--docs-only', args)
        self.assertEqual(self.values(args, '--folder'), ['memory', 'docs=0.5'])
        elsewhere = self.root / 'elsewhere'
        args = self.args('memory', DECODE_PLUGINS_DATA=str(elsewhere), CLAUDE_PLUGIN_DATA=str(self.root / 'other'))
        self.assertEqual(self.values(args, '--storage'), [str(elsewhere / 'memory')])

    def test_code_index_is_decodes_when_decode_is_installed(self):
        self.assertEqual(self.values(self.args(), '--storage'), [str(self.data / 'semantic-search')])
        decode = self.home / 'Library/Application Support/Code/User/globalStorage/ultra.decode'
        decode.mkdir(parents=True)
        args = self.args()
        self.assertEqual(self.values(args, '--storage'), [str(decode)])
        self.assertEqual(self.values(args, '--index'), [str(decode / 'semantic-index')])
        override = self.root / 'override'
        self.assertEqual(self.values(self.args(DECODE_PLUGINS_DATA=str(override)), '--storage'),
                         [str(override / 'semantic-search')])

    def test_linux_finds_decode_under_the_config_home(self):
        self.write_executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo arm64;; esac\n')
        config = self.root / 'config'
        decode = config / 'Code/User/globalStorage/ultra.decode'
        decode.mkdir(parents=True)
        args = self.args(XDG_CONFIG_HOME=str(config), AUTH_FAIL='1')
        self.assertEqual(self.values(args, '--index'), [str(decode / 'semantic-index')])
        self.assertIn('decode-semantic-aarch64-unknown-linux-gnu', self.log.read_text())

    def test_chats_index_stays_per_harness_and_reuses_the_notes_model(self):
        model = self.data / 'memory/semble-model-rev'
        model.mkdir(parents=True)
        (model / 'weights').write_text('w')
        result = self.run_server('memory', 'chats-server.sh')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        chats = Path(self.env['CLAUDE_PLUGIN_DATA']) / 'chats'
        self.assertEqual(self.values(args, '--storage'), [str(chats)])
        self.assertEqual(self.values(args, '--scan'), [str(chats / 'corpus')])
        self.assertEqual((chats / 'semble-model-rev/weights').read_text(), 'w')

    def test_scan_roots_are_single_arguments_without_shell_evaluation(self):
        roots = ['/projects/space here', '/projects/$(touch unwanted)']
        for plugin in ('semantic-search', 'memory'):
            args = self.args(plugin, DECODE_PLUGINS_SCAN_ROOT=roots[0], DECODE_PLUGINS_SCAN_ROOT_2=roots[1])
            self.assertEqual(self.values(args, '--scan'), roots)
        self.assertFalse((self.project / 'unwanted').exists())

    def test_explicit_project_directory_overrides_process_working_directory(self):
        project = self.root / 'explicit project'
        project.mkdir()
        self.assertEqual(self.values(self.args(CLAUDE_PROJECT_DIR=str(project)), '--project'), [str(project)])

    def test_codex_manifests_launch_from_installed_cache_and_preserve_project(self):
        codex_home = self.root / 'codex home with spaces'
        self.write_executable(self.root / 'shared data/bin' / RELEASE / ASSET, FAKE_BINARY.decode())
        for plugin in ('semantic-search', 'memory'):
            source = ROOT / 'plugins' / plugin
            manifest = json.loads((source / '.codex-plugin/plugin.json').read_text())
            installed = codex_home / 'plugins/cache/decode-plugins' / plugin / manifest['version']
            shutil.copytree(source, installed)
            server = manifest['mcpServers'][plugin]
            # Codex passes the server only the variables its manifest names.
            offered = dict(self.env, CODEX_HOME=str(codex_home), DECODE_PLUGINS_DATA=str(self.root / 'shared data'))
            transport_env = {name: offered[name] for name in server['env_vars'] if name in offered}
            result = subprocess.run([server['command'], *server.get('args', [])], cwd=self.project,
                                    env=transport_env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(result.stdout)
            self.assertEqual(self.values(args, '--project'), [str(self.project)])
            self.assertEqual(self.values(args, '--storage'), [str(self.root / 'shared data' / plugin)])
            self.assertEqual(self.values(args, '--tool'), ['search' if plugin == 'memory' else 'project_search'])

    def test_concurrent_downloads_install_a_complete_binary(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: self.run_server(), range(2)))
        for result in results:
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('--project', json.loads(result.stdout))
        self.assertEqual(self.installed.read_bytes(), FAKE_BINARY)


if __name__ == '__main__':
    unittest.main()
