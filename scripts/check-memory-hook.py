#!/usr/bin/env python3
"""Check a deployed memory hook against this repository's canonical copy."""
import argparse
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('deployed', type=Path)
args = parser.parse_args()
canonical = Path(__file__).resolve().parent.parent / 'plugins/memory/hooks/flush-hooks.sh'
if args.deployed.read_bytes() != canonical.read_bytes():
    raise SystemExit('Deployed memory hook differs from the canonical package hook.')
print('Deployed memory hook matches the canonical package hook.')
