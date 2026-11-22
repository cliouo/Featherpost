#!/bin/bash
set -euo pipefail
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

die() {
    printf 'release-bundle: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage:
  packaging/macos/release-bundle.sh --app PATH --version X.Y.Z --preflight-only

  packaging/macos/release-bundle.sh --app PATH --version X.Y.Z \
    --identity IDENTITY --keychain PATH \
    --notary-key PATH --notary-key-id KEYID --notary-issuer UUID \
    --output-dir PATH

The full command copies an unsigned universal2 bundle into private staging,
signs the nested UI, Agent, Menu and the single-host Contents/MacOS/$BRAND_UI_BIN
before the top-level $BRAND_APP_BUNDLE with the hardened runtime and secure timestamps,
notarizes a ZIP, staples the App, verifies Gatekeeper, then creates and
re-verifies the final ZIP and SHA-256 file. It never modifies the input App.
Names come from packaging/macos/brand.env. Formal release preflight requires a
clean exact vX.Y.Z checkout and rejects provisional dev.$BRAND_SLUG.* identifiers.
EOF
}

# The repository's development identity namespace (and the pre-rebrand one) is never released.
is_provisional_id() {
    case "$1" in
        "dev.$BRAND_SLUG".*|dev.maild.*) return 0 ;;
    esac
    return 1
}

