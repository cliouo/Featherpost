#!/bin/bash
# Build the separately installable Studio frontend; never replace the recovery UI.
set -euo pipefail
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
output="$repo_root/dist/studio"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
case "$target_dir" in /*) ;; *) target_dir="$repo_root/$target_dir" ;; esac
export MACOSX_DEPLOYMENT_TARGET=13.0
cd "$repo_root"
pnpm --dir packages/featherpost-studio build
export TAURI_CONFIG
TAURI_CONFIG=$(cat crates/maild-ui/tauri.studio.conf.json)
for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --locked --release --target "$arch" -p maild-ui --features embedded,studio
    # The CLI needs no Tauri config and ships with the installer for existing DMGs.
    env -u TAURI_CONFIG cargo build --locked --release --target "$arch" -p maild --bin featherpostctl
done
package="$output/Featherpost Studio"
app="$package/frontend/Featherpost Studio.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create "$target_dir/aarch64-apple-darwin/release/featherpost-ui" "$target_dir/x86_64-apple-darwin/release/featherpost-ui" -output "$app/Contents/MacOS/featherpost-studio"
lipo -create "$target_dir/aarch64-apple-darwin/release/featherpostctl" "$target_dir/x86_64-apple-darwin/release/featherpostctl" -output "$package/featherpostctl"
cp packaging/macos/studio/Info.plist "$app/Contents/Info.plist"
cp packaging/macos/studio/frontend.toml "$package/frontend/frontend.toml"
cp "packaging/macos/studio/安装 Studio.command" "$package/安装 Studio.command"
cp "packaging/macos/studio/切回原版.command" "$package/切回原版.command"
cp packaging/macos/studio/README.txt "$package/README.txt"
chmod +x "$package/安装 Studio.command" "$package/切回原版.command" "$package/featherpostctl" "$app/Contents/MacOS/featherpost-studio"
codesign --force --deep --sign - "$app"
codesign --force --sign - "$package/featherpostctl"
codesign --verify --deep --strict "$app"
plutil -lint "$app/Contents/Info.plist"
ditto -c -k --sequesterRsrc --keepParent "$package" "$output/featherpost-studio-macos-universal.zip"
shasum -a 256 "$output/featherpost-studio-macos-universal.zip" > "$output/featherpost-studio-macos-universal.zip.sha256"
printf 'Studio package: %s\n' "$output/featherpost-studio-macos-universal.zip"
