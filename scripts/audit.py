#!/usr/bin/env python3
"""Reject machine-specific paths and credential-shaped text before publishing."""
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parent.parent
paths = subprocess.check_output(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=root).decode().split('\0')
patterns = {
    'personal absolute home path': re.compile(r'/(?:Users|home)/[A-Za-z0-9_.-]+'),
    'credential-shaped value': re.compile(r'(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-ant-[A-Za-z0-9_-]{16,}|AKIA[A-Z0-9]{16})'),
    'private key material': re.compile(r'-----BEGIN (?:RSA |OPENSSH |EC )?PRIVATE KEY-----'),
}
failures = []
count = 0
for name in filter(None, paths):
    path = root / name
    if path.is_symlink():
        failures.append(f'{name}: symlinks are not packaged reliably')
        continue
    if not path.is_file():
        continue
    count += 1
    text = path.read_text()
    for label, pattern in patterns.items():
        for match in pattern.finditer(text):
            line = text.count('\n', 0, match.start()) + 1
            failures.append(f'{name}:{line}: {label}')
if failures:
    raise SystemExit('\n'.join(failures))
print(f'Audit passed: {count} text files, no personal home paths, credential-shaped values, private keys, or symlinks.')
