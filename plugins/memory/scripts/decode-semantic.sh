#!/usr/bin/env bash
# Runs the pinned Decode Semantic release with the given arguments, downloading and checksumming it on first use.
# Canonical copy; scripts/sync-launchers.py packages it into each plugin.
#
# Data shared with Decode and between harnesses lives under DECODE_PLUGINS_DATA (default ~/.decode/plugin-data):
# the binary in bin/<release>/, the notes index in memory/. The servers pick their own --storage.
# The engine downloads and verifies its pinned embedding model; the launcher only installs the binary.
set -euo pipefail
release=v0.1.1
repo=tonioriol/decode-plugins
data=${DECODE_PLUGINS_DATA:-$HOME/.decode/plugin-data}

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "decode-semantic: no release build for $(uname -s) $(uname -m); build engine/ with cargo (see docs/building.md)." >&2; exit 1 ;;
esac
asset=decode-semantic-$target
binary="$data/bin/$release/$asset"

if [[ ! -x $binary ]]; then
  mkdir -p "$data/bin/$release"
  # Download in the system temp folder: a server killed mid-download (harnesses SIGKILL their MCP servers) leaves
  # nothing behind in the data folder, and the OS clears what is left there.
  stage=$(mktemp -d "${TMPDIR:-/tmp}/decode-semantic-download.XXXXXX")
  trap 'rm -rf "$stage"' EXIT
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    gh release download "$release" --repo "$repo" --pattern "$asset" --pattern SHA256SUMS --dir "$stage" >&2 || true
  else
    url="https://github.com/$repo/releases/download/$release"
    curl -fsSL -o "$stage/$asset" "$url/$asset" && curl -fsSL -o "$stage/SHA256SUMS" "$url/SHA256SUMS" || true
  fi
  if [[ ! -s $stage/$asset || ! -s $stage/SHA256SUMS ]]; then
    echo "decode-semantic: could not download $asset from release $release of $repo. Check the network (or gh auth login while the repository is private) and the release's platform assets." >&2
    exit 1
  fi
  python3 - "$stage/$asset" "$stage/SHA256SUMS" <<'PY'
import hashlib, pathlib, sys
binary, sums = map(pathlib.Path, sys.argv[1:])
expected = {}
for line in sums.read_text().splitlines():
    digest, name = line.split(maxsplit=1)
    expected[name.lstrip('*')] = digest
if expected.get(binary.name) != hashlib.sha256(binary.read_bytes()).hexdigest():
    raise SystemExit('decode-semantic: release checksum mismatch; refusing to run the download')
PY
  # A pinned, checksummed release: concurrent first runs publish identical bytes atomically, by a rename within
  # the data folder (the temp folder may be another filesystem).
  cp "$stage/$asset" "$binary.$$"
  chmod 755 "$binary.$$"
  mv -f "$binary.$$" "$binary"
  rm -rf "$stage"
  trap - EXIT
fi
exec "$binary" "$@"
