#!/usr/bin/env python3
"""Package the canonical launcher in both independently cached plugin roots."""
import pathlib
import sys

root = pathlib.Path(__file__).resolve().parent.parent
source = (root / "scripts/rsemble.sh").read_bytes()
for plugin in ("memory", "semantic-search"):
    target = root / "plugins" / plugin / "scripts/rsemble.sh"
    if "--check" in sys.argv:
        if target.read_bytes() != source:
            raise SystemExit(f"Stale packaged launcher: {target.relative_to(root)}")
    else:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source)
        target.chmod(0o755)
