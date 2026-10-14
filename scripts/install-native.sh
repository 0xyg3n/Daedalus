#!/usr/bin/env bash
# Native release installer, the default path used by install.sh.
set -euo pipefail
prefix="${DAEDALUS_PREFIX:-$HOME/.local}"
source_dir='' ref="${DAEDALUS_REF:-main}" ref_requested=0 binary='' expected='' yes=0 plan=0 no_deps=0
[[ -z "${DAEDALUS_REF:-}" ]] || ref_requested=1
while (($#)); do
  case "$1" in
    --native) shift ;;
    --compatibility) echo 'The Python distribution has been retired; use the native installer.' >&2; exit 2 ;;
    --prefix) prefix="${2:?missing prefix}"; shift 2 ;;
    --source) source_dir="${2:?missing checkout}"; shift 2 ;;
    --ref) ref="${2:?missing ref}"; ref_requested=1; shift 2 ;;
    --binary) binary="${2:?missing binary}"; shift 2 ;;
    --sha256) expected="${2:?missing checksum}"; shift 2 ;;
    --yes) yes=1; shift ;;
    --plan) plan=1; shift ;;
    --no-deps) no_deps=1; shift ;;
    -h|--help) echo 'Usage: install.sh [--source CHECKOUT --ref REF | --binary ABS --sha256 DIGEST] [--prefix ABS] [--plan] [--yes] [--no-deps]. --native is optional.'; exit 0 ;;
    *) echo "Unknown native installer option: $1 (select harnesses later with daedalus setup --cli)" >&2; exit 2 ;;
  esac
done
case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) architecture=x86_64; machine='62 0' ;;
  Linux:aarch64|Linux:arm64) architecture=aarch64; machine='183 0' ;;
  *) echo 'Native packages currently support Linux x86_64 and ARM64 only.' >&2; exit 1 ;;
