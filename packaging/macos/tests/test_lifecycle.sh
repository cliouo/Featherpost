#!/bin/bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    printf 'test_lifecycle: SKIP (macOS required)\n'
    exit 0
fi
command -v cc >/dev/null 2>&1 || {
    printf 'test_lifecycle: C compiler is required\n' >&2
    exit 1
}

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
packaging_dir=$(CDPATH= cd -- "$test_dir/.." && pwd -P)
# shellcheck source=../brand.env
. "$packaging_dir/brand.env"
work=$(mktemp -d "$packaging_dir/.$BRAND_SLUG-lifecycle-test.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

fail() {
    printf 'test_lifecycle: %s\n' "$*" >&2
    exit 1
}

agent_bundle_id=$BRAND_AGENT_BUNDLE_ID
menu_bundle_id=$BRAND_MENU_BUNDLE_ID
app_stem=${BRAND_APP_BUNDLE%.app}
agent_rel="Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE/Contents/MacOS/$BRAND_AGENT_BIN"
launcher_rel="Contents/MacOS/$BRAND_LAUNCHER_BIN"
host_ui_rel="Contents/MacOS/$BRAND_UI_BIN"
icon_rel="Contents/Resources/$BRAND_ICON_NAME.icns"
agent_bundle_rel="Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
lock_fd_env="${BRAND_ENV_PREFIX}_MAINTENANCE_LOCK_FD"
validator_env="${BRAND_ENV_PREFIX}_MAINTENANCE_VALIDATOR"
mkdir -p -- "$work/bin" "$work/sources" "$work/home/.Trash"
chmod 0700 "$work/home" "$work/home/.Trash"

build_fixture() {
    marker=$1
    version=$2
    output=$3
    launcher="$work/bin/$BRAND_LAUNCHER_BIN-$marker"
    ui="$work/bin/$BRAND_UI_BIN-$marker"
    agent="$work/bin/$BRAND_AGENT_BIN-$marker"
    menu="$work/bin/$BRAND_MENU_BIN-$marker"
    printf '%s\n' \
        '#include <stdio.h>' \
        "__attribute__((used)) static const char identity[] = \"maild-agent-bundle-id:$agent_bundle_id\";" \
        "int main(void) { puts(\"$marker\"); return identity[0] == 0; }" |
        cc -x c - -o "$launcher"
    printf '%s\n' \
        "__attribute__((used)) static const char ui_embedded[] = \"maild-ui-embedded:$BRAND_UI_BUNDLE_ID\";" \
        'int main(void) { return ui_embedded[0] == 0; }' | cc -x c - -o "$ui"
    printf '%s\n' \
        '#include <fcntl.h>' \
        '#include <stdlib.h>' \
        '#include <stdio.h>' \
        '#include <string.h>' \
        '#include <sys/file.h>' \
        '#include <unistd.h>' \
        "__attribute__((used)) static const char identity[] = \"maild-agent-bundle-id:$agent_bundle_id\";" \
        'static int home_path(char *path, size_t size, const char *leaf) {' \
        '  const char *home = getenv("HOME");' \
        '  return home && snprintf(path, size, "%s/%s", home, leaf) > 0 ? 0 : 1;' \
        '}' \
        'static int open_lifecycle_lock(void) {' \
        '  char path[4096];' \
        '  if (home_path(path, sizeof(path), "agent-lifecycle.lock")) return -1;' \
        '  return open(path, O_RDWR | O_CREAT, 0600);' \
        '}' \
        'int main(int argc, char **argv) {' \
        '  if (argc == 2 && strcmp(argv[1], "maintenance-probe") == 0) {' \
        '    char busy[4096];' \
        '    if (home_path(busy, sizeof(busy), "agent-busy")) return 1;' \
        '    if (access(busy, F_OK) == 0) return 3;' \
        '    int fd = open_lifecycle_lock();' \
        '    if (fd < 0) return 1;' \
        '    int result = flock(fd, LOCK_EX | LOCK_NB);' \
        '    if (result == 0) flock(fd, LOCK_UN);' \
        '    close(fd);' \
        '    return result == 0 ? 0 : 3;' \
        '  }' \
        '  if (argc == 4 && strcmp(argv[1], "maintenance-validate-fd") == 0 &&' \
        '      strcmp(argv[2], "--fd") == 0) {' \
        '    int fd = atoi(argv[3]);' \
        '    return fd >= 0 && flock(fd, LOCK_EX | LOCK_NB) == 0 ? 0 : 1;' \
        '  }' \
        '  if (argc >= 6 && strcmp(argv[1], "maintenance-exec") == 0 &&' \
        '      strcmp(argv[2], "--manager") == 0 && strcmp(argv[4], "--") == 0) {' \
        '    char busy[4096], descriptor[32];' \
        '    if (home_path(busy, sizeof(busy), "agent-busy")) return 1;' \
        '    if (access(busy, F_OK) == 0) return 3;' \
        '    int fd = open_lifecycle_lock();' \
        '    if (fd < 0 || flock(fd, LOCK_EX | LOCK_NB) != 0) return 3;' \
        '    int flags = fcntl(fd, F_GETFD);' \
        '    if (flags < 0 || fcntl(fd, F_SETFD, flags & ~FD_CLOEXEC) < 0) return 1;' \
        '    snprintf(descriptor, sizeof(descriptor), "%d", fd);' \
        "    if (setenv(\"$lock_fd_env\", descriptor, 1) != 0 ||" \
        "        setenv(\"$validator_env\", argv[0], 1) != 0) return 1;" \
        '    char **next = calloc((size_t)argc, sizeof(char *));' \
        '    if (!next) return 1;' \
        '    next[0] = argv[3];' \
        '    next[1] = "--internal-maintenance";' \
        '    for (int index = 5; index < argc; ++index) next[index - 3] = argv[index];' \
        '    execv(argv[3], next);' \
        '    return 1;' \
        '  }' \
        '  char started[4096];' \
        '  if (home_path(started, sizeof(started), "agent-started")) return 1;' \
        '  int lock_fd = open_lifecycle_lock();' \
        '  if (lock_fd < 0 || flock(lock_fd, LOCK_EX | LOCK_NB) != 0) return 0;' \
        '  int fd = open(started, O_WRONLY | O_CREAT | O_EXCL, 0600);' \
        '  if (fd >= 0) close(fd);' \
        '  sleep(2);' \
        '  flock(lock_fd, LOCK_UN);' \
        '  close(lock_fd);' \
        '  return identity[0] == 0;' \
        '}' |
        cc -x c - -o "$agent"
    printf '%s\n' \
        '#include <stdio.h>' \
        "__attribute__((used)) static const char identity[] = \"maild-menu-bundle-id:$menu_bundle_id\";" \
        "int main(void) { puts(\"$marker\"); return identity[0] == 0; }" |
        cc -x c - -o "$menu"
    chmod 0755 "$launcher" "$agent" "$menu" "$ui"
    env "${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=$version" \
        "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=$version" \
        "$packaging_dir/build-bundle.sh" \
        --launcher-bin "$launcher" \
        --agent-bin "$agent" \
        --menu-bin "$menu" \
        --ui-bin "$ui" \
        --output "$output" >/dev/null
}

v1="$work/sources/v1/$BRAND_APP_BUNDLE"
v2="$work/sources/v2/$BRAND_APP_BUNDLE"
build_fixture v1 1.0.0 "$v1"
build_fixture v2 2.0.0 "$v2"

remove_agent_icon() {
    local bundle=$1
    rm -- "$bundle/$agent_bundle_rel/$icon_rel"
    rmdir -- "$bundle/$agent_bundle_rel/Contents/Resources"
    plutil -remove CFBundleIconFile "$bundle/$agent_bundle_rel/Contents/Info.plist"
}

manage() {
    HOME="$work/home" "$packaging_dir/manage-app.sh" --development "$@"
}

if HOME="$work/home" "$packaging_dir/manage-app.sh" inventory >/dev/null 2>&1; then
    fail 'production mode trusted a fake ambient HOME'
fi

target="$work/home/Applications/$BRAND_APP_BUNDLE"
# A source missing the single-host Contents/MacOS/<ui_bin> is not installable.
no_host_ui="$work/sources/no-host-ui/$BRAND_APP_BUNDLE"
mkdir -p -- "$(dirname -- "$no_host_ui")"
cp -R -- "$v1" "$no_host_ui"
rm -- "$no_host_ui/$host_ui_rel"
if manage install --source "$no_host_ui" >/dev/null 2>&1; then
    fail "install accepted a bundle without $host_ui_rel"
fi
[ ! -e "$target" ] || fail 'rejected install published a target'

manage install --source "$v1" >/dev/null
cmp -s "$v1/$host_ui_rel" "$target/$host_ui_rel" ||
    fail 'install did not publish the single-host UI executable'
cmp -s "$v1/$icon_rel" "$target/$icon_rel" ||
    fail 'install did not publish the application icon'
remove_agent_icon "$target"
rollback="$work/home/Applications/.$app_stem.rollback.app"
transaction="$work/home/Applications/.$app_stem.transaction"
[ "$("$target/$launcher_rel")" = v1 ] || fail 'install published wrong bundle'
mkdir -- "$work/hostile-bin"
ln -s /usr/bin/false "$work/hostile-bin/stat"
PATH="$work/hostile-bin:$PATH" manage inventory >/dev/null ||
    fail 'manager trusted a hostile ambient PATH'
if manage install --source "$v2" >/dev/null 2>&1; then
    fail 'install clobbered an existing target'
fi

data="$work/home/Library/Application Support/$BRAND_SLUG"
mkdir -p -- "$data/backups"
printf 'keep-config\n' >"$data/$BRAND_SLUG.toml"
printf 'keep-database\n' >"$data/$BRAND_SLUG.sqlite3"
printf 'keep-backup\n' >"$data/backups/keep.sqlite3"

if HOME="$work/home" env "$lock_fd_env=0" "$validator_env=/usr/bin/true" \
    "$packaging_dir/manage-app.sh" --internal-maintenance --development upgrade \
        --source "$v2" --confirm-unregistered >/dev/null 2>&1; then
    fail 'hidden maintenance entry accepted a forged validator'
fi
if HOME="$work/home" MAILD_MAINTENANCE_LOCK_FD=0 \
    MAILD_MAINTENANCE_VALIDATOR=/usr/bin/true \
    "$packaging_dir/manage-app.sh" --internal-maintenance --development upgrade \
        --source "$v2" --confirm-unregistered >/dev/null 2>&1; then
    fail 'hidden maintenance entry accepted a forged legacy validator'
fi
[ "$("$target/$launcher_rel")" = v1 ] ||
    fail 'forged hidden maintenance entry changed the installed bundle'

if manage upgrade --source "$v2" >/dev/null 2>&1; then
    fail 'upgrade did not require explicit unregistration confirmation'
fi
touch "$work/home/agent-busy"
if manage upgrade --source "$v2" --confirm-unregistered >/dev/null 2>&1; then
    fail 'upgrade ignored a busy Agent probe'
fi
rm -- "$work/home/agent-busy"
pre_icon_candidate="$work/sources/pre-icon-candidate/$BRAND_APP_BUNDLE"
mkdir -p -- "$(dirname -- "$pre_icon_candidate")"
cp -R -- "$v2" "$pre_icon_candidate"
remove_agent_icon "$pre_icon_candidate"
if manage upgrade --source "$pre_icon_candidate" --confirm-unregistered >/dev/null 2>&1; then
    fail 'upgrade accepted a candidate without an Agent icon'
fi
[ "$("$target/$launcher_rel")" = v1 ] && [ ! -e "$transaction" ] ||
    fail 'rejected upgrade changed the pre-icon install'
HOME="$work/home" env "${BRAND_ENV_PREFIX}_MANAGE_TEST_PAUSE_AFTER_LEASE=1" \
    "$packaging_dir/manage-app.sh" --development upgrade --source "$v2" \
        --confirm-unregistered >/dev/null &
manage_pid=$!
attempt=0
while [ ! -e "$work/home/maintenance-lease-held" ] && [ "$attempt" -lt 200 ]; do
    attempt=$((attempt + 1))
    sleep 0.01
done
[ -e "$work/home/maintenance-lease-held" ] || fail 'upgrade did not hold a maintenance lease'
HOME="$work/home" "$target/$agent_rel" >/dev/null 2>&1 &
hostile_agent_pid=$!
wait "$manage_pid"
wait "$hostile_agent_pid"
[ ! -e "$work/home/agent-started" ] ||
    fail 'Agent started after maintenance check while upgrade still published'
[ "$("$target/$launcher_rel")" = v2 ] || fail 'upgrade published wrong bundle'
[ -x "$target/$host_ui_rel" ] && [ -f "$target/$icon_rel" ] ||
    fail 'upgrade lost the single-host UI or application icon'
cmp -s "$target/$icon_rel" "$target/$agent_bundle_rel/$icon_rel" ||
    fail 'upgrade did not publish the Agent icon'
[ "$("$rollback/$launcher_rel")" = v1 ] || fail 'upgrade lost old bundle'
"$target/$agent_rel" maintenance-probe || fail 'maintenance lock remained held after transaction exit'
if manage upgrade --source "$v1" --confirm-unregistered >/dev/null 2>&1; then
    fail 'upgrade overwrote the only rollback'
fi

manage rollback --confirm-unregistered >/dev/null
[ "$("$target/$launcher_rel")" = v1 ] || fail 'rollback did not restore v1'
[ "$("$rollback/$launcher_rel")" = v2 ] || fail 'rollback did not retain redo bundle'
manage finalize >/dev/null
[ ! -e "$rollback" ] || fail 'finalize retained rollback bundle'
[ -d "$work/home/.Trash/$app_stem Rollback.app" ] || fail 'finalize did not use Trash'
rm -rf -- "$work/home/.Trash/$app_stem Rollback.app"

set +e
HOME="$work/home" env "${BRAND_ENV_PREFIX}_MANAGE_TEST_CRASH=upgrade-old-moved" \
    "$packaging_dir/manage-app.sh" --development upgrade --source "$v2" \
        --confirm-unregistered >/dev/null 2>&1
crash_status=$?
set -e
[ "$crash_status" -ne 0 ] || fail 'crash seam did not interrupt upgrade'
[ -d "$transaction" ] || fail 'crash did not retain recoverable transaction'
[ ! -e "$target" ] || fail 'crash seam occurred at the wrong phase'
attempt=0
while [ -e "$work/home/maintenance-lease-held" ] && [ "$attempt" -lt 200 ]; do
    attempt=$((attempt + 1))
    sleep 0.01
done
manage recover --confirm-unregistered >/dev/null
[ "$("$target/$launcher_rel")" = v1 ] || fail 'recover did not restore old bundle'
[ ! -e "$transaction" ] || fail 'recover retained transaction residue'

mkdir -m 700 -- "$transaction"
printf 'upgrade\n' >"$transaction/state"
manage recover --confirm-unregistered >/dev/null
[ ! -e "$transaction" ] || fail 'recover retained pre-stage upgrade transaction'

mkdir -m 700 -p -- "$transaction/$BRAND_APP_BUNDLE"
printf 'install\n' >"$transaction/state"
printf 'partial\n' >"$transaction/$BRAND_APP_BUNDLE/partial"
mv -- "$target" "$work/saved-target"
manage recover --confirm-unregistered >/dev/null 2>&1
[ ! -e "$transaction" ] || fail 'recover retained partial install transaction'
[ ! -e "$target" ] || fail 'partial install recovery published invalid staging'
mv -- "$work/saved-target" "$target"

manage upgrade --source "$v2" --confirm-unregistered >/dev/null
manage finalize >/dev/null
rm -rf -- "$work/home/.Trash/$app_stem Rollback.app"
manage uninstall --confirm-unregistered >/dev/null
[ ! -e "$target" ] || fail 'uninstall retained installed target'
[ "$("$work/home/.Trash/$BRAND_APP_BUNDLE/$launcher_rel")" = v2 ] ||
    fail 'uninstall did not move bundle to Trash'
[ "$(cat "$data/$BRAND_SLUG.toml")" = keep-config ] || fail 'uninstall changed config'
[ "$(cat "$data/$BRAND_SLUG.sqlite3")" = keep-database ] || fail 'uninstall changed database'
[ "$(cat "$data/backups/keep.sqlite3")" = keep-backup ] || fail 'uninstall changed backup'

if grep -Eq 'SMAppService|launchctl|sfltool' "$packaging_dir/manage-app.sh"; then
    fail 'manager contains forbidden background-registration calls'
fi

printf 'test_lifecycle: PASS\n'
