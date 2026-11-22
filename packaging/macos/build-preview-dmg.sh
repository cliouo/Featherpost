#!/bin/bash
set -euo pipefail

die() {
    printf 'build-preview-dmg: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: packaging/macos/build-preview-dmg.sh --app PATH/TO/$BRAND_APP_BUNDLE [--output PATH.dmg]

Creates a local ad-hoc-signed preview DMG for hands-on testing. It is not a
Developer ID release, is not notarized, never changes the source bundle, and
never overwrites an existing output. The default output is
dist/macos/$BRAND_PRODUCT_NAME-preview.dmg; names come from packaging/macos/brand.env.
EOF
}

validate_output_parent() {
    local leaf=$1
    local current=$1
    local current_uid
    current_uid=$(/usr/bin/id -u)
    while :; do
        local owner mode_text mode
        owner=$(/usr/bin/stat -f '%u' "$current") ||
            die "cannot inspect output ancestor: $current"
        mode_text=$(/usr/bin/stat -f '%Mp%Lp' "$current") ||
            die "cannot inspect output ancestor permissions: $current"
        mode=$((mode_text))
        if [ "$current" = "$leaf" ]; then
            [ "$owner" -eq "$current_uid" ] ||
                die "output parent must be owned by the current user: $current"
            [ $((mode & 0022)) -eq 0 ] ||
                die "output parent must not be group/world writable: $current"
        else
            { [ "$owner" -eq "$current_uid" ] || [ "$owner" -eq 0 ]; } ||
                die "output ancestor must be owned by the current user or root: $current"
            if [ $((mode & 0022)) -ne 0 ]; then
                [ "$owner" -eq 0 ] && [ $((mode & 01000)) -ne 0 ] ||
                    die "writable output ancestor must be root-owned and sticky: $current"
            fi
        fi
        [ "$current" != / ] || break
        current=$(dirname -- "$current")
    done
}

[ "$(uname -s)" = Darwin ] || die 'macOS is required'
for tool in /usr/bin/codesign /usr/bin/ditto /usr/bin/hdiutil /usr/bin/plutil; do
    [ -x "$tool" ] || die "required system tool is missing: $tool"
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"
app=
output="$repo_root/dist/macos/$BRAND_PRODUCT_NAME-preview.dmg"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --app)
            [ "$#" -ge 2 ] || die '--app requires a path'
            app=$2
            shift 2
            ;;
        --output)
            [ "$#" -ge 2 ] || die '--output requires a path'
            output=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "$app" ] || die '--app is required'
[ ! -L "$app" ] && [ -d "$app" ] || die "input must be a non-symlink $BRAND_APP_BUNDLE directory"
[ "$(basename -- "$app")" = "$BRAND_APP_BUNDLE" ] ||
    die "input final component must be $BRAND_APP_BUNDLE"
[ "${output##*.}" = dmg ] || die 'output must end in .dmg'

output_parent=$(dirname -- "$output")
umask 077
mkdir -p -- "$output_parent"
output_parent=$(CDPATH= cd -- "$output_parent" && pwd -P)
validate_output_parent "$output_parent"
output="$output_parent/$(basename -- "$output")"
[ ! -e "$output" ] && [ ! -L "$output" ] || die "output already exists: $output"

work=$(mktemp -d "$output_parent/.$BRAND_SLUG-preview.XXXXXX") ||
    die 'cannot create preview staging directory'
chmod 0700 "$work"
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

root="$work/root"
mkdir -m 0700 -- "$root"
/usr/bin/ditto --noqtn --noextattr --noacl "$app" "$root/$BRAND_APP_BUNDLE"
preview="$root/$BRAND_APP_BUNDLE"
agent="$preview/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu="$preview/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
ui="$preview/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID/$BRAND_UI_BUNDLE"
host_ui="$preview/Contents/MacOS/$BRAND_UI_BIN"
short_version=$(/usr/bin/plutil -extract CFBundleShortVersionString raw \
    "$preview/Contents/Info.plist") || die 'cannot read bundle short version'
build_version=$(/usr/bin/plutil -extract CFBundleVersion raw \
    "$preview/Contents/Info.plist") || die 'cannot read bundle build version'
short_version_env="${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION"
build_version_env="${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION"
env "$short_version_env=$short_version" "$build_version_env=$build_version" \
    "$script_dir/verify-bundle.sh" "$preview" >/dev/null

# Ad-hoc preview signing is inside-out. Formal release uses Developer ID + timestamp + notary.
# Signing rewrites the UI Mach-O, so reseal its recovery checksum before sealing the app.
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$ui" >/dev/null
. "$script_dir/studio-payload.sh"
studio_payload=$(studio_payload_path "$preview")
if [ -d "$studio_payload" ]; then
    /usr/bin/codesign --force --sign - --options runtime --timestamp=none "$studio_payload/$STUDIO_APP" >/dev/null
fi
"$script_dir/seal-recovery-payload.sh" "$preview"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$agent" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$menu" >/dev/null
# Signing the app only seals its CFBundleExecutable (the launcher). The single-host UI
# sibling must be signed explicitly with the hardened runtime first; a linker ad-hoc
# signature would otherwise survive `codesign --verify --deep` without runtime.
/usr/bin/codesign --force --sign - --identifier "$BRAND_UI_BUNDLE_ID" --options runtime \
    --timestamp=none "$host_ui" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$preview" >/dev/null
env "$short_version_env=$short_version" "$build_version_env=$build_version" \
    "$script_dir/verify-bundle.sh" "$preview" >/dev/null
/usr/bin/codesign --verify --deep --strict=all --verbose=4 "$preview" >/dev/null 2>&1 ||
    die 'ad-hoc preview signature verification failed'
host_ui_details=$(/usr/bin/codesign --display --verbose=4 "$host_ui" 2>&1) ||
    die "cannot inspect host $BRAND_UI_BIN signature"
printf '%s\n' "$host_ui_details" | LC_ALL=C grep -Fqx "Identifier=$BRAND_UI_BUNDLE_ID" ||
    die "host $BRAND_UI_BIN signing identifier is not $BRAND_UI_BUNDLE_ID"
printf '%s\n' "$host_ui_details" | LC_ALL=C grep -Eq \
    '^CodeDirectory .* flags=.*[(].*runtime.*[)]' ||
    die "host $BRAND_UI_BIN does not enable the hardened runtime"

ln -s /Applications "$root/Applications"
temporary="$work/$BRAND_PRODUCT_NAME-preview.dmg"
/usr/bin/hdiutil create -quiet -fs HFS+ -format UDZO -volname "$BRAND_PRODUCT_NAME Preview" \
    -srcfolder "$root" "$temporary" || die 'hdiutil failed to create preview image'
[ ! -e "$output" ] && [ ! -L "$output" ] || die 'output appeared during build'
mv -n -- "$temporary" "$output" || die 'cannot publish preview DMG'
[ -f "$output" ] || die 'preview DMG publication failed'
printf '%s\n' "$output"
