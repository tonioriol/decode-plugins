import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


HOOK = Path(__file__).resolve().parents[1] / "clients/cursor/session-start.sh"


@unittest.skipUnless(shutil.which("jq") and shutil.which("rg"), "hooks require jq and ripgrep")
class CursorSessionStartTest(unittest.TestCase):
    def test_global_hook_uses_reported_project_and_returns_cursor_context(self):
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "project with spaces"
            note = project / "memory/20261121000000-falcon/context.md"
            note.parent.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(project)], check=True)
            # A commit is required for the existing hook's recency ordering.
            note.write_text("status: active\n")
            subprocess.run(["git", "-C", str(project), "add", "--", str(note)], check=True)
            subprocess.run([
                "git", "-C", str(project), "-c", "user.name=Fixture", "-c",
                "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
                "-c", "core.hooksPath=/dev/null", "commit", "-qm", "Add fixture",
            ], check=True)
            payload = {"workspace_roots": [str(project)], "session_id": "test-session"}

            def run(**override):
                return subprocess.run(
                    ["bash", str(HOOK)], input=json.dumps({**payload, **override}),
                    cwd=tmp, check=True, capture_output=True, text=True,
                ).stdout

            context = json.loads(run())["additional_context"]
            self.assertIn("Recent open work:\n- falcon", context)
            self.assertEqual(run(workspace_roots=[str(project), tmp]), "")
            self.assertEqual(run(workspace_roots=[tmp]), "")
            self.assertEqual(run(parent_conversation_id="parent"), "")
