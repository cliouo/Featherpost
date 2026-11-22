#!/bin/bash
set -euo pipefail

die() {
    printf 'build-universal: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: packaging/macos/build-universal.sh [options]

Options:
  --output PATH          Destination; its final component must be $BRAND_APP_BUNDLE.
  --short-version X.Y.Z  CFBundleShortVersionString (default: crates/maild version).
  --build-version X.Y.Z  CFBundleVersion (default: 1).
  -h, --help             Show this help.

Builds locked release binaries for aarch64-apple-darwin and
x86_64-apple-darwin with a macOS 13.0 deployment target, combines both
executables with lipo, then delegates bundle construction and validation to
build-bundle.sh. Binary and bundle names come from packaging/macos/brand.env.
The bundled Tauri UI is compiled with \`--features embedded\` after
\`pnpm --dir packages/maild-ui build\` produces its web assets. When the checkout
is a git work tree, the short commit is compiled in as MAILD_BUILD_REV and
${BRAND_ENV_PREFIX}_BUILD_REV (getAgentInfo.build_rev, ADR-0016); other checkouts
omit it. It never signs, notarizes, registers, or launches the bundle.
EOF
}

manifest_version() {
    local manifest=$1
    local version
    version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$manifest" | head -n 1)
    [ -n "$version" ] || die "cannot read package version from $manifest"
    printf '%s' "$version"
}

normalize_architectures() {
    lipo -archs "$1" 2>/dev/null | tr ' ' '\n' | LC_ALL=C sort | paste -sd ' ' -
}

[ "$(uname -s)" = Darwin ] || die 'macOS is required'
for tool in cargo lipo rustc; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"
output="$repo_root/dist/macos/$BRAND_APP_BUNDLE"
short_version=$(manifest_version "$repo_root/crates/maild/Cargo.toml")
build_version=1

while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            [ "$#" -ge 2 ] || die '--output requires a path'
            output=$2
            shift 2
            ;;
        --short-version)
            [ "$#" -ge 2 ] || die '--short-version requires a value'
            short_version=$2
            shift 2
            ;;
        --build-version)
            [ "$#" -ge 2 ] || die '--build-version requires a value'
            build_version=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[ "$(basename -- "$output")" = "$BRAND_APP_BUNDLE" ] ||
    die "the output final component must be $BRAND_APP_BUNDLE"

target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
case "$target_dir" in
    /*) ;;
    *) target_dir="$repo_root/$target_dir" ;;
esac
export CARGO_TARGET_DIR=$target_dir

work=$(mktemp -d "${TMPDIR:-/tmp}/$BRAND_SLUG-universal.XXXXXX") ||
    die 'cannot create universal build staging directory'
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

export MACOSX_DEPLOYMENT_TARGET=13.0
# ADR-0016：git short rev 编译进 getAgentInfo 的 build_rev；非 git 检出省略。
build_rev=$(/usr/bin/git -C "$repo_root" rev-parse --short=12 HEAD 2>/dev/null || true)
if [ -n "$build_rev" ]; then
    # The crate reads MAILD_BUILD_REV today; the branded name is exported for forward compatibility.
    export MAILD_BUILD_REV=$build_rev
    export "${BRAND_ENV_PREFIX}_BUILD_REV=$build_rev"
fi
command -v pnpm >/dev/null 2>&1 || die 'pnpm is required to build the bundled UI assets'
pnpm --dir "$repo_root/packages/maild-ui" install --frozen-lockfile
pnpm --dir "$repo_root/packages/maild-ui" build
# Crate names (`maild*`) are internal and stable; the [[bin]] names come from brand.toml.
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --manifest-path "$repo_root/Cargo.toml" \
        --release --locked --target "$target" -p maild --bin "$BRAND_AGENT_BIN"
    cargo build --manifest-path "$repo_root/Cargo.toml" \
        --release --locked --target "$target" -p maild-launcher --bin "$BRAND_LAUNCHER_BIN"
    cargo build --manifest-path "$repo_root/Cargo.toml" \
        --release --locked --target "$target" -p maild-menu --bin "$BRAND_MENU_BIN"
    cargo build --manifest-path "$repo_root/Cargo.toml" \
        --release --locked --target "$target" -p maild-ui --bin "$BRAND_UI_BIN" --features embedded
done

expected_architectures='arm64 x86_64'
for binary in "$BRAND_LAUNCHER_BIN" "$BRAND_AGENT_BIN" "$BRAND_MENU_BIN" "$BRAND_UI_BIN"; do
    lipo -create \
        "$target_dir/aarch64-apple-darwin/release/$binary" \
        "$target_dir/x86_64-apple-darwin/release/$binary" \
        -output "$work/$binary"
    chmod 0755 "$work/$binary"
    [ "$(normalize_architectures "$work/$binary")" = "$expected_architectures" ] ||
        die "$binary universal binary does not contain exactly arm64 and x86_64"
done

# Save the legacy universal binary before the Studio feature overwrites the per-target UI.
bash "$script_dir/build-studio-binary.sh" "$work/featherpost-studio"

env "${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=$short_version" \
    "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=$build_version" \
    "$script_dir/build-bundle.sh" \
        --launcher-bin "$work/$BRAND_LAUNCHER_BIN" \
        --agent-bin "$work/$BRAND_AGENT_BIN" \
        --menu-bin "$work/$BRAND_MENU_BIN" \
        --ui-bin "$work/$BRAND_UI_BIN" \
        --studio-bin "$work/featherpost-studio" \
        --output "$output"
