#!/bin/bash
# Build only the unsigned universal Studio executable, suitable for the product app bundle.
set -euo pipefail
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
[ "$#" -eq 1 ] || { printf 'usage: build-studio-binary.sh OUTPUT\n' >&2; exit 1; }
output=$1
. "$script_dir/studio-payload.sh"
version=$(node -p 'require(process.argv[1]).version' "$repo_root/crates/maild-ui/tauri.studio.conf.json")
[ "$version" = "$(studio_payload_version)" ] || { printf 'Studio config/manifest version mismatch\n' >&2; exit 1; }
[ ! -e "$output" ] && [ ! -L "$output" ] || { printf 'output exists\n' >&2; exit 1; }
target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
case "$target_dir" in /*) ;; *) target_dir="$repo_root/$target_dir" ;; esac
export MACOSX_DEPLOYMENT_TARGET=13.0
export TAURI_CONFIG
TAURI_CONFIG=$(cat "$repo_root/crates/maild-ui/tauri.studio.conf.json")
pnpm --dir "$repo_root/packages/featherpost-studio" build
for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --manifest-path "$repo_root/Cargo.toml" --locked --release --target "$arch" \
        -p maild-ui --features embedded,studio
done
lipo -create "$target_dir/aarch64-apple-darwin/release/featherpost-ui" \
    "$target_dir/x86_64-apple-darwin/release/featherpost-ui" -output "$output"
chmod 0755 "$output"