esac
[[ "$prefix" == /* && "$prefix" != / && "$prefix" != *'/../'* && "$prefix" != */.. ]] || { echo 'Prefix must be an absolute path without traversal.' >&2; exit 2; }
[[ -z "$binary" || ( "$binary" == /* && -z "$source_dir" && "$expected" =~ ^[a-fA-F0-9]{64}$ ) ]] || { echo '--binary requires an absolute file and --sha256; it cannot be combined with --source.' >&2; exit 2; }
[[ -n "$binary" || -z "$expected" ]] || { echo '--sha256 requires --binary.' >&2; exit 2; }
printf 'Native plan: Linux %s; install one binary at %s/bin/daedalus\n' "$architecture" "$prefix"
if [[ -n "$binary" ]]; then printf 'Artifact: %s\nRequired SHA-256: %s\n' "$binary" "$expected"
else
  reviewed_ref="$ref"
  if [[ -n "$source_dir" && "$ref_requested" == 0 ]]; then reviewed_ref='committed HEAD'; fi
  printf 'Build committed source: %s (%s), cargo --release --locked; Rust 1.88+, C linker, git required.\n' "${source_dir:-https://github.com/0xyg3n/Daedalus.git}" "$reviewed_ref"
fi
printf 'No Python runtime, harness installation, private configuration writes, or service activation.\n'
[[ "$plan" == 0 ]] || exit 0
if [[ "$yes" != 1 ]]; then
  read -r -p 'Install this native release? Type yes: ' answer </dev/tty
  [[ "$answer" == yes ]] || exit 1
fi
umask 077
staging="$(mktemp -d)"
trap 'rm -rf -- "$staging"' EXIT
if [[ -z "$binary" ]]; then
  export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
  missing=()
  for cmd in git cc curl; do command -v "$cmd" >/dev/null || missing+=("$cmd"); done
  if ((${#missing[@]})); then
    [[ "$no_deps" == 0 ]] || { echo "Missing build prerequisites: ${missing[*]}" >&2; exit 1; }
    packages=()
    for cmd in "${missing[@]}"; do
      if [[ "$cmd" == cc ]]; then
        if command -v apt-get >/dev/null; then packages+=(build-essential)
        elif command -v dnf >/dev/null; then packages+=(gcc glibc-devel)
        else packages+=(base-devel); fi
      else packages+=("$cmd"); fi
    done
    elevate=(); [[ "$(id -u)" == 0 ]] || elevate=(sudo)
    printf 'Install system build dependencies: %s\n' "${packages[*]}"
    if command -v apt-get >/dev/null; then "${elevate[@]}" apt-get update; "${elevate[@]}" apt-get install -y "${packages[@]}"
    elif command -v dnf >/dev/null; then "${elevate[@]}" dnf install -y "${packages[@]}"
    elif command -v pacman >/dev/null; then "${elevate[@]}" pacman -S --needed --noconfirm "${packages[@]}"
    else echo 'Install the listed build dependencies using your OS package manager.' >&2; exit 1; fi
  fi
  if ! command -v cargo >/dev/null || ! command -v rustc >/dev/null; then
    [[ "$no_deps" == 0 ]] || { echo 'Missing Rust toolchain; install Rust 1.88+ or omit --no-deps.' >&2; exit 1; }
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$staging/rustup.sh"
    printf 'Rust bootstrap source: https://sh.rustup.rs\nDownloaded SHA-256: '
    sha256sum "$staging/rustup.sh"
    if [[ "$yes" != 1 ]]; then read -r -p 'Review the downloaded file above; install minimal Rust? Type yes: ' answer </dev/tty; [[ "$answer" == yes ]] || exit 1; fi
    sh "$staging/rustup.sh" -y --profile minimal --no-modify-path
  fi
  version="$(rustc --version)"; version="${version#rustc }"; version="${version%% *}"
  IFS=. read -r major minor _ <<< "$version"
  [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] && ((major > 1 || (major == 1 && minor >= 88))) || { echo 'Rust 1.88+ required; update the existing toolchain explicitly.' >&2; exit 1; }
  git clone --quiet --no-hardlinks -- "${source_dir:-https://github.com/0xyg3n/Daedalus.git}" "$staging/repo"
  if [[ -z "$source_dir" || "$ref_requested" == 1 ]]; then git -C "$staging/repo" checkout --quiet "$ref"; fi
  cargo build --release --locked --manifest-path "$staging/repo/src/tui/Cargo.toml" --target-dir "$staging/target"
  binary="$staging/target/release/daedalus-tui"
  expected="$(sha256sum "$binary")"; expected="${expected%% *}"
fi
[[ -f "$binary" && ! -L "$binary" ]] || { echo 'Artifact must be a regular, non-symlink file.' >&2; exit 1; }
# Verify the copied bytes, not a mutable external path subsequently executed.
cp -- "$binary" "$staging/daedalus"
actual="$(sha256sum "$staging/daedalus")"; actual="${actual%% *}"
[[ "${expected,,}" == "$actual" ]] || { echo 'Artifact checksum mismatch.' >&2; exit 1; }
magic="$(od -An -tx1 -N6 "$staging/daedalus" | xargs)"
actual_machine="$(od -An -tu1 -j18 -N2 "$staging/daedalus" | xargs)"
[[ "$magic" == '7f 45 4c 46 02 01' && "$actual_machine" == "$machine" ]] || { echo 'Artifact is not a matching Linux 64-bit little-endian ELF binary.' >&2; exit 1; }
# Reject symlinked installation parents before creating or replacing anything.
for path in "$prefix" "$prefix/bin" "$prefix/share" "$prefix/share/daedalus" "$prefix/share/daedalus/native"; do
  check="$path"
  while [[ "$check" != / ]]; do [[ ! -L "$check" ]] || { echo 'Installation paths cannot contain symlinks.' >&2; exit 1; }; check="${check%/*}"; [[ -n "$check" ]] || check=/; done
done
command_path="$prefix/bin/daedalus"
if [[ -e "$command_path" || -L "$command_path" ]]; then
  existing="$(readlink "$command_path" 2>/dev/null || true)"
  case "$existing" in "$prefix/share/daedalus/native/"*/daedalus) ;; *) echo 'Refusing to replace an unrelated or legacy command; choose another prefix.' >&2; exit 1 ;; esac
fi
release="$prefix/share/daedalus/native/$actual"
[[ ! -L "$release" ]] || { echo 'Release directory cannot be a symlink.' >&2; exit 1; }
mkdir -p -- "$release" "$prefix/bin"
if [[ -e "$release/daedalus" ]]; then
  installed="$(sha256sum "$release/daedalus")"; [[ "${installed%% *}" == "$actual" && ! -L "$release/daedalus" ]] || { echo 'Existing release content mismatch.' >&2; exit 1; }
else
  chmod 755 "$staging/daedalus"
  artifact="$(mktemp "$release/.daedalus-new-XXXXXXXX")"
  install -m 755 -- "$staging/daedalus" "$artifact"
  mv -T -- "$artifact" "$release/daedalus"
fi
link="$prefix/bin/.daedalus-install-$$"
[[ ! -e "$link" && ! -L "$link" ]] || exit 1
ln -s -- "$release/daedalus" "$link"
mv -Tf -- "$link" "$command_path"
printf 'Installed native binary: %s\nSHA-256: %s\nNext: %q setup\n' "$command_path" "$actual" "$command_path"
