#!/bin/bash
set -euo pipefail

die() {
    printf 'build-bundle: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: packaging/macos/build-bundle.sh [options]

Options:
  --launcher-bin PATH Built $BRAND_LAUNCHER_BIN executable (top-level $BRAND_APP_BUNDLE entry).
  --agent-bin PATH    Built $BRAND_AGENT_BIN executable.
  --menu-bin PATH     Built $BRAND_MENU_BIN executable.
  --ui-bin PATH       Built embedded $BRAND_UI_BIN executable (\`--features embedded\`).
  --studio-bin PATH   Also bundle the embedded Studio frontend (product builds supply this).
  --output PATH       Destination; its final component must be $BRAND_APP_BUNDLE.
  -h, --help          Show this help.

Every product name, bundle name/identifier, binary name and the icon come from
packaging/macos/brand.env, which scripts/sync-brand.sh generates from brand.toml.
Defaults use target/release binaries and dist/macos/$BRAND_APP_BUNDLE. When only
--launcher-bin is given, Menu and UI default to siblings in the same directory.
The embedded $BRAND_UI_BIN is installed twice: as the single-host main window at
Contents/MacOS/$BRAND_UI_BIN (exec'd in place by $BRAND_LAUNCHER_BIN) and as the
recovery copy at Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID with a rendered
manifest and recovery.sha256. assets/AppIcon.icns becomes
Contents/Resources/$BRAND_ICON_NAME.icns. Versions come from
${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION / ${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION (legacy
MAILD_BUNDLE_* is still read as a fallback). The command never overwrites an
existing destination, signs code, registers a login item, launches any
executable, or modifies platform configuration/data directories.
EOF
}

require_regular_executable() {
    local label=$1
    local path=$2
    [ ! -L "$path" ] || die "$label must not be a symbolic link: $path"
    [ -f "$path" ] || die "$label is not a regular file: $path"
    [ -x "$path" ] || die "$label is not executable: $path"
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

# Render a template: every __BRAND_<KEY>__ comes from brand.env, plus the two bundle versions.
# FORMAT is xml (values are XML-escaped) or toml (brand.toml forbids quotes and backslashes).
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

validate_bundle_name() {
    local label=$1
    local value=$2
    case "$value" in
        */*|.app|'') die "$label is not a valid bundle file name: $value" ;;
        *.app) ;;
        *) die "$label must end in .app: $value" ;;
    esac
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

validate_output_parent() {
    local leaf=$1
    local current=$1
    local current_uid
    current_uid=$(id -u)
    while :; do
        local owner mode_text mode
        owner=$(stat -f '%u' "$current") || die "cannot inspect output ancestor: $current"
        mode_text=$(stat -f '%Mp%Lp' "$current") ||
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
command -v plutil >/dev/null 2>&1 || die 'plutil is required'
command -v lipo >/dev/null 2>&1 || die 'lipo is required'
command -v strings >/dev/null 2>&1 || die 'strings is required'

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
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
# Stable binary-scan marker prefixes (maild-brand AGENT/MENU/UI_EMBEDDED_CONTRACT_PREFIX).
agent_contract_prefix='maild-agent-bundle-id:'
menu_contract_prefix='maild-menu-bundle-id:'
ui_contract_prefix='maild-ui-embedded:'
launcher_bin="$repo_root/target/release/$BRAND_LAUNCHER_BIN"
agent_bin="$repo_root/target/release/$BRAND_AGENT_BIN"
menu_bin="$repo_root/target/release/$BRAND_MENU_BIN"
ui_bin="$repo_root/target/release/$BRAND_UI_BIN"
menu_bin_explicit=false
ui_bin_explicit=false
studio_bin=
output="$repo_root/dist/macos/$BRAND_APP_BUNDLE"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --launcher-bin)
            [ "$#" -ge 2 ] || die '--launcher-bin requires a path'
            launcher_bin=$2
            shift 2
            ;;
        --ui-bin)
            [ "$#" -ge 2 ] || die '--ui-bin requires a path'
            ui_bin=$2
            ui_bin_explicit=true
            shift 2
            ;;
        --studio-bin)
            [ "$#" -ge 2 ] || die '--studio-bin requires a path'
            studio_bin=$2
            shift 2
            ;;
        --agent-bin)
            [ "$#" -ge 2 ] || die '--agent-bin requires a path'
            agent_bin=$2
            shift 2
            ;;
        --menu-bin)
            [ "$#" -ge 2 ] || die '--menu-bin requires a path'
            menu_bin=$2
            menu_bin_explicit=true
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
        *)
            die "unknown argument: $1"
            ;;
    esac
done

if [ "$launcher_bin" != "$repo_root/target/release/$BRAND_LAUNCHER_BIN" ]; then
    [ "$menu_bin_explicit" = true ] || menu_bin=$(dirname -- "$launcher_bin")/$BRAND_MENU_BIN
    [ "$ui_bin_explicit" = true ] || ui_bin=$(dirname -- "$launcher_bin")/$BRAND_UI_BIN
fi

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
# The Rust binaries compile in the *-bundle-id.txt manifests; both are generated from brand.toml.
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
validate_bundle_name BRAND_APP_BUNDLE "$BRAND_APP_BUNDLE"
validate_bundle_name BRAND_AGENT_BUNDLE "$BRAND_AGENT_BUNDLE"
validate_bundle_name BRAND_MENU_BUNDLE "$BRAND_MENU_BUNDLE"
validate_bundle_name BRAND_UI_BUNDLE "$BRAND_UI_BUNDLE"
validate_version "$short_version_env" "$short_version"
validate_version "$build_version_env" "$build_version"
require_regular_executable "$BRAND_LAUNCHER_BIN" "$launcher_bin"
require_regular_executable "$BRAND_AGENT_BIN" "$agent_bin"
require_regular_executable "$BRAND_MENU_BIN" "$menu_bin"
require_regular_executable "$BRAND_UI_BIN" "$ui_bin"
icon_source="$script_dir/assets/AppIcon.icns"
[ ! -L "$icon_source" ] && [ -f "$icon_source" ] ||
    die "missing application icon: $icon_source"
require_compiled_identity "$BRAND_LAUNCHER_BIN" "$launcher_bin" "$agent_contract_prefix" "$agent_bundle_id"
require_compiled_identity "$BRAND_AGENT_BIN" "$agent_bin" "$agent_contract_prefix" "$agent_bundle_id"
require_compiled_identity "$BRAND_MENU_BIN" "$menu_bin" "$menu_contract_prefix" "$menu_bundle_id"
# Only `--features embedded` serves bundled assets; a default build would load devUrl.
require_compiled_identity "$BRAND_UI_BIN" "$ui_bin" "$ui_contract_prefix" "$ui_bundle_id"
. "$script_dir/studio-payload.sh"
if [ -n "$studio_bin" ]; then
    require_regular_executable Studio "$studio_bin"
    require_compiled_identity Studio "$studio_bin" "$ui_contract_prefix" "$STUDIO_FRONTEND_ID"
    [ "$(binary_architectures Studio "$studio_bin")" = "$(binary_architectures launcher "$launcher_bin")" ] ||
        die 'Studio binary architectures differ from the launcher'
fi
launcher_architectures=$(binary_architectures "$BRAND_LAUNCHER_BIN" "$launcher_bin")
for pair in "$BRAND_AGENT_BIN:$agent_bin" "$BRAND_MENU_BIN:$menu_bin" "$BRAND_UI_BIN:$ui_bin"; do
    label=${pair%%:*}
    architectures=$(binary_architectures "$label" "${pair#*:}")
    [ "$launcher_architectures" = "$architectures" ] ||
        die "binary architectures differ: $BRAND_LAUNCHER_BIN [$launcher_architectures], $label [$architectures]"
done

[ "$(basename -- "$output")" = "$BRAND_APP_BUNDLE" ] ||
    die "the output final component must be $BRAND_APP_BUNDLE"
output_parent=$(dirname -- "$output")
umask 022
mkdir -p -- "$output_parent"
output_parent=$(CDPATH= cd -- "$output_parent" && pwd -P)
validate_output_parent "$output_parent"
output="$output_parent/$BRAND_APP_BUNDLE"
[ ! -e "$output" ] && [ ! -L "$output" ] || die "destination already exists: $output"

staging_root=$(mktemp -d "$output_parent/.$BRAND_SLUG-bundle.XXXXXX") ||
    die "cannot create staging directory under $output_parent"
cleanup() {
    rm -rf -- "$staging_root"
}
trap cleanup EXIT HUP INT TERM

bundle="$staging_root/$BRAND_APP_BUNDLE"
agent_bundle="$bundle/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu_bundle="$bundle/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
ui_payload="$bundle/Contents/Resources/frontends/$ui_bundle_id"
ui_bundle="$ui_payload/$BRAND_UI_BUNDLE"
mkdir -p -- \
    "$bundle/Contents/MacOS" \
    "$agent_bundle/Contents/MacOS" \
    "$menu_bundle/Contents/MacOS" \
    "$ui_bundle/Contents/MacOS"

cp -X -- "$launcher_bin" "$bundle/Contents/MacOS/$BRAND_LAUNCHER_BIN"
# Single-host layout: the launcher execs this sibling in place, so the main window runs
# under the top-level app bundle identity (one Dock icon). The recovery copy below stays
# byte-identical in the frontend payload.
cp -X -- "$ui_bin" "$bundle/Contents/MacOS/$BRAND_UI_BIN"
cp -X -- "$agent_bin" "$agent_bundle/Contents/MacOS/$BRAND_AGENT_BIN"
cp -X -- "$menu_bin" "$menu_bundle/Contents/MacOS/$BRAND_MENU_BIN"
cp -X -- "$ui_bin" "$ui_bundle/Contents/MacOS/$BRAND_UI_BIN"
if [ -n "$studio_bin" ]; then copy_studio_payload "$bundle" "$studio_bin"; fi
chmod 0755 \
    "$bundle/Contents/MacOS/$BRAND_LAUNCHER_BIN" \
    "$bundle/Contents/MacOS/$BRAND_UI_BIN" \
    "$agent_bundle/Contents/MacOS/$BRAND_AGENT_BIN" \
    "$menu_bundle/Contents/MacOS/$BRAND_MENU_BIN" \
    "$ui_bundle/Contents/MacOS/$BRAND_UI_BIN"

mkdir -p -- "$bundle/Contents/Resources"
cp -X -- "$icon_source" "$bundle/Contents/Resources/$BRAND_ICON_NAME.icns"
chmod 0644 "$bundle/Contents/Resources/$BRAND_ICON_NAME.icns"
render_template "$script_dir/templates/App.Info.plist.in" xml >"$bundle/Contents/Info.plist"
render_template "$script_dir/templates/Agent.Info.plist.in" xml >"$agent_bundle/Contents/Info.plist"
render_template "$script_dir/templates/Menu.Info.plist.in" xml >"$menu_bundle/Contents/Info.plist"
render_template "$script_dir/templates/UI.Info.plist.in" xml >"$ui_bundle/Contents/Info.plist"
render_template "$script_dir/templates/frontend.toml.in" toml >"$ui_payload/frontend.toml"
chmod 0644 \
    "$bundle/Contents/Info.plist" \
    "$agent_bundle/Contents/Info.plist" \
    "$menu_bundle/Contents/Info.plist" \
    "$ui_bundle/Contents/Info.plist" \
    "$ui_payload/frontend.toml"
"$script_dir/seal-recovery-payload.sh" "$bundle"

env "$short_version_env=$short_version" "$build_version_env=$build_version" \
    "$script_dir/verify-bundle.sh" "$bundle" >/dev/null

[ ! -e "$output" ] && [ ! -L "$output" ] || die "destination appeared while building: $output"
mv -n -- "$bundle" "$output_parent/"
[ ! -e "$bundle" ] && [ ! -L "$bundle" ] ||
    die "destination appeared while publishing; existing path was preserved: $output"

printf '%s\n' "$output"
