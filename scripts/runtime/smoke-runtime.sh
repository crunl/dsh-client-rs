#!/usr/bin/env bash
# Runtime smoke against an already-running host.
#
#   RUNTIME_SMOKE_ISOLATED=1 scripts/runtime/smoke-runtime.sh /path/to/ready-url
#
# The URL contains a launch token: keep the file mode at 0600. The host must
# use an isolated $DSH_HOME because the model example writes a model selection.
set -euo pipefail

if [ "${RUNTIME_SMOKE_ISOLATED:-}" != "1" ]; then
  echo "set RUNTIME_SMOKE_ISOLATED=1 to confirm the host uses an isolated DSH_HOME" >&2
  exit 2
fi
if [ "$#" -ne 1 ]; then
  echo "usage: $0 <ready-url-file>" >&2
  exit 2
fi

url_file=$1
if [ ! -f "$url_file" ]; then
  echo "missing ready URL file: $url_file" >&2
  exit 2
fi
url=$(<"$url_file")
if [ -z "$url" ]; then
  echo "empty ready URL file: $url_file" >&2
  exit 2
fi

workspace=$(mktemp -d "${TMPDIR:-/tmp}/cowork-runtime-smoke.XXXXXX")
trap 'rm -rf "$workspace"' EXIT

echo '==> probe: session/list + permission catalog'
cargo run -q -p dsh-host --example probe -- "$url"

echo '==> model: model catalog + selection'
cargo run -q -p dsh-host --example model -- "$url"

echo '==> workspaces: follow + create/delete'
cargo run -q -p dsh-host --example workspaces -- "$url" "$workspace"

echo '==> follow: live prompt + durable transcript'
cargo run -q -p dsh-host --example follow -- "$url" '只回复两个字：收到'

echo 'RUNTIME_SMOKE_OK'
