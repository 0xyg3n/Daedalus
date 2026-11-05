#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/src/tui/Cargo.toml"
# macOS /tmp and /var are symlinks; state-path checks reject symlinked roots.
if [[ "$(uname -s)" == Darwin ]]; then
  TMPDIR="$(cd /tmp && pwd -P)"
  export TMPDIR
fi
cargo fmt --manifest-path "$manifest" --check
cargo test --manifest-path "$manifest" --locked --all-targets
cargo clippy --manifest-path "$manifest" --locked --all-targets -- -D warnings
cargo build --manifest-path "$manifest" --locked --release
