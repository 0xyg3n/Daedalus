#!/usr/bin/env bash
# Install the Daedalus CLI on macOS.
#
# From a checkout:
#   ./scripts/install-macos.sh
#   ./scripts/install-macos.sh --prefix "$HOME/.local" --yes
#
# From a verified release binary:
#   ./scripts/install-macos.sh --binary /abs/daedalus --sha256 HEX --yes
#
# The command is installed as PREFIX/bin/daedalus. Add PREFIX/bin to PATH if
# the installer reports that it is missing. This installs only the CLI; the
# Daedalus.app bundle is a separate artifact.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${DAEDALUS_PREFIX:-$HOME/.local}"
source_dir='' binary='' expected='' yes=0 plan=0
while (($#)); do
  case "$1" in
    --prefix) prefix="${2:?missing prefix}"; shift 2 ;;
    --source) source_dir="${2:?missing checkout}"; shift 2 ;;
    --binary) binary="${2:?missing binary}"; shift 2 ;;
    --sha256) expected="${2:?missing checksum}"; shift 2 ;;
    --yes) yes=1; shift ;;
    --plan) plan=1; shift ;;
    -h|--help)
      echo 'Usage: install-macos.sh [--source CHECKOUT | --binary ABS --sha256 HEX] [--prefix ABS] [--plan] [--yes]'
      exit 0
      ;;
    *) echo "Unknown installer option: $1" >&2; exit 2 ;;
  esac
done
case "$(uname -s):$(uname -m)" in
  Darwin:arm64) architecture=arm64; target=aarch64-apple-darwin ;;
  Darwin:x86_64) architecture=x86_64; target=x86_64-apple-darwin ;;
  Darwin:*) echo "Unsupported macOS architecture: $(uname -m)" >&2; exit 1 ;;
  *) echo 'This installer supports macOS. Use install.sh on Linux or install.ps1 on Windows.' >&2; exit 1 ;;
