#!/bin/bash
set -euo pipefail

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

internal_maintenance=false
if [ "${1:-}" = --internal-maintenance ]; then
    internal_maintenance=true
    shift
fi
original_arguments=("$@")

die() {
    printf 'manage-app: ' >&2
    printf '%s' "$*" | LC_ALL=C tr '\000-\037\177' ' ' >&2
    printf '\n' >&2
    exit 1
}

usage() {
    cat <<EOF
Usage:
  manage-app.sh [--development] install --source /absolute/path/$BRAND_APP_BUNDLE
  manage-app.sh [--development] upgrade --source /absolute/path/$BRAND_APP_BUNDLE --confirm-unregistered
  manage-app.sh [--development] rollback --confirm-unregistered
  manage-app.sh [--development] recover --confirm-unregistered
  manage-app.sh [--development] inventory
  manage-app.sh [--development] finalize
  manage-app.sh [--development] uninstall --confirm-unregistered

Production always manages ~/Applications/$BRAND_APP_BUNDLE and validates candidates with
verify-release.sh. --development is the only test/development mode; it still uses
\$HOME/Applications/$BRAND_APP_BUNDLE and validates with verify-bundle.sh.
Names come from packaging/macos/brand.env (generated from brand.toml).

This command never registers or unregisters the Login Item and never modifies
$BRAND_PRODUCT_NAME configuration (~/Library/Application Support/$BRAND_SLUG), databases,
backups, logs, or other platform data.
EOF
}

[ "$(uname -s)" = Darwin ] || die 'macOS is required'
[ "$(/usr/bin/id -u)" -ne 0 ] || die 'root execution is unsupported'

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
script_path="$script_dir/$(basename -- "$0")"
# shellcheck source=brand.env
. "$script_dir/brand.env"
app_stem=${BRAND_APP_BUNDLE%.app}
agent_executable_rel="Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE/Contents/MacOS/$BRAND_AGENT_BIN"
short_version_env="${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION"
build_version_env="${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION"
# The installed Agent hands the lease over in ${BRAND_ENV_PREFIX}_MAINTENANCE_*; Agents built
# before the rename used MAILD_MAINTENANCE_*. Either way the validator below must prove the lock.
lock_fd_env="${BRAND_ENV_PREFIX}_MAINTENANCE_LOCK_FD"
validator_env="${BRAND_ENV_PREFIX}_MAINTENANCE_VALIDATOR"
test_crash_env="${BRAND_ENV_PREFIX}_MANAGE_TEST_CRASH"
test_pause_env="${BRAND_ENV_PREFIX}_MANAGE_TEST_PAUSE_AFTER_LEASE"
development=false
if [ "${1:-}" = --development ]; then
    development=true
    shift
fi

if [ "$development" = true ]; then
    [ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] ||
        die 'development HOME must be an absolute path'
    managed_home=$HOME
else
    account_home=$(/usr/bin/id -P | /usr/bin/awk -F: 'NF == 10 { print $9 }') ||
        die 'cannot resolve the current uid home directory'
    [ -n "$account_home" ] && [ "${account_home#/}" != "$account_home" ] ||
        die 'current uid home directory is not absolute'
    [ -d "$account_home" ] && [ ! -L "$account_home" ] ||
        die 'current uid home directory is missing or symbolic'
    managed_home=$(CDPATH= cd -- "$account_home" && pwd -P) ||
        die 'cannot canonicalize the current uid home directory'
    [ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] && [ -d "$HOME" ] ||
        die 'ambient HOME does not identify the current uid home directory'
    ambient_home=$(CDPATH= cd -- "$HOME" && pwd -P) ||
        die 'cannot canonicalize ambient HOME'
    [ "$ambient_home" = "$managed_home" ] ||
        die 'ambient HOME differs from the current uid home directory; use --development for a fake HOME'
fi

command=${1:-}
[ -n "$command" ] || { usage >&2; exit 2; }
shift

source_bundle=
confirmed=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --source)
            [ "$#" -ge 2 ] || die '--source requires a path'
            source_bundle=$2
            shift 2
            ;;
        --confirm-unregistered)
            confirmed=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

