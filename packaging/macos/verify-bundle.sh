#!/bin/bash
set -euo pipefail

die() {
    printf 'verify-bundle: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: packaging/macos/verify-bundle.sh PATH/TO/$BRAND_APP_BUNDLE

Validates the bundle layout (single-host Contents/MacOS/$BRAND_UI_BIN, $BRAND_ICON_NAME.icns and
the $BRAND_UI_BUNDLE_ID recovery payload) and plist contract without changing it.
Expected names and identifiers come from packaging/macos/brand.env (generated from
brand.toml). Versions can be overridden with the same ${BRAND_ENV_PREFIX}_BUNDLE_* variables
accepted by build-bundle.sh (legacy MAILD_BUNDLE_* is read as a fallback).
EOF
}

require_directory() {
    local path=$1
    [ ! -L "$path" ] || die "directory must not be a symbolic link: $path"
    [ -d "$path" ] || die "missing directory: $path"
}

require_file() {
    local path=$1
    [ ! -L "$path" ] || die "file must not be a symbolic link: $path"
    [ -f "$path" ] || die "missing file: $path"
}

plist_string() {
    local plist=$1
    local key=$2
    plutil -extract "$key" raw -expect string "$plist"
}

expect_plist_string() {
    local plist=$1
    local key=$2
    local expected=$3
    local actual
    actual=$(plist_string "$plist" "$key") || die "missing string key $key in $plist"
    [ "$actual" = "$expected" ] ||
        die "$key in $plist is '$actual', expected '$expected'"
}

# ADR-0048 decision 8: the frontend bundles declare both UI languages so that system-drawn
# strings (WebKit context menu, open/save panels, alert buttons) follow the per-app
# AppleLanguages the shell mirrors; with no match they fall back to Simplified Chinese.
expect_frontend_localizations() {
    local plist=$1
    local actual
    expect_plist_string "$plist" CFBundleDevelopmentRegion zh-Hans
    actual=$(plutil -extract CFBundleLocalizations json -o - -- "$plist" 2>/dev/null) ||
        die "missing array key CFBundleLocalizations in $plist"
    [ "$actual" = '["en","zh-Hans"]' ] ||
        die "CFBundleLocalizations in $plist is $actual, expected [\"en\",\"zh-Hans\"]"
}