manifest_version() {
    local manifest=$1
    local value
    value=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$manifest" | head -n 1)
    [ -n "$value" ] || die "cannot read package version from $manifest"
    printf '%s' "$value"
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

validate_source_preflight() {
    printf '%s\n' "$version" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
        die 'version must use X.Y.Z numeric form'
    /usr/bin/git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
        die 'release must run from a Git worktree'
    [ -z "$(/usr/bin/git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ] ||
        die 'release requires a clean worktree'
    /usr/bin/git -C "$repo_root" tag --points-at HEAD | LC_ALL=C grep -Fqx "v$version" ||
        die "HEAD must have the exact release tag v$version"

    for manifest in \
        "$repo_root/crates/maild/Cargo.toml" \
        "$repo_root/crates/maild-launcher/Cargo.toml" \
        "$repo_root/crates/maild-menu/Cargo.toml" \
        "$repo_root/crates/maild-ui/Cargo.toml"; do
        [ "$(manifest_version "$manifest")" = "$version" ] ||
            die "package version does not match v$version: $manifest"
    done

    [ ! -e "$app/Contents/_CodeSignature" ] && [ ! -L "$app/Contents/_CodeSignature" ] ||
        die 'release input App must be unsigned'
    agent="$app/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
    menu="$app/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
    ui="$app/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID/$BRAND_UI_BUNDLE"
    [ ! -e "$ui/Contents/_CodeSignature" ] && [ ! -L "$ui/Contents/_CodeSignature" ] ||
        die 'release input UI must be unsigned'
    . "$script_dir/studio-payload.sh"
    studio_payload=$(studio_payload_path "$app")
    [ ! -e "$studio_payload/$STUDIO_APP/Contents/_CodeSignature" ] &&
        [ ! -L "$studio_payload/$STUDIO_APP/Contents/_CodeSignature" ] ||
        die 'release input Studio must be unsigned'
    [ ! -e "$agent/Contents/_CodeSignature" ] &&
        [ ! -L "$agent/Contents/_CodeSignature" ] ||
        die 'release input Agent must be unsigned'
    [ ! -e "$menu/Contents/_CodeSignature" ] &&
        [ ! -L "$menu/Contents/_CodeSignature" ] ||
        die 'release input Menu must be unsigned'
    [ ! -e "$app/Contents/CodeResources" ] && [ ! -L "$app/Contents/CodeResources" ] ||
        die 'release input App must not contain a stapled ticket'

    env "$short_version_env=$version" "$build_version_env=$version" \
        "$script_dir/verify-bundle.sh" "$app" >/dev/null

    app_id=$(plist_string "$app/Contents/Info.plist" CFBundleIdentifier) ||
        die 'cannot read App bundle identifier'
    agent_id=$(plist_string "$agent/Contents/Info.plist" CFBundleIdentifier) ||
        die 'cannot read Agent bundle identifier'
    menu_id=$(plist_string "$menu/Contents/Info.plist" CFBundleIdentifier) ||
        die 'cannot read Menu bundle identifier'
    ! is_provisional_id "$app_id" ||
        die "provisional App bundle identifier cannot be released: $app_id"
    ! is_provisional_id "$agent_id" ||
        die "provisional Agent bundle identifier cannot be released: $agent_id"
    ! is_provisional_id "$menu_id" ||
        die "provisional Menu bundle identifier cannot be released: $menu_id"

    expected_architectures='arm64 x86_64'
    [ "$(normalize_architectures "$app/Contents/MacOS/$BRAND_LAUNCHER_BIN")" = \
        "$expected_architectures" ] ||
        die "$BRAND_LAUNCHER_BIN release must contain exactly arm64 and x86_64"
    [ "$(normalize_architectures "$ui/Contents/MacOS/$BRAND_UI_BIN")" = \
        "$expected_architectures" ] ||
        die "$BRAND_UI_BIN release must contain exactly arm64 and x86_64"
    [ "$(normalize_architectures "$app/Contents/MacOS/$BRAND_UI_BIN")" = \
        "$expected_architectures" ] ||
        die "host $BRAND_UI_BIN release must contain exactly arm64 and x86_64"
    [ "$(normalize_architectures "$agent/Contents/MacOS/$BRAND_AGENT_BIN")" = \
        "$expected_architectures" ] ||
        die "$BRAND_AGENT_BIN release must contain exactly arm64 and x86_64"
    [ "$(normalize_architectures "$menu/Contents/MacOS/$BRAND_MENU_BIN")" = \
        "$expected_architectures" ] ||
        die "$BRAND_MENU_BIN release must contain exactly arm64 and x86_64"
}

validate_private_file() {
    local label=$1
    local path=$2
    [ ! -L "$path" ] || die "$label must not be a symbolic link: $path"
    [ -f "$path" ] || die "$label is not a regular file: $path"
    local owner mode
    owner=$(/usr/bin/stat -f '%u' "$path") || die "cannot inspect $label owner: $path"
    mode=$(/usr/bin/stat -f '%Lp' "$path") || die "cannot inspect $label permissions: $path"
    [ "$owner" -eq "$(id -u)" ] || die "$label must be owned by the current user: $path"
    [ $((8#$mode & 077)) -eq 0 ] || die "$label permissions must not allow group/other access"
}

validate_output_directory() {
    local path=$1
    case "$path" in
        /*) ;;
        *) path="$(pwd -P)/$path" ;;
    esac
    [ ! -L "$path" ] || die "output directory must not be a symbolic link: $path"
    [ -d "$path" ] || die "output directory must already exist: $path"
    local leaf=$path
    local current=$path
    local current_uid owner mode
    current_uid=$(id -u)
    while :; do
        [ ! -L "$current" ] || die "output path ancestor must not be a symbolic link: $current"
        [ -d "$current" ] || die "output path ancestor is not a directory: $current"
        owner=$(/usr/bin/stat -f '%u' "$current") || die "cannot inspect output path owner: $current"
        mode=$(/usr/bin/stat -f '%Lp' "$current") || die "cannot inspect output path permissions: $current"
        if [ "$current" = "$leaf" ]; then
            [ "$owner" -eq "$current_uid" ] ||
                die "output directory must be owned by the current user: $current"
            [ $((8#$mode & 022)) -eq 0 ] ||
                die "output directory must not be group/world writable: $current"
        else
            { [ "$owner" -eq "$current_uid" ] || [ "$owner" -eq 0 ]; } ||
                die "output ancestor must be owned by the current user or root: $current"
            if [ $((8#$mode & 022)) -ne 0 ]; then
                [ "$owner" -eq 0 ] && [ $((8#$mode & 01000)) -ne 0 ] ||
                    die "writable output ancestor must be root-owned and sticky: $current"
            fi
        fi
        [ "$current" != / ] || break
        current=$(dirname -- "$current")
    done
    path=$(CDPATH= cd -- "$path" && pwd -P)
    printf '%s' "$path"
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"
short_version_env="${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION"
build_version_env="${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION"

app=
version=
identity=
keychain=
notary_key=
notary_key_id=
notary_issuer=
output_dir=
preflight_only=false

while [ "$#" -gt 0 ]; do
    case "$1" in
        --app|--version|--identity|--keychain|--notary-key|--notary-key-id|--notary-issuer|--output-dir)
            [ "$#" -ge 2 ] || die "$1 requires a value"
            option=$1
            value=$2
            shift 2
            case "$option" in
                --app) app=$value ;;
                --version) version=$value ;;
                --identity) identity=$value ;;
                --keychain) keychain=$value ;;
                --notary-key) notary_key=$value ;;
                --notary-key-id) notary_key_id=$value ;;
                --notary-issuer) notary_issuer=$value ;;
                --output-dir) output_dir=$value ;;
            esac
            ;;
        --preflight-only)
            preflight_only=true
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
[ "$(/usr/bin/uname -s)" = Darwin ] || die 'macOS is required'
for tool in /usr/bin/ditto /usr/bin/git /usr/bin/lipo /usr/bin/plutil /usr/bin/stat /usr/bin/xcrun; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

validate_source_preflight
read_trusted_team_id >/dev/null
if [ "$preflight_only" = true ]; then
    printf 'release preflight passed: %s\n' "$app"
    exit 0
fi

for required in identity keychain notary_key notary_key_id notary_issuer output_dir; do
    eval "value=\${$required}"
    [ -n "$value" ] || die "--${required//_/-} is required"
done
printf '%s\n' "$notary_key_id" | LC_ALL=C grep -Eq '^[A-Z0-9]{10,}$' ||
    die 'notary key ID must contain at least 10 uppercase ASCII letters or digits'
printf '%s\n' "$notary_issuer" | LC_ALL=C grep -Eq \
    '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' ||
    die 'notary issuer must be a UUID'
validate_private_file 'signing keychain' "$keychain"
validate_private_file 'notary API key' "$notary_key"
output_dir=$(validate_output_directory "$output_dir")

archive_name="$BRAND_PRODUCT_NAME-v$version-universal2.zip"
checksum_name="$archive_name.sha256"
result_name="$BRAND_PRODUCT_NAME-v$version-notary.plist"
log_name="$BRAND_PRODUCT_NAME-v$version-notary-log.json"
for leaf in "$archive_name" "$checksum_name" "$result_name" "$log_name"; do
    [ ! -e "$output_dir/$leaf" ] && [ ! -L "$output_dir/$leaf" ] ||
        die "release output already exists: $output_dir/$leaf"
done

work=$(mktemp -d "$output_dir/.$BRAND_SLUG-release.XXXXXX") ||
    die 'cannot create private release staging directory'
chmod 0700 "$work"
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

staged_app="$work/$BRAND_APP_BUNDLE"
/usr/bin/ditto --noqtn --noextattr --noacl "$app" "$staged_app"
env "$short_version_env=$version" "$build_version_env=$version" \
    "$script_dir/verify-bundle.sh" "$staged_app" >/dev/null
staged_agent="$staged_app/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
staged_menu="$staged_app/Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
staged_ui="$staged_app/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID/$BRAND_UI_BUNDLE"
staged_host_ui="$staged_app/Contents/MacOS/$BRAND_UI_BIN"

# Sign from the inside out. --deep is intentionally forbidden for signing.
# Signing rewrites the UI Mach-O; reseal its recovery checksum before sealing the app.
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
    --options runtime --timestamp "$staged_ui"
. "$script_dir/studio-payload.sh"
studio_payload=$(studio_payload_path "$staged_app")
if [ -d "$studio_payload" ]; then
    /usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
        --options runtime --timestamp "$studio_payload/$STUDIO_APP"
fi
"$script_dir/seal-recovery-payload.sh" "$staged_app"
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
    --options runtime --timestamp "$staged_agent"
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
    --options runtime --timestamp "$staged_menu"
# Signing the app seals only its CFBundleExecutable; the single-host UI sibling needs its
# own Developer ID + hardened runtime + timestamp signature or notary rejects it.
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
    --identifier "$BRAND_UI_BUNDLE_ID" --options runtime --timestamp "$staged_host_ui"
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
    --options runtime --timestamp "$staged_app"
"$script_dir/verify-release.sh" \
    --app "$staged_app" --version "$version" >/dev/null

pre_staple_archive="$work/$BRAND_PRODUCT_NAME-pre-staple.zip"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$staged_app" "$pre_staple_archive"
notary_result="$work/notary-result.plist"
set +e
/usr/bin/xcrun notarytool submit "$pre_staple_archive" \
    --key "$notary_key" \
    --key-id "$notary_key_id" \
    --issuer "$notary_issuer" \
    --wait --timeout 30m --no-progress --output-format plist >"$notary_result"
notary_exit=$?
set -e
notary_status=$(/usr/bin/plutil -extract status raw "$notary_result" 2>/dev/null || true)
if [ "$notary_exit" -ne 0 ] || [ "$notary_status" != Accepted ]; then
    cp -X -n -- "$notary_result" "$output_dir/$result_name" 2>/dev/null || true
    submission_id=$(/usr/bin/plutil -extract id raw "$notary_result" 2>/dev/null || true)
    if [ -n "$submission_id" ]; then
        notary_log="$work/notary-log.json"
        /usr/bin/xcrun notarytool log "$submission_id" "$notary_log" \
            --key "$notary_key" --key-id "$notary_key_id" --issuer "$notary_issuer" \
            >/dev/null 2>&1 || true
        if [ -f "$notary_log" ] && [ ! -L "$notary_log" ]; then
            cp -X -n -- "$notary_log" "$output_dir/$log_name" 2>/dev/null || true
        fi
    fi
    die "notarization was not accepted (status: ${notary_status:-unavailable})"
fi

/usr/bin/xcrun stapler staple -v "$staged_app"
/usr/bin/xcrun stapler validate -v "$staged_app"
"$script_dir/verify-release.sh" \
    --app "$staged_app" --version "$version" --stapled >/dev/null

final_archive="$work/$archive_name"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$staged_app" "$final_archive"
extracted="$work/extracted"
mkdir -- "$extracted"
/usr/bin/ditto -x -k "$final_archive" "$extracted"
[ -d "$extracted/$BRAND_APP_BUNDLE" ] ||
    die "final archive does not contain top-level $BRAND_APP_BUNDLE"
"$script_dir/verify-release.sh" \
    --app "$extracted/$BRAND_APP_BUNDLE" --version "$version" --stapled \
    >/dev/null

hash=$(/usr/bin/shasum -a 256 "$final_archive" | awk '{print $1}')
printf '%s  %s\n' "$hash" "$archive_name" >"$work/$checksum_name"
cp -X -- "$notary_result" "$work/$result_name"

for leaf in "$archive_name" "$checksum_name" "$result_name"; do
    [ ! -e "$output_dir/$leaf" ] && [ ! -L "$output_dir/$leaf" ] ||
        die "release output appeared while publishing: $output_dir/$leaf"
    mv -n -- "$work/$leaf" "$output_dir/"
    [ ! -e "$work/$leaf" ] || die "could not publish release output: $leaf"
done

printf '%s\n' "$output_dir/$archive_name"
printf '%s\n' "$output_dir/$checksum_name"
printf '%s\n' "$output_dir/$result_name"