applications="$managed_home/Applications"
target="$applications/$BRAND_APP_BUNDLE"
rollback="$applications/.$app_stem.rollback.app"
transaction="$applications/.$app_stem.transaction"
staged="$transaction/$BRAND_APP_BUNDLE"
old="$transaction/old.app"
current="$transaction/current.app"
state="$transaction/state"
trash="$managed_home/.Trash/$BRAND_APP_BUNDLE"
rollback_trash="$managed_home/.Trash/$app_stem Rollback.app"
trap 'exit 130' HUP INT TERM

validate_directory_chain() {
    local leaf=$1 current current_uid owner mode_text mode
    current=$leaf
    current_uid=$(id -u)
    while :; do
        [ ! -L "$current" ] || die "directory ancestor must not be a symlink: $current"
        [ -d "$current" ] || die "directory ancestor is missing or not a directory: $current"
        owner=$(stat -f '%u' "$current") || die "cannot inspect directory owner: $current"
        mode_text=$(stat -f '%Lp' "$current") || die "cannot inspect directory mode: $current"
        mode=$((8#$mode_text))
        if [ "$current" = "$leaf" ]; then
            [ "$owner" -eq "$current_uid" ] || die "directory must be owned by current user: $current"
            [ $((mode & 0022)) -eq 0 ] || die "directory must not be group/world writable: $current"
        else
            { [ "$owner" -eq "$current_uid" ] || [ "$owner" -eq 0 ]; } ||
                die "directory ancestor has an unexpected owner: $current"
            if [ $((mode & 0022)) -ne 0 ]; then
                [ "$owner" -eq 0 ] && [ $((mode & 01000)) -ne 0 ] ||
                    die "writable ancestor must be root-owned and sticky: $current"
            fi
        fi
        [ "$current" != / ] || break
        current=$(dirname -- "$current")
    done
}

ensure_applications() {
    validate_directory_chain "$managed_home"
    if [ ! -e "$applications" ]; then
        mkdir -m 700 -- "$applications" || die "cannot create $applications"
    fi
    validate_directory_chain "$applications"
}

plist_value() {
    local plist=$1 key=$2
    plutil -extract "$key" raw -expect string "$plist"
}

architectures() {
    local executable=$1
    lipo -archs "$executable" 2>/dev/null | tr ' ' '\n' | LC_ALL=C sort | paste -sd ' ' -
}

team_identifier() {
    local bundle=$1
    /usr/bin/codesign --display --verbose=4 "$bundle" 2>&1 |
        sed -n 's/^TeamIdentifier=//p' | head -n 1
}

validate_source() {
    local bundle=$1
    [ "${bundle#/}" != "$bundle" ] || die '--source must be absolute'
    [ "$(basename -- "$bundle")" = "$BRAND_APP_BUNDLE" ] || die "--source must end in $BRAND_APP_BUNDLE"
    validate_bundle "$bundle"
}

validate_bundle() {
    local bundle=$1 verify_work verifier short_version build_version version
    [ "${bundle#/}" != "$bundle" ] || die 'bundle path must be absolute'
    [ -d "$bundle" ] && [ ! -L "$bundle" ] || die 'source must be a non-symlink bundle'
    if [ "$(basename -- "$bundle")" != "$BRAND_APP_BUNDLE" ]; then
        verify_work=$(mktemp -d "${TMPDIR:-/tmp}/$BRAND_SLUG-manage-verify.XXXXXX") ||
            die 'cannot create bundle verification directory'
        /usr/bin/ditto "$bundle" "$verify_work/$BRAND_APP_BUNDLE" || {
            rm -rf -- "$verify_work"
            die "cannot copy bundle for verification: $bundle"
        }
        if ! (validate_bundle "$verify_work/$BRAND_APP_BUNDLE"); then
            rm -rf -- "$verify_work"
            die "bundle verification failed: $bundle"
        fi
        rm -rf -- "$verify_work"
        return
    fi
    if [ "$development" = true ]; then
        verifier="$script_dir/verify-bundle.sh"
        [ -x "$verifier" ] || die "required verifier is unavailable: $verifier"
        short_version=$(plist_value "$bundle/Contents/Info.plist" CFBundleShortVersionString) ||
            die 'cannot read candidate short version'
        build_version=$(plist_value "$bundle/Contents/Info.plist" CFBundleVersion) ||
            die 'cannot read candidate build version'
        env "$short_version_env=$short_version" "$build_version_env=$build_version" \
            "$verifier" "$bundle" >/dev/null || die "bundle verification failed: $bundle"
    else
        verifier="$script_dir/verify-release.sh"
        [ -x "$verifier" ] || die "required verifier is unavailable: $verifier"
        version=$(plist_value "$bundle/Contents/Info.plist" CFBundleShortVersionString) ||
            die 'cannot read candidate release version'
        "$verifier" --app "$bundle" --version "$version" --stapled \
            >/dev/null || die "release verification failed: $bundle"
    fi
}

bundle_app_id() {
    local bundle=$1
    plist_value "$bundle/Contents/Info.plist" CFBundleIdentifier
}

bundle_agent_id() {
    local bundle=$1
    plist_value "$bundle/Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE/Contents/Info.plist" \
        CFBundleIdentifier
}

bundle_version() {
    local bundle=$1
    plist_value "$bundle/Contents/Info.plist" CFBundleShortVersionString
}

bundle_architectures() {
    local bundle=$1 main agent
    main=$(architectures "$bundle/Contents/MacOS/$BRAND_LAUNCHER_BIN") || return 1
    agent=$(architectures "$bundle/$agent_executable_rel") || return 1
    printf '%s|%s' "$main" "$agent"
}

version_is_strictly_newer() {
    local old_version=$1 new_version=$2
    printf '%s\n%s\n' "$old_version" "$new_version" |
        awk -F. '
            NR == 1 {
                if (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/ || length($1) > 9 || length($2) > 9 || length($3) > 9) exit 2
                a=$1; b=$2; c=$3; next
            }
            {
                if (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/ || length($1) > 9 || length($2) > 9 || length($3) > 9) exit 2
                exit !(($1 > a) || ($1 == a && $2 > b) || ($1 == a && $2 == b && $3 > c))
            }
        '
}

validate_upgrade_pair() {
    local installed=$1 candidate=$2 installed_version candidate_version
    validate_identity_pair "$installed" "$candidate"
    installed_version=$(bundle_version "$installed")
    candidate_version=$(bundle_version "$candidate")
    version_is_strictly_newer "$installed_version" "$candidate_version" ||
        die "candidate version must be newer than installed version: $installed_version -> $candidate_version"
}

validate_identity_pair() {
    local installed=$1 candidate=$2
    [ "$(bundle_app_id "$installed")" = "$(bundle_app_id "$candidate")" ] ||
        die 'candidate App bundle identifier differs from installed App'
    [ "$(bundle_agent_id "$installed")" = "$(bundle_agent_id "$candidate")" ] ||
        die 'candidate Agent bundle identifier differs from installed Agent'
    [ "$(bundle_architectures "$installed")" = "$(bundle_architectures "$candidate")" ] ||
        die 'candidate architectures differ from installed App'
    if [ "$development" = false ]; then
        [ "$(team_identifier "$installed")" = "$(team_identifier "$candidate")" ] ||
            die 'candidate Team ID differs from installed App'
    fi
}

ensure_trash() {
    if [ ! -e "$managed_home/.Trash" ]; then
        mkdir -m 700 -- "$managed_home/.Trash" || die 'cannot create ~/.Trash'
    fi
    validate_directory_chain "$managed_home/.Trash"
}

copy_candidate() {
    local bundle=$1
    /usr/bin/ditto "$bundle" "$staged" || die 'cannot stage candidate bundle'
    validate_bundle "$staged"
}

begin_transaction() {
    local operation=$1
    [ ! -e "$transaction" ] && [ ! -L "$transaction" ] ||
        die 'unfinished transaction exists; run recover'
    mkdir -m 700 -- "$transaction" || die 'cannot create transaction directory'
    printf '%s\n' "$operation" >"$state" || die 'cannot record transaction state'
}

remove_staged() {
    [ "$1" = "$staged" ] || die 'refusing to remove an unexpected path'
    [ ! -L "$1" ] || die 'refusing to remove a staged symlink'
    /bin/rm -rf -- "$1"
}

finish_transaction() {
    rm -f -- "$state"
    rmdir -- "$transaction" || die 'transaction directory is not empty'
}

test_crash() {
    [ "$development" = true ] || return 0
    [ "${!test_crash_env:-}" = "$1" ] || return 0
    kill -KILL $$
}

lease_agent_path() {
    if [ -x "$target/$agent_executable_rel" ]; then
        printf '%s\n' "$target/$agent_executable_rel"
    elif [ -x "$old/$agent_executable_rel" ]; then
        printf '%s\n' "$old/$agent_executable_rel"
    elif [ -x "$current/$agent_executable_rel" ]; then
        printf '%s\n' "$current/$agent_executable_rel"
    else
        return 1
    fi
}

acquire_maintenance_lease() {
    local agent=${1:-} descriptor validator marker expected_validator
    [ "$confirmed" = true ] || die '--confirm-unregistered is required'
    if [ -z "$agent" ]; then
        agent=$(lease_agent_path) || die 'installed Agent cannot prove maintenance-exec support'
    fi
    [ -x "$agent" ] && [ ! -L "$agent" ] ||
        die 'installed Agent cannot prove maintenance-exec support'
    if [ "$internal_maintenance" = true ]; then
        descriptor=${!lock_fd_env:-${MAILD_MAINTENANCE_LOCK_FD:-}}
        validator=${!validator_env:-${MAILD_MAINTENANCE_VALIDATOR:-}}
        case "$descriptor" in
            ''|*[!0-9]*) die 'inherited maintenance lock descriptor is invalid' ;;
        esac
        [ -n "$validator" ] && [ "${validator#/}" != "$validator" ] &&
            [ -f "$validator" ] && [ ! -L "$validator" ] && [ -x "$validator" ] ||
            die 'inherited maintenance validator is invalid'
        expected_validator=$(CDPATH= cd -- "$(dirname -- "$agent")" && pwd -P)/$(basename -- "$agent")
        validator=$(CDPATH= cd -- "$(dirname -- "$validator")" && pwd -P)/$(basename -- "$validator")
        [ "$validator" = "$expected_validator" ] ||
            die 'inherited maintenance validator differs from the validated Agent'
        HOME="$managed_home" "$validator" maintenance-validate-fd --fd "$descriptor" \
            >/dev/null 2>&1 || die 'inherited maintenance lease is not held'
        if [ "$development" = true ] &&
            [ "${!test_pause_env:-}" = 1 ]; then
            marker="$managed_home/maintenance-lease-held"
            [ ! -e "$marker" ] && [ ! -L "$marker" ] ||
                die 'maintenance test marker already exists'
            : >"$marker"
            sleep 1
            rm -f -- "$marker"
        fi
        return
    fi
    exec env HOME="$managed_home" "$agent" maintenance-exec \
        --manager "$script_path" -- "${original_arguments[@]}"
    die 'Agent is running, unsafe, or too old to execute maintenance'
}

recover_transaction() {
    local operation
    [ -d "$transaction" ] && [ ! -L "$transaction" ] || die 'no recoverable transaction exists'
    operation=$(cat -- "$state" 2>/dev/null) || die 'transaction state is missing'
    case "$operation" in
        install)
            if [ ! -e "$target" ] && [ ! -L "$target" ] && [ -e "$staged" ] && [ ! -L "$staged" ]; then
                if [ -d "$staged" ] && (validate_bundle "$staged"); then
                    mv -- "$staged" "$target" || die 'cannot complete interrupted install'
                    validate_bundle "$target"
                else
                    remove_staged "$staged"
                fi
            elif [ ! -e "$target" ] && [ ! -e "$staged" ]; then
                :
            elif [ -d "$target" ] && [ ! -e "$staged" ]; then
                validate_bundle "$target"
            else
                die 'ambiguous install transaction; no files changed'
            fi
            ;;
        upgrade)
            if [ -d "$target" ] && [ -e "$staged" ] && [ ! -L "$staged" ] && [ ! -e "$old" ]; then
                validate_bundle "$target"
                remove_staged "$staged"
            elif [ -d "$target" ] && [ ! -e "$staged" ] && [ ! -e "$old" ]; then
                validate_bundle "$target"
            elif [ ! -e "$target" ] && [ -d "$staged" ] && [ -d "$old" ]; then
                validate_bundle "$staged"
                validate_bundle "$old"
                mv -- "$old" "$target" || die 'cannot restore old bundle'
                validate_bundle "$target"
                remove_staged "$staged"
            elif [ -d "$target" ] && [ ! -e "$staged" ] && [ -d "$old" ] && [ ! -e "$rollback" ]; then
                validate_bundle "$target"
                validate_bundle "$old"
                validate_identity_pair "$old" "$target"
                mv -- "$old" "$rollback" || die 'cannot preserve rollback bundle'
                validate_bundle "$rollback"
                validate_identity_pair "$rollback" "$target"
            elif [ -d "$target" ] && [ -d "$rollback" ] && [ ! -e "$staged" ] && [ ! -e "$old" ]; then
                validate_bundle "$target"
                validate_bundle "$rollback"
                validate_identity_pair "$rollback" "$target"
            else
                die 'ambiguous upgrade transaction; no files changed'
            fi
            ;;
        rollback)
            if [ ! -e "$target" ] && [ -d "$current" ] && [ -d "$rollback" ]; then
                validate_bundle "$current"
                validate_bundle "$rollback"
                mv -- "$current" "$target" || die 'cannot restore current bundle'
                validate_bundle "$target"
            elif [ -d "$target" ] && [ -d "$current" ] && [ ! -e "$rollback" ]; then
                validate_bundle "$target"
                validate_bundle "$current"
                validate_identity_pair "$current" "$target"
                mv -- "$current" "$rollback" || die 'cannot preserve replaced bundle'
                validate_bundle "$rollback"
                validate_identity_pair "$rollback" "$target"
            elif [ -d "$target" ] && [ -d "$rollback" ] && [ ! -e "$current" ]; then
                validate_bundle "$target"
                validate_bundle "$rollback"
                validate_identity_pair "$rollback" "$target"
            else
                die 'ambiguous rollback transaction; no files changed'
            fi
            ;;
        *) die 'unknown transaction state' ;;
    esac
    finish_transaction
}