esac
[[ "$prefix" == /* && "$prefix" != / && "$prefix" != *'/../'* && "$prefix" != */.. ]] || { echo 'Prefix must be an absolute path without traversal.' >&2; exit 2; }
[[ -z "$binary" || ( "$binary" == /* && "$expected" =~ ^[a-fA-F0-9]{64}$ ) ]] || { echo '--binary requires an absolute file and --sha256.' >&2; exit 2; }
[[ -z "$source_dir" || -z "$binary" ]] || { echo '--source and --binary are mutually exclusive.' >&2; exit 2; }
[[ -n "$binary" || -z "$expected" ]] || { echo '--sha256 requires --binary.' >&2; exit 2; }
app=''
if [[ -z "$binary" ]]; then
  if [[ -z "$source_dir" ]]; then
    source_dir="$root"
  fi
  source_dir="$(cd -- "$source_dir" && pwd -P)"
  [[ -f "$source_dir/src/tui/Cargo.toml" ]] || { echo "Not a Daedalus checkout: $source_dir" >&2; exit 1; }
  app="$source_dir/src/tui/target/$target/release/daedalus-tui"
  printf 'Native plan: macOS %s (%s); build %s with cargo --release --locked, install at %s/bin/daedalus\n' "$architecture" "$target" "$source_dir" "$prefix"
else
  printf 'Native plan: macOS %s; install %s after SHA-256 verification at %s/bin/daedalus\n' "$architecture" "$binary" "$prefix"
fi
[[ "$plan" == 0 ]] || exit 0
if [[ "$yes" != 1 ]]; then
  read -r -p 'Install this native release? Type yes: ' answer </dev/tty
  [[ "$answer" == yes ]] || exit 1
fi
umask 077
staging="$(mktemp -d)"
trap 'rm -rf -- "$staging"' EXIT
if [[ -z "$binary" ]]; then
  for tool in cargo clang git; do
    command -v "$tool" >/dev/null || {
      echo "Missing $tool. Install Rust 1.88+ and Xcode Command Line Tools first (brew install rustup; xcode-select --install)." >&2
      exit 1
    }
  done
  cargo_cmd=(cargo)
  toolchain=''
  if command -v rustup >/dev/null; then
    toolchain="$(rustup show active-toolchain 2>/dev/null | awk '{print $1}')"
    if [[ -n "$toolchain" ]] && rustup run "$toolchain" cargo --version >/dev/null 2>&1; then
      cargo_cmd=(rustup run "$toolchain" cargo)
    export RUSTC="$(rustup which rustc --toolchain "$toolchain")"
      rustup target list --installed --toolchain "$toolchain" | grep -qx "$target" || rustup target add --toolchain "$toolchain" "$target"
    fi
  fi
  version="$("${cargo_cmd[@]}" --version | awk '{print $2}')"
  IFS=. read -r major minor _ <<< "$version"
  [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] && ((major > 1 || (major == 1 && minor >= 88))) || { echo 'Rust 1.88+ required.' >&2; exit 1; }
  "${cargo_cmd[@]}" build --release --locked --manifest-path "$source_dir/src/tui/Cargo.toml" --target "$target"
  binary="$app"
else
  [[ -f "$binary" && ! -L "$binary" ]] || { echo 'Artifact must be a regular, non-symlink file.' >&2; exit 1; }
fi
[[ -f "$binary" && ! -L "$binary" ]] || { echo 'Artifact must be a regular, non-symlink file.' >&2; exit 1; }
actual="$(shasum -a 256 "$binary" | awk '{print $1}')"
expected_lc="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
if [[ -n "$expected" && "$expected_lc" != "$actual" ]]; then
  echo 'Artifact checksum mismatch.' >&2
  exit 1
fi
kind="$(file -b "$binary")"
case "$kind" in
  *"Mach-O 64-bit executable"*|*"Mach-O universal binary"*) ;;
  *) echo 'Artifact is not a 64-bit Mach-O executable.' >&2; exit 1 ;;
esac
if command -v lipo >/dev/null; then
  archs="$(lipo -archs "$binary" 2>/dev/null || true)"
  [[ " $archs " == *" $architecture "* ]] || { echo "Artifact does not contain the $architecture architecture." >&2; exit 1; }
else
  [[ "$kind" == *"$architecture"* || "$kind" == *"universal"* ]] || { echo "Artifact does not contain the $architecture architecture." >&2; exit 1; }
fi
# Reject symlinked installation parents before creating or replacing anything.
for path in "$prefix" "$prefix/bin" "$prefix/share" "$prefix/share/daedalus" "$prefix/share/daedalus/native"; do
  check="$path"
  while [[ "$check" != / ]]; do [[ ! -L "$check" ]] || { echo 'Installation paths cannot contain symlinks.' >&2; exit 1; }; check="${check%/*}"; [[ -n "$check" ]] || check=/; done
done
command_path="$prefix/bin/daedalus"
if [[ -e "$command_path" || -L "$command_path" ]]; then
  existing="$(readlink "$command_path" 2>/dev/null || true)"
  case "$existing" in "$prefix/share/daedalus/native/"*/daedalus|"$prefix/bin/.daedalus-updates/"*/daedalus) ;; *) echo 'Refusing to replace an unrelated or legacy command; choose another prefix.' >&2; exit 1 ;; esac
fi
release="$prefix/share/daedalus/native/$actual"
[[ ! -L "$release" ]] || { echo 'Release directory cannot be a symlink.' >&2; exit 1; }
mkdir -p -- "$release" "$prefix/bin"
if [[ -e "$release/daedalus" ]]; then
  installed="$(shasum -a 256 "$release/daedalus" | awk '{print $1}')"
  [[ "$installed" == "$actual" && ! -L "$release/daedalus" ]] || { echo 'Existing release content mismatch.' >&2; exit 1; }
else
  cp "$binary" "$staging/daedalus"
  chmod 755 "$staging/daedalus"
  install -m 755 "$staging/daedalus" "$release/daedalus"
fi
link="$prefix/bin/.daedalus-install-$$"
[[ ! -e "$link" && ! -L "$link" ]] || exit 1
ln -s "$release/daedalus" "$link"
mv -f "$link" "$command_path"
printf 'Installed native binary: %s\nSHA-256: %s\n' "$command_path" "$actual"
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) printf 'Add the command directory to PATH:\n  export PATH="%s/bin:$PATH"\n' "$prefix" ;;
esac
printf 'Next: %q setup --cli\n' "$command_path"
