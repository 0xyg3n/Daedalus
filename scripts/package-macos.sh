#!/usr/bin/env bash
# Build a local Daedalus.app and .dmg on macOS.
#
# Local build (ad-hoc signature, no notarization):
#   ./scripts/package-macos.sh
#
# Distribution build (requires an Apple Developer Program membership):
#   DAEDALUS_MACOS_IDENTITY='Developer ID Application: Name (TEAMID)' \
#   DAEDALUS_MACOS_NOTARY_PROFILE='daedalus-notary' \
#   ./scripts/package-macos.sh
#
# The notary profile is created once with:
#   xcrun notarytool store-credentials daedalus-notary \
#     --apple-id you@example.com --team-id TEAMID \
#     --password 'app-specific-password'
#
# Optional:
#   DAEDALUS_MACOS_UNIVERSAL=1   build arm64 + x86_64 and lipo them
#   DAEDALUS_MACOS_BUNDLE_ID=... override the default bundle identifier
#   DAEDALUS_MACOS_MINIMUM=...   override LSMinimumSystemVersion (default 11.0)
#   DAEDALUS_MACOS_NOTARY_KEYCHAIN=... keychain holding the notary profile (CI)
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/src/tui/Cargo.toml"
version="$(tr -d '[:space:]' < "$root/VERSION")"
bundle_version="$(printf '%s' "$version" | sed -E 's/^([0-9]+(\.[0-9]+){0,2}).*/\1/')"
bundle_id="${DAEDALUS_MACOS_BUNDLE_ID:-io.github.0xyg3n.daedalus}"
identity="${DAEDALUS_MACOS_IDENTITY:--}"
notary_profile="${DAEDALUS_MACOS_NOTARY_PROFILE:-}"
notary_keychain="${DAEDALUS_MACOS_NOTARY_KEYCHAIN:-}"
minimum="${DAEDALUS_MACOS_MINIMUM:-11.0}"
universal="${DAEDALUS_MACOS_UNIVERSAL:-0}"
dist="$root/dist"
app="$dist/Daedalus.app"
stage="$(mktemp -d)"
trap 'rm -rf -- "$stage" "$dist/Daedalus.iconset"' EXIT

for tool in cargo clang codesign hdiutil iconutil lipo sips; do
  command -v "$tool" >/dev/null || { echo "Missing required tool: $tool" >&2; exit 1; }
done
cargo_cmd=(cargo)
toolchain=''
if command -v rustup >/dev/null; then
  toolchain="$(rustup show active-toolchain 2>/dev/null | awk '{print $1}')"
  if [[ -n "$toolchain" ]] && rustup run "$toolchain" cargo --version >/dev/null 2>&1; then
    cargo_cmd=(rustup run "$toolchain" cargo)
    export RUSTC="$(rustup which rustc --toolchain "$toolchain")"
  fi
fi

case "$(uname -m)" in
  arm64) host_label=arm64 ;;
  x86_64) host_label=x86_64 ;;
  *) echo "Unsupported build architecture: $(uname -m)" >&2; exit 1 ;;
esac
if [[ "$universal" == 1 ]]; then
  arch_label=universal
else
  arch_label="$host_label"
fi
dmg="$dist/Daedalus-$version-macos-$arch_label.dmg"

targets=()
if [[ "$universal" == 1 ]]; then
  command -v rustup >/dev/null || {
    echo "Universal builds need rustup targets; build host-only or install rustup." >&2
    exit 1
  }
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
  if [[ -n "$toolchain" ]]; then
    rustup target add --toolchain "$toolchain" "${targets[@]}"
  else
    rustup target add "${targets[@]}"
  fi
else
  case "$(uname -m)" in
    arm64) targets=(aarch64-apple-darwin) ;;
    x86_64) targets=(x86_64-apple-darwin) ;;
    *) echo "Unsupported build architecture: $(uname -m)" >&2; exit 1 ;;
  esac
fi

binaries=()
for target in "${targets[@]}"; do
  echo "Building $target"
  "${cargo_cmd[@]}" build --release --locked --manifest-path "$manifest" --target "$target"
  binaries+=("$root/src/tui/target/$target/release/daedalus-tui")
done