case "$command" in
    install)
        [ -n "$source_bundle" ] || die 'install requires --source'
        [ "$confirmed" = false ] || die 'install does not accept --confirm-unregistered'
        ensure_applications
        [ ! -e "$target" ] && [ ! -L "$target" ] || die "$BRAND_APP_BUNDLE is already installed"
        [ ! -e "$rollback" ] && [ ! -L "$rollback" ] || die 'rollback bundle already exists'
        validate_source "$source_bundle"
        begin_transaction install
        copy_candidate "$source_bundle"
        mv -- "$staged" "$target" || die "cannot publish $BRAND_APP_BUNDLE"
        validate_bundle "$target" || die "published $BRAND_APP_BUNDLE failed verification"
        finish_transaction
        ;;
    upgrade)
        [ -n "$source_bundle" ] || die 'upgrade requires --source'
        ensure_applications
        [ -d "$target" ] && [ ! -L "$target" ] || die "$BRAND_APP_BUNDLE is not installed"
        [ ! -e "$rollback" ] && [ ! -L "$rollback" ] ||
            die 'rollback already exists; run rollback or finalize first'
        validate_bundle "$target"
        acquire_maintenance_lease
        validate_source "$source_bundle"
        validate_upgrade_pair "$target" "$source_bundle"
        begin_transaction upgrade
        copy_candidate "$source_bundle"
        mv -- "$target" "$old" || die 'cannot stage installed bundle'
        test_crash upgrade-old-moved
        mv -- "$staged" "$target" || {
            mv -- "$old" "$target" || true
            die 'cannot publish candidate; old bundle recovery attempted'
        }
        if ! (validate_bundle "$target"); then
            mv -- "$target" "$staged" || true
            mv -- "$old" "$target" || true
            die 'published candidate failed verification; old bundle recovery attempted'
        fi
        mv -- "$old" "$rollback" || die 'candidate published; run recover to preserve rollback'
        validate_bundle "$rollback"
        validate_identity_pair "$rollback" "$target"
        finish_transaction
        ;;
    rollback)
        ensure_applications
        [ -d "$target" ] && [ ! -L "$target" ] || die "$BRAND_APP_BUNDLE is not installed"
        [ -d "$rollback" ] && [ ! -L "$rollback" ] || die 'no rollback bundle exists'
        validate_bundle "$target"
        validate_bundle "$rollback"
        validate_identity_pair "$target" "$rollback"
        acquire_maintenance_lease
        begin_transaction rollback
        mv -- "$target" "$current" || die 'cannot stage current bundle'
        mv -- "$rollback" "$target" || {
            mv -- "$current" "$target" || true
            die 'cannot publish rollback; current bundle recovery attempted'
        }
        if ! (validate_bundle "$target"); then
            mv -- "$target" "$rollback" || true
            mv -- "$current" "$target" || true
            die 'published rollback failed verification; current bundle recovery attempted'
        fi
        mv -- "$current" "$rollback" || die 'rollback published; run recover to preserve replaced bundle'
        validate_bundle "$rollback"
        validate_identity_pair "$rollback" "$target"
        finish_transaction
        ;;
    recover)
        ensure_applications
        [ -f "$state" ] && [ ! -L "$state" ] || die 'transaction state is missing'
        recovery_operation=$(cat -- "$state")
        [ "$confirmed" = true ] || die '--confirm-unregistered is required'
        case "$recovery_operation" in
            install)
                if [ -d "$target" ] && [ ! -L "$target" ]; then
                    validate_bundle "$target"
                    acquire_maintenance_lease
                elif [ -d "$staged" ] && [ ! -L "$staged" ] &&
                    (validate_bundle "$staged"); then
                    acquire_maintenance_lease \
                        "$staged/$agent_executable_rel"
                fi
                ;;
            upgrade|rollback)
                acquire_maintenance_lease
                ;;
            *) die 'unknown transaction state' ;;
        esac
        recover_transaction
        ;;
    finalize)
        ensure_applications
        [ ! -e "$transaction" ] && [ ! -L "$transaction" ] ||
            die 'unfinished transaction exists; run recover'
        [ -d "$rollback" ] && [ ! -L "$rollback" ] || die 'no rollback bundle exists'
        validate_bundle "$rollback"
        ensure_trash
        [ ! -e "$rollback_trash" ] && [ ! -L "$rollback_trash" ] ||
            die "~/.Trash/$app_stem Rollback.app already exists"
        mv -- "$rollback" "$rollback_trash" || die 'cannot move rollback bundle to Trash'
        ;;
    uninstall)
        ensure_applications
        [ -d "$target" ] && [ ! -L "$target" ] || die "$BRAND_APP_BUNDLE is not installed"
        [ ! -e "$transaction" ] && [ ! -L "$transaction" ] ||
            die 'unfinished transaction exists; run recover'
        [ ! -e "$rollback" ] && [ ! -L "$rollback" ] ||
            die 'rollback exists; run finalize before uninstall'
        validate_bundle "$target"
        acquire_maintenance_lease
        ensure_trash
        [ ! -e "$trash" ] && [ ! -L "$trash" ] || die "~/.Trash/$BRAND_APP_BUNDLE already exists"
        mv -- "$target" "$trash" || die "cannot move $BRAND_APP_BUNDLE to Trash"
        ;;
    inventory)
        [ -z "$source_bundle" ] && [ "$confirmed" = false ] ||
            die 'inventory does not accept mutation options'
        validate_directory_chain "$managed_home"
        if [ ! -e "$applications" ]; then
            printf 'installed=none\nrollback=none\ntransaction=none\n'
            exit 0
        fi
        validate_directory_chain "$applications"
        if [ -d "$target" ] && [ ! -L "$target" ]; then
            validate_bundle "$target"
            printf 'installed=%s\n' "$(bundle_version "$target")"
        else
            printf 'installed=none\n'
        fi
        if [ -d "$rollback" ] && [ ! -L "$rollback" ]; then
            validate_bundle "$rollback"
            printf 'rollback=%s\n' "$(bundle_version "$rollback")"
        else
            printf 'rollback=none\n'
        fi
        if [ -d "$transaction" ] && [ ! -L "$transaction" ]; then
            transaction_state=$(cat -- "$state" 2>/dev/null || printf unknown)
            transaction_state=$(printf '%s' "$transaction_state" | LC_ALL=C tr '\000-\037\177' ' ')
            printf 'transaction=%s\n' "$transaction_state"
        else
            printf 'transaction=none\n'
        fi
        exit 0
        ;;
    -h|--help)
        usage
        ;;
    *) usage >&2; exit 2 ;;
esac

printf 'manage-app: %s complete\n' "$command"
