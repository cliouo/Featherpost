#!/bin/bash
set -euo pipefail
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

die() {
    printf 'verify-release: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: packaging/macos/verify-release.sh --app PATH --version X.Y.Z [--stapled] [--allow-missing-agent-icon]

Validates a Developer ID-signed universal2 $BRAND_APP_BUNDLE, including the separately
signed single-host Contents/MacOS/$BRAND_UI_BIN. Names come from brand.env. The verifier requires
the hardened runtime, secure timestamps, a single expected Team ID, no
entitlements, and non-provisional bundle identifiers. --stapled additionally
requires a valid stapled ticket and a successful Gatekeeper assessment.
Use --allow-missing-agent-icon only for existing bundles from before the Agent had an icon.
EOF
}

is_provisional_id() {
    case "$1" in
        "dev.$BRAND_SLUG".*|dev.maild.*) return 0 ;;
    esac
    return 1
}

plist_string() {
    /usr/bin/plutil -extract "$2" raw -expect string "$1"
}

normalize_architectures() {
    /usr/bin/lipo -archs "$1" 2>/dev/null | tr ' ' '\n' | LC_ALL=C sort | paste -sd ' ' -
}

read_trusted_team_id() {
    local manifest="$script_dir/team-id.txt" line_count value
    [ ! -L "$manifest" ] || die "trusted Team ID manifest must not be a symbolic link: $manifest"
    [ -f "$manifest" ] || die "trusted Team ID manifest is missing: $manifest"
    line_count=$(/usr/bin/wc -l <"$manifest") || die 'cannot inspect trusted Team ID manifest'
    [ "$line_count" -eq 1 ] || die 'trusted Team ID manifest must contain exactly one line'
    value=$(/bin/cat -- "$manifest") || die 'cannot read trusted Team ID manifest'
    case "$value" in
        *$'\n'*|*$'\r'*) die 'trusted Team ID manifest must contain exactly one line' ;;
        UNSET) die 'trusted Team ID is UNSET; freeze it before formal release' ;;
    esac
    printf '%s\n' "$value" | LC_ALL=C grep -Eq '^[A-Z0-9]{10}$' ||
        die 'trusted Team ID must contain exactly 10 uppercase ASCII letters or digits'
    printf '%s' "$value"
}

validate_signed_structure() {
    local label=$1
    local code=$2
    local expected_identifier=$3
    local details entitlements

    /usr/bin/codesign --verify --strict=all --verbose=4 "$code" >/dev/null 2>&1 ||
        die "$label code signature is invalid"
    details=$(/usr/bin/codesign --display --verbose=4 "$code" 2>&1) ||
        die "cannot inspect $label code signature"
    printf '%s\n' "$details" | LC_ALL=C grep -Fqx "Identifier=$expected_identifier" ||
        die "$label signing identifier does not match $expected_identifier"
    printf '%s\n' "$details" | LC_ALL=C grep -Eq \
        '^CodeDirectory .* flags=.*[(].*runtime.*[)]' ||
        die "$label does not enable the hardened runtime"

    entitlements=$(mktemp "${TMPDIR:-/tmp}/$BRAND_SLUG-entitlements.XXXXXX") ||
        die "cannot create entitlements inspection file for $label"
    : >"$entitlements"
    if ! /usr/bin/codesign --display --entitlements "$entitlements" "$code" \
        >/dev/null 2>&1; then
        rm -f -- "$entitlements"
        die "cannot inspect $label entitlements"
    fi
    if [ -s "$entitlements" ]; then
        if /usr/bin/plutil -extract com.apple.security.get-task-allow raw "$entitlements" \
            >/dev/null 2>&1; then
            rm -f -- "$entitlements"
            die "$label contains the forbidden get-task-allow entitlement"
        fi
        rm -f -- "$entitlements"
        die "$label must not contain entitlements"
    fi
    rm -f -- "$entitlements"
}

validate_release_authority() {
    local label=$1
    local code=$2
    local details

    details=$(/usr/bin/codesign --display --verbose=4 "$code" 2>&1) ||
        die "cannot inspect $label code signature"
    printf '%s\n' "$details" | LC_ALL=C grep -Fqx "TeamIdentifier=$team_id" ||
        die "$label TeamIdentifier does not match $team_id"
    printf '%s\n' "$details" | LC_ALL=C grep -Eq '^Authority=Developer ID Application:' ||
        die "$label is not signed with a Developer ID Application certificate"
    printf '%s\n' "$details" | LC_ALL=C grep -Eq '^Timestamp=' ||
        die "$label does not contain a secure timestamp"
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"

app=
version=
require_stapled=false
allow_missing_agent_icon=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --app)
            [ "$#" -ge 2 ] || die '--app requires a path'
            app=$2
            shift 2
            ;;
        --version)
            [ "$#" -ge 2 ] || die '--version requires a value'
            version=$2
            shift 2
            ;;
        --stapled)
            require_stapled=true
            shift
            ;;
        --allow-missing-agent-icon)
            allow_missing_agent_icon=true
            shift
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

[ -n "$app" ] || die '--app is required'
[ -n "$version" ] || die '--version is required'
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    die 'version must use X.Y.Z numeric form'
[ "$(/usr/bin/uname -s)" = Darwin ] || die 'macOS is required'
for tool in /usr/bin/codesign /usr/sbin/spctl /usr/bin/lipo /usr/bin/plutil /usr/bin/xcrun; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

if [ "$allow_missing_agent_icon" = true ]; then
    set -- --allow-missing-agent-icon