binary="$stage/daedalus"
if (( ${#binaries[@]} == 1 )); then
  cp "${binaries[0]}" "$binary"
else
  lipo -create -output "$binary" "${binaries[@]}"
fi
chmod 755 "$binary"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/daedalus-cli"
chmod 755 "$app/Contents/MacOS/daedalus-cli"
# The staging copy is not shipped alongside the signed app.
rm -f "$binary"
clang -O2 -Wall -Wextra -mmacosx-version-min="$minimum" \
  -o "$app/Contents/MacOS/Daedalus" "$root/scripts/macos-launcher.c"
chmod 755 "$app/Contents/MacOS/Daedalus"

cat > "$app/Contents/Resources/Daedalus.command" <<'COMMAND'
#!/bin/bash
set -euo pipefail
here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$here/../MacOS/daedalus-cli" "$@"
COMMAND
chmod 755 "$app/Contents/Resources/Daedalus.command"

mkdir -p "$dist/Daedalus.iconset"
for spec in "16 icon_16x16" "32 icon_16x16@2x" "32 icon_32x32" "64 icon_32x32@2x" \
  "128 icon_128x128" "256 icon_128x128@2x" "256 icon_256x256" "512 icon_256x256@2x" \
  "512 icon_512x512" "1024 icon_512x512@2x"; do
  size="${spec%% *}"
  name="${spec#* }"
  sips -z "$size" "$size" "$root/assets/daedalus.png" \
    --out "$dist/Daedalus.iconset/$name.png" >/dev/null
done
iconutil -c icns "$dist/Daedalus.iconset" -o "$app/Contents/Resources/Daedalus.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Daedalus</string>
  <key>CFBundleExecutable</key><string>Daedalus</string>
  <key>CFBundleIconFile</key><string>Daedalus.icns</string>
  <key>CFBundleIdentifier</key><string>$bundle_id</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Daedalus</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$bundle_version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>$minimum</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

sign() {
  if [[ "$identity" == "-" ]]; then
    codesign --force --sign - "$1"
  else
    codesign --force --options runtime --timestamp --sign "$identity" "$1"
  fi
}
sign "$app/Contents/MacOS/daedalus-cli"
sign "$app/Contents/MacOS/Daedalus"
sign "$app"

# In-app update assets: one thin, signed CLI per architecture, named as
# `daedalus update` selects them (`daedalus-<version>-<arch>-macos`).
for target in "${targets[@]}"; do
  case "$target" in
    aarch64-apple-darwin) asset_arch=aarch64 ;;
    x86_64-apple-darwin) asset_arch=x86_64 ;;
  esac
  asset="daedalus-$version-$asset_arch-macos"
  cp "$root/src/tui/target/$target/release/daedalus-tui" "$dist/$asset"
  chmod 755 "$dist/$asset"
  sign "$dist/$asset"
  (cd "$dist" && shasum -a 256 "$asset" > "$asset.sha256")
  printf 'Built update asset %s\n' "$dist/$asset"
done

ditto "$app" "$stage/Daedalus.app"
ln -s /Applications "$stage/Applications"
cat > "$stage/README.txt" <<README
Daedalus $version for macOS

Drag Daedalus.app to Applications, then open it. The app starts Terminal and
runs the bundled CLI, because Daedalus is a terminal interface.

If macOS blocks this build, use right-click > Open, or review it under
System Settings > Privacy & Security. Distribution builds are signed with a
Developer ID certificate and notarized by Apple; local builds are ad-hoc signed.

Daedalus on macOS is a preview. Linux is the fully supported desktop platform.
README

hdiutil create -volname "Daedalus $version" -srcfolder "$stage" \
  -ov -format UDZO "$dmg" >/dev/null

if [[ -n "$notary_profile" ]]; then
  [[ "$identity" != "-" ]] || {
    echo "Notarization requires DAEDALUS_MACOS_IDENTITY" >&2
    exit 1
  }
  codesign --force --timestamp --sign "$identity" "$dmg"
  notary_args=(--keychain-profile "$notary_profile")
  if [[ -n "$notary_keychain" ]]; then
    notary_args+=(--keychain "$notary_keychain")
  fi
  xcrun notarytool submit "$dmg" "${notary_args[@]}" --wait
  xcrun stapler staple "$dmg"
  xcrun stapler validate "$dmg"
fi

(
  cd "$dist"
  name="$(basename "$dmg")"
  shasum -a 256 "$name" | tee "$name.sha256"
)
printf 'Built %s\n' "$dmg"
