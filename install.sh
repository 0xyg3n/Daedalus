#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$(uname -s)" == Darwin ]]; then
  exec bash "$root/scripts/install-macos.sh" "$@"
fi
exec bash "$root/scripts/install-native.sh" "$@"
