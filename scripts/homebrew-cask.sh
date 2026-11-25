#!/usr/bin/env bash
# Print a Homebrew cask for a built universal DMG, for a maintainer's tap.
#
#   ./scripts/homebrew-cask.sh dist/Daedalus-<version>-macos-universal.dmg > daedalus.rb
#
# The cask installs Daedalus.app and links its bundled CLI as `daedalus`. The
# DMG must be the exact file published on the GitHub release: the checksum is
# computed from it. `brew upgrade --cask daedalus` replaces the whole signed app.
set -euo pipefail

dmg="${1:-}"
if [[ -z "$dmg" || ! -f "$dmg" ]]; then
  echo "Usage: $0 PATH/Daedalus-<version>-macos-universal.dmg" >&2
  exit 2
fi
name="$(basename -- "$dmg")"
if [[ ! "$name" =~ ^Daedalus-([0-9A-Za-z.+-]+)-macos-universal\.dmg$ ]]; then
  echo "Expected a universal DMG named Daedalus-<version>-macos-universal.dmg" >&2
  exit 2
fi
version="${BASH_REMATCH[1]}"
sha="$(shasum -a 256 "$dmg" | awk '{print $1}')"

cat <<CASK
cask "daedalus" do
  version "$version"
  sha256 "$sha"

  url "https://github.com/0xyg3n/Daedalus/releases/download/v#{version}/Daedalus-#{version}-macos-universal.dmg"
  name "Daedalus"
  desc "Terminal control interface that coordinates existing agent tools"
  homepage "https://github.com/0xyg3n/Daedalus"

  depends_on macos: ">= :big_sur"

  app "Daedalus.app"
  binary "#{appdir}/Daedalus.app/Contents/MacOS/daedalus-cli", target: "daedalus"
end
CASK