validate_bundle_id() {
    local label=$1
    local value=$2
    [ -n "$value" ] || die "$label must not be empty"
    [ "${#value}" -le 255 ] || die "$label is longer than 255 bytes"
    case "$value" in
        .*|*.|*..*|*/*|*:*|*[!A-Za-z0-9.-]*)
            die "$label is not a valid bundle identifier: $value"
            ;;
        *.*)
            ;;
        *)
            die "$label must contain at least one dot: $value"
            ;;
    esac
}

# Literal (glob- and `&`-free) replacement of every occurrence; portable to bash 3.2 and 5.2.
replace_all() {
    local text=$1
    local pattern=$2
    local replacement=$3
    local output=
    while :; do
        case "$text" in
            *"$pattern"*)
                output=$output${text%%"$pattern"*}$replacement
                text=${text#*"$pattern"}
                ;;
            *) break ;;
        esac
    done
    printf '%s' "$output$text"
}

xml_escape() {
    local value=$1
    value=$(replace_all "$value" '&' '&amp;')
    value=$(replace_all "$value" '<' '&lt;')
    replace_all "$value" '>' '&gt;'
}

# Render a template exactly as build-bundle.sh does (same placeholders, same escaping).
render_template() {
    local template=$1
    local format=$2
    local text key name value
    [ ! -L "$template" ] && [ -f "$template" ] || die "missing template: $template"
    text=$(cat -- "$template") || die "cannot read template: $template"
    for key in PRODUCT_NAME LAUNCHER_BIN AGENT_BIN MENU_BIN UI_BIN ICON_NAME \
        APP_BUNDLE_ID AGENT_BUNDLE_ID MENU_BUNDLE_ID UI_BUNDLE_ID \
        APP_BUNDLE AGENT_BUNDLE MENU_BUNDLE UI_BUNDLE \
        BUNDLE_SHORT_VERSION BUNDLE_BUILD_VERSION; do
        case "$key" in
            BUNDLE_SHORT_VERSION) value=$short_version ;;
            BUNDLE_BUILD_VERSION) value=$build_version ;;
            *) name="BRAND_$key"; value=${!name} ;;
        esac
        if [ "$format" = xml ]; then value=$(xml_escape "$value"); fi
        text=$(replace_all "$text" "__BRAND_${key}__" "$value")
    done
    case "$text" in
        *__BRAND_*|*__MAILD_*) die "template has an unknown placeholder: $template" ;;
    esac
    printf '%s\n' "$text"
}

# ${BRAND_ENV_PREFIX}_<NAME>, falling back to the legacy MAILD_<NAME>, then DEFAULT.
brand_env_value() {
    local name="${BRAND_ENV_PREFIX}_$1"
    local legacy="MAILD_$1"
    if [ -n "${!name:-}" ]; then
        printf '%s' "${!name}"
    elif [ -n "${!legacy:-}" ]; then
        printf '%s' "${!legacy}"
    else
        printf '%s' "$2"
    fi
}

validate_version() {
    local label=$1
    local value=$2
    printf '%s\n' "$value" | LC_ALL=C grep -Eq '^[0-9]+([.][0-9]+){0,2}$' ||
        die "$label must contain one to three dot-separated integers: $value"
}

read_identity() {
    local path=$1
    [ ! -L "$path" ] || die "identity manifest must not be a symbolic link: $path"
    [ -f "$path" ] || die "missing identity manifest: $path"
    local value
    value=$(cat -- "$path") || die "cannot read identity manifest: $path"
    case "$value" in
        *$'\n'*|*$'\r'*) die "identity manifest must contain exactly one line: $path" ;;
    esac
    printf '%s' "$value"
}

binary_architectures() {
    local label=$1
    local path=$2
    local architectures
    architectures=$(lipo -archs "$path" 2>/dev/null) ||
        die "$label is not a Mach-O executable: $path"
    printf '%s\n' "$architectures" | tr ' ' '\n' | LC_ALL=C sort | paste -sd ' ' -
}

require_compiled_identity() {
    local label=$1
    local binary=$2
    local prefix=$3
    local expected=$4
    local architectures architecture thin identities remove_thin
    architectures=$(binary_architectures "$label" "$binary")
    for architecture in $architectures; do
        thin=$binary
        remove_thin=false
        case "$architectures" in
            *' '*)
                thin=$(mktemp "${TMPDIR:-/tmp}/$BRAND_SLUG-identity.XXXXXX") ||
                    die "cannot create identity inspection file for $label"
                remove_thin=true
                if ! lipo -thin "$architecture" "$binary" -output "$thin"; then
                    rm -f -- "$thin"
                    die "cannot inspect $architecture slice in $label: $binary"
                fi
                ;;
        esac
        identities=$(strings -a "$thin" |
            awk -v prefix="$prefix" '
                index($0, prefix) == 1 {
                    identity = substr($0, length(prefix) + 1)
                    if (identity ~ /^[A-Za-z0-9][A-Za-z0-9.-]*$/ && identity ~ /[.]/) print identity
                }
            ' |
            LC_ALL=C sort -u) || {
                if [ "$remove_thin" = true ]; then rm -f -- "$thin"; fi
                die "cannot inspect compiled Agent identity in $label $architecture slice"
            }
        if [ "$remove_thin" = true ]; then rm -f -- "$thin"; fi
        [ "$identities" = "$expected" ] ||
            die "$label $architecture slice does not contain exactly the expected compiled identity contract: $expected"
    done
}

terminal_lines() {
    LC_ALL=C tr '\000-\011\013-\037\177' ' '
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
[ ! -L "$script_dir/brand.env" ] && [ -f "$script_dir/brand.env" ] ||
    die "missing $script_dir/brand.env; run bash scripts/sync-brand.sh"
# shellcheck source=brand.env
. "$script_dir/brand.env"
for brand_key in BRAND_PRODUCT_NAME BRAND_SLUG BRAND_ENV_PREFIX \
    BRAND_LAUNCHER_BIN BRAND_AGENT_BIN BRAND_MENU_BIN BRAND_UI_BIN \
    BRAND_APP_BUNDLE BRAND_AGENT_BUNDLE BRAND_MENU_BUNDLE BRAND_UI_BUNDLE BRAND_ICON_NAME \
    BRAND_APP_BUNDLE_ID BRAND_AGENT_BUNDLE_ID BRAND_MENU_BUNDLE_ID BRAND_UI_BUNDLE_ID; do
    [ -n "${!brand_key:-}" ] || die "brand.env does not define $brand_key; run bash scripts/sync-brand.sh"
done
agent_contract_prefix='maild-agent-bundle-id:'
menu_contract_prefix='maild-menu-bundle-id:'
ui_contract_prefix='maild-ui-embedded:'

if [ "$#" -eq 1 ] && { [ "$1" = -h ] || [ "$1" = --help ]; }; then
    usage
    exit 0
fi
[ "$#" -eq 1 ] || {
    usage >&2
    exit 2
}
[ "$(uname -s)" = Darwin ] || die 'macOS is required'
command -v plutil >/dev/null 2>&1 || die 'plutil is required'
command -v lipo >/dev/null 2>&1 || die 'lipo is required'
command -v strings >/dev/null 2>&1 || die 'strings is required'

bundle=$1
[ "$(basename -- "$bundle")" = "$BRAND_APP_BUNDLE" ] ||
    die "bundle final component must be $BRAND_APP_BUNDLE"
require_directory "$bundle"

agent_bundle="$bundle/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu_bundle="$bundle/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
main_plist="$bundle/Contents/Info.plist"
agent_plist="$agent_bundle/Contents/Info.plist"
menu_plist="$menu_bundle/Contents/Info.plist"
launcher_executable="$bundle/Contents/MacOS/$BRAND_LAUNCHER_BIN"
host_ui_executable="$bundle/Contents/MacOS/$BRAND_UI_BIN"
app_icon="$bundle/Contents/Resources/$BRAND_ICON_NAME.icns"
agent_executable="$agent_bundle/Contents/MacOS/$BRAND_AGENT_BIN"
menu_executable="$menu_bundle/Contents/MacOS/$BRAND_MENU_BIN"
ui_payload="$bundle/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID"
ui_bundle="$ui_payload/$BRAND_UI_BUNDLE"
ui_plist="$ui_bundle/Contents/Info.plist"
ui_executable="$ui_bundle/Contents/MacOS/$BRAND_UI_BIN"
ui_manifest="$ui_payload/frontend.toml"
ui_checksum="$ui_payload/recovery.sha256"

require_directory "$bundle/Contents"
require_directory "$bundle/Contents/MacOS"
require_directory "$bundle/Contents/Library"
require_directory "$bundle/Contents/Library/Helpers"
require_directory "$bundle/Contents/Library/LoginItems"
require_directory "$agent_bundle"
require_directory "$agent_bundle/Contents"
require_directory "$agent_bundle/Contents/MacOS"
require_directory "$menu_bundle"
require_directory "$menu_bundle/Contents"
require_directory "$menu_bundle/Contents/MacOS"
require_directory "$bundle/Contents/Resources"
require_directory "$bundle/Contents/Resources/frontends"
require_directory "$ui_payload"
require_directory "$ui_bundle"
require_directory "$ui_bundle/Contents"
require_directory "$ui_bundle/Contents/MacOS"
require_file "$ui_plist"
require_file "$ui_executable"
require_file "$ui_manifest"
require_file "$ui_checksum"
require_file "$main_plist"
require_file "$agent_plist"
require_file "$menu_plist"
require_file "$launcher_executable"
require_file "$host_ui_executable"
require_file "$app_icon"
require_file "$agent_executable"
require_file "$menu_executable"
[ -x "$launcher_executable" ] || die "launcher executable bit is missing: $launcher_executable"
[ -x "$host_ui_executable" ] || die "host UI executable bit is missing: $host_ui_executable"
[ -x "$agent_executable" ] || die "agent executable bit is missing: $agent_executable"
[ -x "$menu_executable" ] || die "menu executable bit is missing: $menu_executable"
[ -x "$ui_executable" ] || die "UI executable bit is missing: $ui_executable"
launcher_architectures=$(binary_architectures "$BRAND_LAUNCHER_BIN" "$launcher_executable")
for pair in "$BRAND_AGENT_BIN:$agent_executable" "$BRAND_MENU_BIN:$menu_executable" \
    "$BRAND_UI_BIN:$ui_executable" "$BRAND_UI_BIN (host):$host_ui_executable"; do
    label=${pair%%:*}
    architectures=$(binary_architectures "$label" "${pair#*:}")
    [ "$launcher_architectures" = "$architectures" ] ||
        die "binary architectures differ: $BRAND_LAUNCHER_BIN [$launcher_architectures], $label [$architectures]"
done

actual_layout=$(
    cd -- "$bundle"
    find . -mindepth 1 -print | LC_ALL=C sort
)
menu_rel="./Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
agent_rel="./Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
payload_rel="./Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID"
ui_rel="$payload_rel/$BRAND_UI_BUNDLE"
expected_layout=$(cat <<EOF
./Contents
./Contents/Info.plist
./Contents/Library
./Contents/Library/Helpers
$menu_rel
$menu_rel/Contents
$menu_rel/Contents/Info.plist
$menu_rel/Contents/MacOS
$menu_rel/Contents/MacOS/$BRAND_MENU_BIN
./Contents/Library/LoginItems
$agent_rel
$agent_rel/Contents
$agent_rel/Contents/Info.plist
$agent_rel/Contents/MacOS
$agent_rel/Contents/MacOS/$BRAND_AGENT_BIN
./Contents/MacOS
./Contents/MacOS/$BRAND_LAUNCHER_BIN
./Contents/MacOS/$BRAND_UI_BIN
./Contents/Resources
./Contents/Resources/$BRAND_ICON_NAME.icns
./Contents/Resources/frontends
$payload_rel
$ui_rel
$ui_rel/Contents
$ui_rel/Contents/Info.plist
$ui_rel/Contents/MacOS
$ui_rel/Contents/MacOS/$BRAND_UI_BIN
$payload_rel/frontend.toml
$payload_rel/recovery.sha256
EOF
)

main_signature="$bundle/Contents/_CodeSignature"
agent_signature="$agent_bundle/Contents/_CodeSignature"
menu_signature="$menu_bundle/Contents/_CodeSignature"
ui_signature="$ui_bundle/Contents/_CodeSignature"
if [ -e "$main_signature" ] || [ -L "$main_signature" ] ||
    [ -e "$agent_signature" ] || [ -L "$agent_signature" ] ||
    [ -e "$menu_signature" ] || [ -L "$menu_signature" ]; then
    require_directory "$main_signature"
    require_directory "$agent_signature"
    require_file "$main_signature/CodeResources"
    require_file "$agent_signature/CodeResources"
    require_directory "$menu_signature"
    require_file "$menu_signature/CodeResources"
    require_directory "$ui_signature"
    require_file "$ui_signature/CodeResources"
    expected_layout=$(printf '%s\n%s\n' "$expected_layout" "$(cat <<EOF
$ui_rel/Contents/_CodeSignature
$ui_rel/Contents/_CodeSignature/CodeResources
$agent_rel/Contents/_CodeSignature
$agent_rel/Contents/_CodeSignature/CodeResources
$menu_rel/Contents/_CodeSignature
$menu_rel/Contents/_CodeSignature/CodeResources
./Contents/_CodeSignature
./Contents/_CodeSignature/CodeResources
EOF
)")
fi

stapled_ticket="$bundle/Contents/CodeResources"
if [ -e "$stapled_ticket" ] || [ -L "$stapled_ticket" ]; then
    [ -d "$main_signature" ] && [ -d "$agent_signature" ] && [ -d "$menu_signature" ] ||
        die 'stapled ticket requires signed App, Agent and Menu bundles'
    require_file "$stapled_ticket"
    expected_layout=$(printf '%s\n%s' "$expected_layout" './Contents/CodeResources')
fi

. "$script_dir/studio-payload.sh"
verify_studio_payload "$bundle"
expected_layout=$(printf '%s\n' "$expected_layout" | LC_ALL=C sort)
[ "$actual_layout" = "$expected_layout" ] || {
    printf 'verify-bundle: unexpected bundle layout\nexpected:\n' >&2
    printf '%s\n' "$expected_layout" | terminal_lines >&2
    printf 'actual:\n' >&2
    printf '%s\n' "$actual_layout" | terminal_lines >&2
    exit 1
}

plutil -lint -- "$main_plist" >/dev/null
plutil -lint -- "$agent_plist" >/dev/null
plutil -lint -- "$menu_plist" >/dev/null
plutil -lint -- "$ui_plist" >/dev/null

for id_key in APP_BUNDLE_ID AGENT_BUNDLE_ID MENU_BUNDLE_ID UI_BUNDLE_ID; do
    for id_override in "${BRAND_ENV_PREFIX}_$id_key" "MAILD_$id_key"; do
        [ -z "${!id_override+x}" ] ||
            die "$id_override override is unsupported; edit brand.toml, run bash scripts/sync-brand.sh and rebuild"
    done
done
app_bundle_id=$BRAND_APP_BUNDLE_ID
agent_bundle_id=$BRAND_AGENT_BUNDLE_ID
menu_bundle_id=$BRAND_MENU_BUNDLE_ID
ui_bundle_id=$BRAND_UI_BUNDLE_ID
for manifest_pair in "app:$app_bundle_id" "agent:$agent_bundle_id" "menu:$menu_bundle_id"; do
    manifest_value=$(read_identity "$script_dir/${manifest_pair%%:*}-bundle-id.txt") || exit 1
    [ "$manifest_value" = "${manifest_pair#*:}" ] ||
        die "${manifest_pair%%:*}-bundle-id.txt differs from brand.env; run bash scripts/sync-brand.sh"
done
short_version_env="${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION"
build_version_env="${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION"
short_version=$(brand_env_value BUNDLE_SHORT_VERSION 0.1.0)
build_version=$(brand_env_value BUNDLE_BUILD_VERSION 1)

validate_bundle_id BRAND_APP_BUNDLE_ID "$app_bundle_id"
validate_bundle_id BRAND_AGENT_BUNDLE_ID "$agent_bundle_id"
validate_bundle_id BRAND_MENU_BUNDLE_ID "$menu_bundle_id"
validate_bundle_id BRAND_UI_BUNDLE_ID "$ui_bundle_id"
[ "$app_bundle_id" != "$agent_bundle_id" ] &&
    [ "$app_bundle_id" != "$menu_bundle_id" ] &&
    [ "$app_bundle_id" != "$ui_bundle_id" ] &&
    [ "$agent_bundle_id" != "$menu_bundle_id" ] &&
    [ "$agent_bundle_id" != "$ui_bundle_id" ] &&
    [ "$menu_bundle_id" != "$ui_bundle_id" ] ||
    die 'app, agent, menu and UI bundle identifiers must differ'
validate_version "$short_version_env" "$short_version"
validate_version "$build_version_env" "$build_version"
require_compiled_identity "$BRAND_LAUNCHER_BIN" "$launcher_executable" "$agent_contract_prefix" "$agent_bundle_id"
require_compiled_identity "$BRAND_AGENT_BIN" "$agent_executable" "$agent_contract_prefix" "$agent_bundle_id"
require_compiled_identity "$BRAND_MENU_BIN" "$menu_executable" "$menu_contract_prefix" "$menu_bundle_id"
require_compiled_identity "$BRAND_UI_BIN" "$ui_executable" "$ui_contract_prefix" "$ui_bundle_id"
# The single-host main window must be the embedded-assets build too; a default
# (devUrl) build placed at Contents/MacOS/<ui_bin> would ship a blank window.
require_compiled_identity "$BRAND_UI_BIN (host)" "$host_ui_executable" "$ui_contract_prefix" "$ui_bundle_id"

expect_plist_string "$main_plist" CFBundleDisplayName "$BRAND_PRODUCT_NAME"
expect_plist_string "$main_plist" CFBundleName "$BRAND_PRODUCT_NAME"
expect_plist_string "$main_plist" CFBundleExecutable "$BRAND_LAUNCHER_BIN"
expect_plist_string "$main_plist" CFBundleIconFile "$BRAND_ICON_NAME"
expect_plist_string "$main_plist" CFBundleIdentifier "$app_bundle_id"
expect_plist_string "$main_plist" CFBundlePackageType APPL
expect_plist_string "$main_plist" CFBundleShortVersionString "$short_version"
expect_plist_string "$main_plist" CFBundleVersion "$build_version"
expect_plist_string "$main_plist" LSMinimumSystemVersion 13.0

expect_plist_string "$agent_plist" CFBundleDisplayName "$BRAND_PRODUCT_NAME Agent"
expect_plist_string "$agent_plist" CFBundleExecutable "$BRAND_AGENT_BIN"
expect_plist_string "$agent_plist" CFBundleIdentifier "$agent_bundle_id"
expect_plist_string "$agent_plist" CFBundlePackageType APPL
expect_plist_string "$agent_plist" CFBundleShortVersionString "$short_version"
expect_plist_string "$agent_plist" CFBundleVersion "$build_version"
expect_plist_string "$agent_plist" LSMinimumSystemVersion 13.0

expect_plist_string "$menu_plist" CFBundleDisplayName "$BRAND_PRODUCT_NAME Menu"
expect_plist_string "$menu_plist" CFBundleExecutable "$BRAND_MENU_BIN"
expect_plist_string "$menu_plist" CFBundleIdentifier "$menu_bundle_id"
expect_plist_string "$menu_plist" CFBundlePackageType APPL
expect_plist_string "$menu_plist" CFBundleShortVersionString "$short_version"
expect_plist_string "$menu_plist" CFBundleVersion "$build_version"
expect_plist_string "$menu_plist" LSMinimumSystemVersion 13.0

expect_plist_string "$ui_plist" CFBundleDisplayName "$BRAND_PRODUCT_NAME UI"
expect_plist_string "$ui_plist" CFBundleExecutable "$BRAND_UI_BIN"
expect_plist_string "$ui_plist" CFBundleIdentifier "$ui_bundle_id"
expect_plist_string "$ui_plist" CFBundlePackageType APPL
expect_plist_string "$ui_plist" CFBundleShortVersionString "$short_version"
expect_plist_string "$ui_plist" CFBundleVersion "$build_version"
expect_plist_string "$ui_plist" LSMinimumSystemVersion 13.0
expect_frontend_localizations "$ui_plist"

# The recovery manifest is generated, never hand-edited: it must equal the rendered template.
expected_manifest=$(render_template "$script_dir/templates/frontend.toml.in" toml)
[ "$(cat -- "$ui_manifest")" = "$expected_manifest" ] ||
    die "bundled frontend manifest differs from the template: $ui_manifest"
# Exactly 64 lowercase hex digits and a newline, matching the current (possibly signed) bytes.
[ "$(wc -c <"$ui_checksum" | tr -d ' ')" = 65 ] || die "recovery checksum must be 65 bytes: $ui_checksum"
expected_digest=$(/usr/bin/shasum -a 256 "$ui_executable" | awk '{print $1}')
[ "$(cat -- "$ui_checksum")" = "$expected_digest" ] ||
    die 'recovery checksum does not match the bundled UI executable; run seal-recovery-payload.sh'

agent_ui_element=$(plutil -extract LSUIElement raw -expect bool "$agent_plist") ||
    die "missing boolean LSUIElement in $agent_plist"
[ "$agent_ui_element" = true ] || die 'agent LSUIElement must be true'
menu_ui_element=$(plutil -extract LSUIElement raw -expect bool "$menu_plist") ||
    die "missing boolean LSUIElement in $menu_plist"
[ "$menu_ui_element" = true ] || die 'menu LSUIElement must be true'
# Menu-bar-first: the top-level app starts as LSUIElement and the UI switches to Regular only
# while a window is visible, so Dock keeps no icon or recent-apps slot after the window closes.
main_ui_element=$(plutil -extract LSUIElement raw -expect bool "$main_plist") ||
    die "missing boolean LSUIElement in $main_plist"
[ "$main_ui_element" = true ] || die "top-level $BRAND_APP_BUNDLE LSUIElement must be true"

printf 'verified: %s\n' "$bundle"
