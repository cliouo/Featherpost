#!/bin/bash
set -euo pipefail

[ "$(uname -s)" = Darwin ] || {
    printf 'dry-run: macOS is required\n' >&2
    exit 1
}
command -v cc >/dev/null 2>&1 || {
    printf 'dry-run: C compiler is required for the fixture\n' >&2
    exit 1
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"
work=$(mktemp -d "${TMPDIR:-/tmp}/$BRAND_SLUG-bundle-dry-run.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

# C fixtures that carry the same compiled identity contracts as the real binaries
# (maild-brand agent_id_contract! / menu_id_contract! / ui_embedded_contract!).
fixture() {
    local output=$1
    local marker=$2
    printf '%s\n' \
        "__attribute__((used)) static const char identity[] = \"$marker\";" \
        'int main(void) { return identity[0] == 0; }' |
        cc -x c - -o "$output"
    chmod 0755 "$output"
}

mkdir -p -- "$work/bin" "$work/home" "$work/output"
fixture "$work/bin/$BRAND_LAUNCHER_BIN" "maild-agent-bundle-id:$BRAND_AGENT_BUNDLE_ID"
fixture "$work/bin/$BRAND_AGENT_BIN" "maild-agent-bundle-id:$BRAND_AGENT_BUNDLE_ID"
fixture "$work/bin/$BRAND_MENU_BIN" "maild-menu-bundle-id:$BRAND_MENU_BUNDLE_ID"
fixture "$work/bin/$BRAND_UI_BIN" "maild-ui-embedded:$BRAND_UI_BUNDLE_ID"

bundle="$work/output/$BRAND_APP_BUNDLE"
HOME="$work/home" \
    env "${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=0.4.0" \
    "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=1" \
    "$script_dir/build-bundle.sh" \
        --launcher-bin "$work/bin/$BRAND_LAUNCHER_BIN" \
        --agent-bin "$work/bin/$BRAND_AGENT_BIN" \
        --output "$bundle" >/dev/null

HOME="$work/home" \
    env "${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=0.4.0" \
    "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=1" \
    "$script_dir/verify-bundle.sh" "$bundle" >/dev/null

# Single-host layout: the launcher's in-place exec target and the native app icon.
cmp -s "$work/bin/$BRAND_UI_BIN" "$bundle/Contents/MacOS/$BRAND_UI_BIN" || {
    printf 'dry-run: Contents/MacOS/%s is not the embedded UI fixture\n' "$BRAND_UI_BIN" >&2
    exit 1
}
cmp -s "$script_dir/assets/AppIcon.icns" "$bundle/Contents/Resources/$BRAND_ICON_NAME.icns" || {
    printf 'dry-run: Contents/Resources/%s.icns does not match packaging/macos/assets/AppIcon.icns\n' \
        "$BRAND_ICON_NAME" >&2
    exit 1
}

[ -z "$(find "$work/home" -mindepth 1 -print -quit)" ] || {
    printf 'dry-run: bundle scripts unexpectedly wrote under HOME\n' >&2
    exit 1
}

printf 'dry-run: PASS (temporary unsigned %s verified and removed)\n' "$BRAND_APP_BUNDLE"
