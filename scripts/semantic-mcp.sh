#!/usr/bin/env bash
# Canonical bootstrap; scripts/sync-launchers.py copies this into each plugin.
set -euo pipefail
release=v0.1.4
repo=tonioriol/decode-plugins
data=${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/decode-plugins}
shared="$HOME/.local/share/decode-plugins/bin/semantic-mcp"

if [[ ${DECODE_PLUGINS_PREFER_DECODE:-0} == 1 && ${DECODE_PLUGINS_STANDALONE:-0} != 1 ]]; then
  binary=$(python3 - <<'PY'
import os, pathlib
candidates = pathlib.Path.home().glob('.vscode/extensions/ultra.decode-*/dist/bin/decode')
executables = [p for p in candidates if os.access(p, os.X_OK)]
print(max(executables, key=lambda p: p.stat().st_mtime_ns) if executables else '')
PY
  )
  if [[ -n $binary ]]; then
    exec "$binary" semantic-mcp --storage "$data/index" "$@"
  fi
fi

if [[ ${DECODE_PLUGINS_PREFER_DECODE:-0} == 1 && -x $shared ]]; then
  exec "$shared" --storage "$data/index" "$@"
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo 'semantic-mcp: unsupported platform; see the repository build documentation.' >&2; exit 1 ;;
esac
asset=semantic-mcp-$target
binary="$data/releases/$release/$asset"
if [[ ! -x $binary ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo 'semantic-mcp: install GitHub CLI (gh), then run gh auth login with access to tonioriol/decode-plugins.' >&2
    exit 1
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo 'semantic-mcp: GitHub authentication is required. Run gh auth login with an account that can read tonioriol/decode-plugins.' >&2
    exit 1
  fi
  mkdir -p "$data/releases/$release"
  stage=$(mktemp -d "$data/releases/$release/download.XXXXXX")
  trap 'rm -rf "$stage"' EXIT
  if ! gh release download "$release" --repo "$repo" --pattern "$asset" --pattern SHA256SUMS --dir "$stage" >&2; then
    echo "semantic-mcp: could not download $asset from release $release. Check repository access and available platform assets; see build documentation." >&2
    exit 1
  fi
  python3 - "$stage/$asset" "$stage/SHA256SUMS" <<'PY'
import hashlib, pathlib, sys
binary, sums = map(pathlib.Path, sys.argv[1:])
expected = {}
for line in sums.read_text().splitlines():
    digest, name = line.split(maxsplit=1)
    expected[name.lstrip('*')] = digest
actual = hashlib.sha256(binary.read_bytes()).hexdigest()
if expected.get(binary.name) != actual:
    raise SystemExit('semantic-mcp: release checksum mismatch; refusing to execute download')
PY
  chmod 755 "$stage/$asset"
  # Immutable, checksummed release: concurrent installs atomically publish identical bytes.
  mv -f "$stage/$asset" "$binary"
  rm -rf "$stage"
  trap - EXIT
fi
mkdir -p "$(dirname "$shared")"
published=$(mktemp "$(dirname "$shared")/.semantic-mcp.XXXXXX")
cp "$binary" "$published"
chmod 755 "$published"
mv -f "$published" "$shared"
exec "$binary" --storage "$data/index" "$@"
