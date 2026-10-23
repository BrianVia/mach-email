#!/usr/bin/env bash
# Builds macos/build/Mach.app. Pass --install to copy it to ~/Applications/Mach-dev/.
set -euo pipefail
cd "$(dirname "$0")/.."

# Rebuild the Rust bridge when it is missing or older than any engine source.
stamp=Generated/mach_ffi.swift
if [[ ! -f "$stamp" ]] || [[ -n "$(find ../crates ../config ../Cargo.lock \( -name '*.rs' -o -name '*.toml' -o -name Cargo.lock \) -newer "$stamp" -print -quit)" ]]; then
  bash scripts/build-ffi.sh
fi

swift build -c release -Xswiftc -warnings-as-errors
bin="$(swift build -c release --show-bin-path)/Mach"

icon=build/AppIcon.icns
if [[ ! -f "$icon" || scripts/make-icon.swift -nt "$icon" ]]; then
  rm -rf build/AppIcon.iconset
  swift scripts/make-icon.swift build/AppIcon.iconset
  iconutil -c icns -o "$icon" build/AppIcon.iconset
fi

version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' ../Cargo.toml)"
[[ -n "$version" ]] || { echo "no [workspace.package] version in Cargo.toml" >&2; exit 1; }

app=build/Mach.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Mach"
cp "$icon" "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.via.mach</string>
  <key>CFBundleName</key><string>Mach</string>
  <key>CFBundleDisplayName</key><string>Mach</string>
  <key>CFBundleExecutable</key><string>Mach</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$(git rev-list --count HEAD 2>/dev/null || echo 1)</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>NSHumanReadableCopyright</key><string>© $(date +%Y) Brian Via</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$app"
echo "built $PWD/$app"

if [[ "${1:-}" == "--install" ]]; then
  dest="$HOME/Applications/Mach-dev"
  mkdir -p "$dest"
  rm -rf "$dest/Mach.app"
  ditto "$app" "$dest/Mach.app"
  echo "installed $dest/Mach.app"
fi
