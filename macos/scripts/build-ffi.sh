#!/usr/bin/env bash
# Builds crates/mach-ffi for Apple silicon and writes
# macos/Generated/MachFFI.xcframework plus macos/Generated/mach_ffi.swift.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
target=aarch64-apple-darwin
generated="$root/macos/Generated"
staging="$root/target/mach-ffi-bindings"
lib="$root/target/$target/release/libmach_ffi.a"
# C deps (sqlite, ring) otherwise target the build host's macOS and the app
# link warns about every object.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

cd "$root"
cargo build --release -p mach-ffi --target "$target"

rm -rf "$staging" "$generated/MachFFI.xcframework" "$generated/mach_ffi.swift"
mkdir -p "$staging/headers" "$generated"
cargo run --quiet -p mach-ffi --features bindgen --bin uniffi-bindgen-swift -- \
  "$lib" "$staging" --swift-sources --headers
cargo run --quiet -p mach-ffi --features bindgen --bin uniffi-bindgen-swift -- \
  "$lib" "$staging/headers" --modulemap \
  --module-name mach_ffiFFI --modulemap-filename module.modulemap
mv "$staging"/*.h "$staging/headers/"

xcodebuild -create-xcframework \
  -library "$lib" -headers "$staging/headers" \
  -output "$generated/MachFFI.xcframework" >/dev/null
cp "$staging/mach_ffi.swift" "$generated/mach_ffi.swift"
echo "wrote $generated/MachFFI.xcframework and $generated/mach_ffi.swift"