fi
env "${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=$version" \
    "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=$version" \
    "$script_dir/verify-bundle.sh" "$@" "$app" >/dev/null

main_plist="$app/Contents/Info.plist"
agent="$app/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu="$app/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
agent_plist="$agent/Contents/Info.plist"
menu_plist="$menu/Contents/Info.plist"
launcher_executable="$app/Contents/MacOS/$BRAND_LAUNCHER_BIN"
ui="$app/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID/$BRAND_UI_BUNDLE"
ui_executable="$ui/Contents/MacOS/$BRAND_UI_BIN"
host_ui_executable="$app/Contents/MacOS/$BRAND_UI_BIN"
agent_executable="$agent/Contents/MacOS/$BRAND_AGENT_BIN"
menu_executable="$menu/Contents/MacOS/$BRAND_MENU_BIN"
app_id=$(plist_string "$main_plist" CFBundleIdentifier) || die 'cannot read App bundle identifier'
agent_id=$(plist_string "$agent_plist" CFBundleIdentifier) || die 'cannot read Agent bundle identifier'
menu_id=$(plist_string "$menu_plist" CFBundleIdentifier) || die 'cannot read Menu bundle identifier'
! is_provisional_id "$app_id" || die "provisional App bundle identifier cannot be released: $app_id"
! is_provisional_id "$agent_id" || die "provisional Agent bundle identifier cannot be released: $agent_id"
! is_provisional_id "$menu_id" || die "provisional Menu bundle identifier cannot be released: $menu_id"

expected_architectures='arm64 x86_64'
[ "$(normalize_architectures "$launcher_executable")" = "$expected_architectures" ] ||
    die "$BRAND_LAUNCHER_BIN release must contain exactly arm64 and x86_64"
[ "$(normalize_architectures "$ui_executable")" = "$expected_architectures" ] ||
    die "$BRAND_UI_BIN release must contain exactly arm64 and x86_64"
[ "$(normalize_architectures "$host_ui_executable")" = "$expected_architectures" ] ||
    die "host $BRAND_UI_BIN release must contain exactly arm64 and x86_64"
[ "$(normalize_architectures "$agent_executable")" = "$expected_architectures" ] ||
    die "$BRAND_AGENT_BIN release must contain exactly arm64 and x86_64"
[ "$(normalize_architectures "$menu_executable")" = "$expected_architectures" ] ||
    die "$BRAND_MENU_BIN release must contain exactly arm64 and x86_64"

. "$script_dir/studio-payload.sh"
studio_payload=$(studio_payload_path "$app")
if [ -d "$studio_payload" ]; then
    [ "$(normalize_architectures "$studio_payload/$STUDIO_APP/Contents/MacOS/$STUDIO_BIN")" = "$expected_architectures" ] ||
        die "$STUDIO_BIN release must contain exactly arm64 and x86_64"
    [ "$(normalize_architectures "$(studio_host_path "$app")")" = "$expected_architectures" ] ||
        die "host $STUDIO_BIN release must contain exactly arm64 and x86_64"
fi

team_id=$(read_trusted_team_id)
# Check structural signature invariants (validity, signing identifier, hardened runtime, and
# forbidden entitlements) across all components before verifying Developer ID trust, so any
# missing explicit single-host signature is caught by component name.
validate_signed_structure "$BRAND_PRODUCT_NAME host UI" "$host_ui_executable" "$BRAND_UI_BUNDLE_ID"
if [ -d "$studio_payload" ]; then
    validate_signed_structure "$BRAND_PRODUCT_NAME host Studio" "$(studio_host_path "$app")" "$STUDIO_FRONTEND_ID"
fi
validate_signed_structure "$BRAND_PRODUCT_NAME Agent" "$agent" "$agent_id"
validate_signed_structure "$BRAND_PRODUCT_NAME Menu" "$menu" "$menu_id"
validate_signed_structure "$BRAND_PRODUCT_NAME UI" "$ui" "$BRAND_UI_BUNDLE_ID"
if [ -d "$studio_payload" ]; then
    validate_signed_structure Studio "$studio_payload/$STUDIO_APP" "$STUDIO_FRONTEND_ID"
fi
validate_signed_structure "$BRAND_PRODUCT_NAME" "$app" "$app_id"

validate_release_authority "$BRAND_PRODUCT_NAME host UI" "$host_ui_executable"
if [ -d "$studio_payload" ]; then
    validate_release_authority "$BRAND_PRODUCT_NAME host Studio" "$(studio_host_path "$app")"
fi
validate_release_authority "$BRAND_PRODUCT_NAME Agent" "$agent"
validate_release_authority "$BRAND_PRODUCT_NAME Menu" "$menu"
validate_release_authority "$BRAND_PRODUCT_NAME UI" "$ui"
if [ -d "$studio_payload" ]; then
    validate_release_authority Studio "$studio_payload/$STUDIO_APP"
fi
validate_release_authority "$BRAND_PRODUCT_NAME" "$app"
/usr/bin/codesign --verify --deep --strict=all --verbose=4 "$app" >/dev/null 2>&1 ||
    die 'nested code signature verification failed'

if [ "$require_stapled" = true ]; then
    /usr/bin/xcrun stapler validate -v "$app" >/dev/null ||
        die 'stapled ticket validation failed'
    /usr/sbin/spctl --assess --type execute --verbose=4 "$app" >/dev/null 2>&1 ||
        die 'Gatekeeper assessment failed'
fi

printf 'release verified: %s\n' "$app"
