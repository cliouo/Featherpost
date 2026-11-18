#!/bin/bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    printf 'test_bundle: SKIP (macOS required)\n'
    exit 0
fi
command -v cc >/dev/null 2>&1 || {
    printf 'test_bundle: C compiler is required for the fixture\n' >&2
    exit 1
}

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
packaging_dir=$(CDPATH= cd -- "$test_dir/.." && pwd -P)
# shellcheck source=../brand.env
. "$packaging_dir/brand.env"
work=$(mktemp -d "${TMPDIR:-/tmp}/$BRAND_SLUG-bundle-test.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

fail() {
    printf 'test_bundle: %s\n' "$*" >&2
    exit 1
}

# C fixture carrying one compiled identity contract (maild-brand *_contract! markers).
# Usage: fixture OUTPUT MARKER [cc options...]; an empty MARKER builds an unmarked binary.
fixture() {
    local output=$1
    local marker=$2
    shift 2
    if [ -n "$marker" ]; then
        printf '%s\n' \
            "__attribute__((used)) static const char identity[] = \"$marker\";" \
            'int main(void) { return identity[0] == 0; }'
    else
        printf '%s\n' 'int main(void) { return 0; }'
    fi | cc "$@" -x c - -o "$output"
    chmod 0755 "$output"
}

mkdir -p -- "$work/bin" "$work/home" "$work/output"
agent_bundle_id=$BRAND_AGENT_BUNDLE_ID
menu_bundle_id=$BRAND_MENU_BUNDLE_ID
agent_marker="maild-agent-bundle-id:$agent_bundle_id"
menu_marker="maild-menu-bundle-id:$menu_bundle_id"
ui_marker="maild-ui-embedded:$BRAND_UI_BUNDLE_ID"
launcher_name=$BRAND_LAUNCHER_BIN
agent_name=$BRAND_AGENT_BIN
menu_name=$BRAND_MENU_BIN
ui_name=$BRAND_UI_BIN
agent_rel="Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu_rel="Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
payload_rel="Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID"
fixture "$work/bin/$launcher_name" "$agent_marker"
fixture "$work/bin/$agent_name" "$agent_marker"
fixture "$work/bin/$menu_name" "$menu_marker"
fixture "$work/bin/$ui_name" "$ui_marker"
fixture "$work/bin/$ui_name-dev-form" ''
xattr -w com.example.fixture launcher "$work/bin/$launcher_name"

export HOME="$work/home"
short_version_env="${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION"
build_version_env="${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION"
unset_versions=(env -u "$short_version_env" -u "$build_version_env"
    -u MAILD_BUNDLE_SHORT_VERSION -u MAILD_BUNDLE_BUILD_VERSION
    -u "${BRAND_ENV_PREFIX}_APP_BUNDLE_ID" -u "${BRAND_ENV_PREFIX}_AGENT_BUNDLE_ID"
    -u MAILD_APP_BUNDLE_ID -u MAILD_AGENT_BUNDLE_ID)
default_bundle="$work/output/default/$BRAND_APP_BUNDLE"
"${unset_versions[@]}" \
    "$packaging_dir/build-bundle.sh" \
        --launcher-bin "$work/bin/$launcher_name" \
        --agent-bin "$work/bin/$agent_name" \
        --output "$default_bundle" >/dev/null
"${unset_versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$default_bundle" >/dev/null
for pair in "app-bundle-id.txt:Contents/Info.plist:$BRAND_APP_BUNDLE_ID" \
    "agent-bundle-id.txt:$agent_rel/Contents/Info.plist:$BRAND_AGENT_BUNDLE_ID" \
    "menu-bundle-id.txt:$menu_rel/Contents/Info.plist:$BRAND_MENU_BUNDLE_ID"; do
    manifest=${pair%%:*}
    rest=${pair#*:}
    expected=${rest#*:}
    [ "$(cat -- "$packaging_dir/$manifest")" = "$expected" ] ||
        fail "$manifest drifted from brand.env"
    [ "$(plutil -extract CFBundleIdentifier raw "$default_bundle/${rest%%:*}")" = "$expected" ] ||
        fail "default bundle identifier drifted: ${rest%%:*}"
done
[ "$(plutil -extract CFBundleIdentifier raw \
    "$default_bundle/$payload_rel/$BRAND_UI_BUNDLE/Contents/Info.plist")" = "$BRAND_UI_BUNDLE_ID" ] ||
    fail 'default UI bundle identifier drifted'
[ "$(plutil -extract CFBundleShortVersionString raw "$default_bundle/Contents/Info.plist")" = 0.1.0 ] ||
    fail 'default short version is not 0.1.0'

# Legacy MAILD_BUNDLE_* is still honoured as a fallback; the branded name wins when both are set.
legacy_bundle="$work/output/legacy/$BRAND_APP_BUNDLE"
"${unset_versions[@]}" MAILD_BUNDLE_SHORT_VERSION=3.4.5 MAILD_BUNDLE_BUILD_VERSION=7 \
    "$packaging_dir/build-bundle.sh" \
        --launcher-bin "$work/bin/$launcher_name" \
        --agent-bin "$work/bin/$agent_name" \
        --output "$legacy_bundle" >/dev/null
[ "$(plutil -extract CFBundleShortVersionString raw "$legacy_bundle/Contents/Info.plist")" = 3.4.5 ] ||
    fail 'legacy MAILD_BUNDLE_SHORT_VERSION fallback was ignored'
"${unset_versions[@]}" "$short_version_env=3.4.5" "$build_version_env=7" \
    MAILD_BUNDLE_SHORT_VERSION=9.9.9 MAILD_BUNDLE_BUILD_VERSION=99 \
    "$packaging_dir/verify-bundle.sh" "$legacy_bundle" >/dev/null ||
    fail 'branded version variables did not take precedence over legacy ones'

export "$short_version_env=2.3.4"
export "$build_version_env=42"
build_version_value=42

bundle="$work/output/$BRAND_APP_BUNDLE"
"$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name" \
    --output "$bundle" >/dev/null
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

cmp -s "$work/bin/$launcher_name" "$bundle/Contents/MacOS/$launcher_name" ||
    fail 'launcher executable was not copied byte-for-byte'
cmp -s "$work/bin/$agent_name" "$bundle/$agent_rel/Contents/MacOS/$agent_name" ||
    fail 'agent executable was not copied byte-for-byte'
cmp -s "$work/bin/$menu_name" "$bundle/$menu_rel/Contents/MacOS/$menu_name" ||
    fail 'menu executable was not copied byte-for-byte'
if xattr -p com.example.fixture "$bundle/Contents/MacOS/$launcher_name" >/dev/null 2>&1; then
    fail 'builder copied an input extended attribute into the bundle'
fi
payload="$bundle/$payload_rel"
ui_executable="$payload/$BRAND_UI_BUNDLE/Contents/MacOS/$ui_name"
cmp -s "$work/bin/$ui_name" "$ui_executable" ||
    fail 'UI executable was not copied byte-for-byte'
[ "$(cat -- "$payload/recovery.sha256")" = "$(shasum -a 256 "$work/bin/$ui_name" | awk '{print $1}')" ] ||
    fail 'recovery checksum does not describe the bundled UI executable'
[ "$(wc -c <"$payload/recovery.sha256" | tr -d ' ')" = 65 ] || fail 'recovery checksum is not 65 bytes'
grep -Fqx 'version = "2.3.4"' "$payload/frontend.toml" || fail 'bundled manifest version drifted'
grep -Fqx 'lifecycle = "on_demand"' "$payload/frontend.toml" || fail 'bundled manifest is not on_demand'
grep -Fqx "id = \"$BRAND_UI_BUNDLE_ID\"" "$payload/frontend.toml" || fail 'bundled manifest id drifted'
grep -Fqx "name = \"$BRAND_PRODUCT_NAME\"" "$payload/frontend.toml" || fail 'bundled manifest name drifted'
grep -Fqx "path = \"$BRAND_UI_BUNDLE\"" "$payload/frontend.toml" || fail 'bundled manifest launch path drifted'
! grep -Fq '__' "$payload/frontend.toml" || fail 'bundled manifest kept a template placeholder'
[ "$(plutil -extract CFBundleShortVersionString raw "$payload/$BRAND_UI_BUNDLE/Contents/Info.plist")" = 2.3.4 ] ||
    fail 'bundled UI plist version drifted'
[ "$(plutil -extract CFBundleExecutable raw "$bundle/Contents/Info.plist")" = "$launcher_name" ] ||
    fail 'top-level executable is not the native launcher'
[ "$(plutil -extract CFBundleDisplayName raw "$bundle/Contents/Info.plist")" = "$BRAND_PRODUCT_NAME" ] ||
    fail 'top-level display name is not the brand product name'
host_ui="$bundle/Contents/MacOS/$ui_name"
app_icon="$bundle/Contents/Resources/$BRAND_ICON_NAME.icns"
cmp -s "$work/bin/$ui_name" "$host_ui" ||
    fail 'single-host UI executable was not copied byte-for-byte'
[ "$(stat -f '%Lp' "$host_ui")" = 755 ] || fail 'single-host UI executable is not 0755'
cmp -s "$packaging_dir/assets/AppIcon.icns" "$app_icon" ||
    fail 'application icon was not copied byte-for-byte'
[ "$(stat -f '%Lp' "$app_icon")" = 644 ] || fail 'application icon is not 0644'
[ "$(plutil -extract CFBundleIconFile raw "$bundle/Contents/Info.plist")" = "$BRAND_ICON_NAME" ] ||
    fail "top-level CFBundleIconFile is not $BRAND_ICON_NAME"

[ -z "$(find "$work/home" -mindepth 1 -print -quit)" ] ||
    fail 'bundle scripts wrote under HOME'

printf 'keep-me\n' >"$bundle/sentinel"
sentinel_before=$(shasum -a 256 "$bundle/sentinel")
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name" \
    --output "$bundle" >/dev/null 2>&1; then
    fail 'builder overwrote an existing destination'
fi
sentinel_after=$(shasum -a 256 "$bundle/sentinel")
[ "$sentinel_before" = "$sentinel_after" ] || fail 'existing destination bytes changed'
rm -- "$bundle/sentinel"

for override in "${BRAND_ENV_PREFIX}_AGENT_BUNDLE_ID" MAILD_AGENT_BUNDLE_ID "${BRAND_ENV_PREFIX}_UI_BUNDLE_ID"; do
    if env "$override=com.example.override" \
        "$packaging_dir/build-bundle.sh" \
            --launcher-bin "$work/bin/$launcher_name" \
            --agent-bin "$work/bin/$agent_name" \
            --output "$work/output/invalid/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
        fail "builder accepted an unverifiable $override override"
    fi
    [ ! -e "$work/output/invalid/$BRAND_APP_BUNDLE" ] || fail 'invalid build left an artifact'
done

cp -X -- /usr/bin/true "$work/bin/$launcher_name-without-identity"
chmod 0755 "$work/bin/$launcher_name-without-identity"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name-without-identity" \
    --agent-bin "$work/bin/$agent_name" \
    --output "$work/output/stale-identity/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a launcher without the compiled Agent identity contract'
fi
[ ! -e "$work/output/stale-identity/$BRAND_APP_BUNDLE" ] || fail 'identity mismatch left an artifact'

if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name" \
    --ui-bin "$work/bin/$ui_name-dev-form" \
    --output "$work/output/dev-form-ui/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a UI without the embedded-assets contract'
fi
[ ! -e "$work/output/dev-form-ui/$BRAND_APP_BUNDLE" ] || fail 'dev-form UI left an artifact'

cp -X -- /usr/bin/true "$work/bin/$agent_name-without-identity"
chmod 0755 "$work/bin/$agent_name-without-identity"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name-without-identity" \
    --output "$work/output/stale-agent-identity/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted an Agent without its compiled identity contract'
fi
[ ! -e "$work/output/stale-agent-identity/$BRAND_APP_BUNDLE" ] ||
    fail 'missing Agent identity left an artifact'

cp -X -- /usr/bin/true "$work/bin/$menu_name-without-identity"
chmod 0755 "$work/bin/$menu_name-without-identity"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name" \
    --menu-bin "$work/bin/$menu_name-without-identity" \
    --output "$work/output/stale-menu-identity/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a Menu companion without its compiled identity contract'
fi
[ ! -e "$work/output/stale-menu-identity/$BRAND_APP_BUNDLE" ] ||
    fail 'missing Menu identity left an artifact'

for arch in arm64 x86_64; do
    fixture "$work/bin/$launcher_name-$arch" "$agent_marker" -arch "$arch"
    fixture "$work/bin/$agent_name-$arch" "$agent_marker" -arch "$arch"
    fixture "$work/bin/$menu_name-$arch" "$menu_marker" -arch "$arch"
    fixture "$work/bin/$ui_name-$arch" "$ui_marker" -arch "$arch"
done
lipo -create "$work/bin/$launcher_name-arm64" "$work/bin/$launcher_name-x86_64" \
    -output "$work/bin/$launcher_name-universal"
lipo -create "$work/bin/$agent_name-arm64" "$work/bin/$agent_name-x86_64" \
    -output "$work/bin/$agent_name-universal"
lipo -create "$work/bin/$menu_name-arm64" "$work/bin/$menu_name-x86_64" \
    -output "$work/bin/$menu_name-universal"
lipo -create "$work/bin/$ui_name-arm64" "$work/bin/$ui_name-x86_64" \
    -output "$work/bin/$ui_name-universal"
chmod 0755 "$work/bin/$launcher_name-universal" "$work/bin/$agent_name-universal" \
    "$work/bin/$menu_name-universal" "$work/bin/$ui_name-universal"
universal_bundle="$work/output/universal/$BRAND_APP_BUNDLE"
"$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name-universal" \
    --agent-bin "$work/bin/$agent_name-universal" \
    --menu-bin "$work/bin/$menu_name-universal" \
    --ui-bin "$work/bin/$ui_name-universal" \
    --output "$universal_bundle" >/dev/null
"$packaging_dir/verify-bundle.sh" "$universal_bundle" >/dev/null
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name-universal" \
    --agent-bin "$work/bin/$agent_name-universal" \
    --menu-bin "$work/bin/$menu_name-universal" \
    --ui-bin "$work/bin/$ui_name-arm64" \
    --output "$work/output/ui-mismatch/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a UI executable with different architectures'
fi
[ ! -e "$work/output/ui-mismatch/$BRAND_APP_BUNDLE" ] || fail 'UI architecture mismatch left an artifact'

fixture "$work/bin/$launcher_name-stale-x86_64" "maild-agent-bundle-id:dev.$BRAND_SLUG.stale" -arch x86_64
lipo -create "$work/bin/$launcher_name-arm64" "$work/bin/$launcher_name-stale-x86_64" \
    -output "$work/bin/$launcher_name-mixed-identity"
chmod 0755 "$work/bin/$launcher_name-mixed-identity"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name-mixed-identity" \
    --agent-bin "$work/bin/$agent_name-universal" \
    --menu-bin "$work/bin/$menu_name-universal" \
    --ui-bin "$work/bin/$ui_name-universal" \
    --output "$work/output/mixed-identity/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a universal launcher with mixed compiled Agent identities'
fi
[ ! -e "$work/output/mixed-identity/$BRAND_APP_BUNDLE" ] ||
    fail 'mixed universal identity left an artifact'

fixture "$work/bin/$agent_name-unmarked-x86_64" '' -arch x86_64
lipo -create "$work/bin/$agent_name-arm64" "$work/bin/$agent_name-unmarked-x86_64" \
    -output "$work/bin/$agent_name-missing-slice-identity"
chmod 0755 "$work/bin/$agent_name-missing-slice-identity"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name-universal" \
    --agent-bin "$work/bin/$agent_name-missing-slice-identity" \
    --menu-bin "$work/bin/$menu_name-universal" \
    --ui-bin "$work/bin/$ui_name-universal" \
    --output "$work/output/missing-slice-identity/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a universal Agent with an unmarked slice'
fi
[ ! -e "$work/output/missing-slice-identity/$BRAND_APP_BUNDLE" ] ||
    fail 'missing slice identity left an artifact'

universal_host_ui="$universal_bundle/Contents/MacOS/$ui_name"
cp -X -- "$universal_host_ui" "$work/bin/$ui_name-universal-host-copy"
cp -X -- "$work/bin/$ui_name-arm64" "$universal_host_ui"
chmod 0755 "$universal_host_ui"
if "$packaging_dir/verify-bundle.sh" "$universal_bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a single-host UI with different architectures'
fi
cp -X -- "$work/bin/$ui_name-universal-host-copy" "$universal_host_ui"
chmod 0755 "$universal_host_ui"
"$packaging_dir/verify-bundle.sh" "$universal_bundle" >/dev/null

embedded_universal_agent="$universal_bundle/$agent_rel/Contents/MacOS/$agent_name"
cp -X -- "$embedded_universal_agent" "$work/bin/$agent_name-universal-valid-copy"
cp -X -- "$work/bin/$agent_name-missing-slice-identity" "$embedded_universal_agent"
chmod 0755 "$embedded_universal_agent"
if "$packaging_dir/verify-bundle.sh" "$universal_bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a universal Agent with an unmarked slice'
fi
cp -X -- "$work/bin/$agent_name-universal-valid-copy" "$embedded_universal_agent"
chmod 0755 "$embedded_universal_agent"
"$packaging_dir/verify-bundle.sh" "$universal_bundle" >/dev/null

mkdir -- "$work/unsafe-output"
chmod 0777 "$work/unsafe-output"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name" \
    --output "$work/unsafe-output/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted a group/world-writable output parent'
fi
[ ! -e "$work/unsafe-output/$BRAND_APP_BUNDLE" ] || fail 'unsafe output parent left an artifact'

launcher_architectures=$(lipo -archs "$work/bin/$launcher_name")
case " $launcher_architectures " in
    *' arm64 '*) mismatched_arch=x86_64 ;;
    *) mismatched_arch=arm64 ;;
esac
fixture "$work/bin/$agent_name-mismatched" "$agent_marker" -arch "$mismatched_arch"
if "$packaging_dir/build-bundle.sh" \
    --launcher-bin "$work/bin/$launcher_name" \
    --agent-bin "$work/bin/$agent_name-mismatched" \
    --output "$work/output/mismatched/$BRAND_APP_BUNDLE" >/dev/null 2>&1; then
    fail 'builder accepted mismatched executable architectures'
fi
[ ! -e "$work/output/mismatched/$BRAND_APP_BUNDLE" ] ||
    fail 'architecture mismatch left an artifact'

# A corrupted/substituted UI executable is caught until the payload is explicitly resealed.
cp -X -- "$ui_executable" "$work/ui-original"
printf 'x' >>"$ui_executable"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a UI executable that no longer matches recovery.sha256'
fi
"$packaging_dir/seal-recovery-payload.sh" "$bundle"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null
cp -X -- "$work/ui-original" "$ui_executable"
"$packaging_dir/seal-recovery-payload.sh" "$bundle"
cp -X -- "$payload/frontend.toml" "$work/manifest-original"
printf 'permissions = ["plugins_manage"]\n' >>"$payload/frontend.toml"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a hand-edited bundled manifest'
fi
cp -X -- "$work/manifest-original" "$payload/frontend.toml"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

# Single-host UI: a missing, symlinked, non-executable or non-embedded (devUrl) build at
# Contents/MacOS/<ui_bin> is rejected even though the recovery copy is still valid.
cp -X -- "$host_ui" "$work/host-ui-original"
rm -- "$host_ui"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail "verifier accepted a bundle without Contents/MacOS/$ui_name"
fi
cp -X -- "$work/bin/$ui_name-dev-form" "$host_ui"
chmod 0755 "$host_ui"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>"$work/host-dev-form.stderr"; then
    fail "verifier accepted a non-embedded Contents/MacOS/$ui_name"
fi
grep -F "$ui_name (host)" "$work/host-dev-form.stderr" >/dev/null ||
    fail 'non-embedded host UI failed at the wrong gate'
rm -- "$host_ui"
ln -s -- "Resources/frontends/$BRAND_UI_BUNDLE_ID/$BRAND_UI_BUNDLE/Contents/MacOS/$ui_name" "$host_ui"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail "verifier accepted a symlinked Contents/MacOS/$ui_name"
fi
rm -- "$host_ui"
cp -X -- "$work/host-ui-original" "$host_ui"
chmod 0644 "$host_ui"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail "verifier accepted a non-executable Contents/MacOS/$ui_name"
fi
chmod 0755 "$host_ui"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

# Application icon: missing file, symlink, or a drifted CFBundleIconFile is rejected.
mv -- "$app_icon" "$work/icon-original.icns"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail "verifier accepted a bundle without Contents/Resources/$BRAND_ICON_NAME.icns"
fi
ln -s -- "$work/icon-original.icns" "$app_icon"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail "verifier accepted a symlinked Contents/Resources/$BRAND_ICON_NAME.icns"
fi
rm -- "$app_icon"
mv -- "$work/icon-original.icns" "$app_icon"
plutil -replace CFBundleIconFile -string "$BRAND_ICON_NAME-drift" "$bundle/Contents/Info.plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a drifted CFBundleIconFile'
fi
plutil -replace CFBundleIconFile -string "$BRAND_ICON_NAME" "$bundle/Contents/Info.plist"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

touch "$bundle/Contents/unexpected"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted an unexpected bundle entry'
fi
rm -- "$bundle/Contents/unexpected"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

agent_plist="$bundle/$agent_rel/Contents/Info.plist"
plutil -replace LSMinimumSystemVersion -string 12.0 "$agent_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a mismatched minimum system version'
fi
plutil -replace LSMinimumSystemVersion -string 13.0 "$agent_plist"
plutil -replace LSUIElement -bool false "$agent_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted LSUIElement=false for the agent'
fi
plutil -replace LSUIElement -bool true "$agent_plist"
main_plist="$bundle/Contents/Info.plist"
[ "$(plutil -extract LSUIElement raw -expect bool "$main_plist")" = true ] ||
    fail "generated top-level $BRAND_APP_BUNDLE is not LSUIElement=true"
plutil -replace LSUIElement -bool false "$main_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted LSUIElement=false for the top-level app'
fi
plutil -replace LSUIElement -string true "$main_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a non-boolean LSUIElement for the top-level app'
fi
plutil -remove LSUIElement "$main_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted a top-level app without LSUIElement'
fi
plutil -insert LSUIElement -bool true "$main_plist"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null
plutil -replace CFBundleVersion -string 43 "$agent_plist"
if "$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null 2>&1; then
    fail 'verifier accepted mismatched app and agent build versions'
fi
plutil -replace CFBundleVersion -string "$build_version_value" "$agent_plist"
"$packaging_dir/verify-bundle.sh" "$bundle" >/dev/null

# Rebrand without touching a script: a copy of packaging/macos with another brand.env (what
# `scripts/sync-brand.sh` writes after a brand.toml edit) must build and verify a bundle whose
# every name, identifier, icon and manifest comes from that brand.env. Spaces and `&` in the
# product name exercise path quoting and XML escaping.
rebrand="$work/rebrand/packaging/macos"
mkdir -p -- "$(dirname -- "$rebrand")"
cp -R -- "$packaging_dir" "$rebrand"
rm -rf -- "$rebrand/tests"
cat >"$rebrand/brand.env" <<'EOF'
BRAND_PRODUCT_NAME='Quill & Ink'
BRAND_SLUG=quill-ink
BRAND_ENV_PREFIX=QUILL_INK
BRAND_CLI_DAEMON=quilld
BRAND_CLI_CTL=quillctl
BRAND_LAUNCHER_BIN=quill-launcher
BRAND_AGENT_BIN=quill-agent
BRAND_MENU_BIN=quill-menu
BRAND_UI_BIN=quill-ui
BRAND_APP_BUNDLE='Quill & Ink.app'
BRAND_AGENT_BUNDLE='Quill & Ink Agent.app'
BRAND_MENU_BUNDLE='Quill & Ink Menu.app'
BRAND_UI_BUNDLE='Quill & Ink UI.app'
BRAND_ICON_NAME='Quill Ink'
BRAND_APP_BUNDLE_ID=org.example.quill.desktop
BRAND_AGENT_BUNDLE_ID=org.example.quill.agent
BRAND_MENU_BUNDLE_ID=org.example.quill.menu
BRAND_UI_BUNDLE_ID=org.example.quill.ui
EOF
printf 'org.example.quill.desktop\n' >"$rebrand/app-bundle-id.txt"
printf 'org.example.quill.agent\n' >"$rebrand/agent-bundle-id.txt"
printf 'org.example.quill.menu\n' >"$rebrand/menu-bundle-id.txt"
mkdir -- "$work/rebrand/bin"
fixture "$work/rebrand/bin/quill-launcher" 'maild-agent-bundle-id:org.example.quill.agent'
fixture "$work/rebrand/bin/quill-agent" 'maild-agent-bundle-id:org.example.quill.agent'
fixture "$work/rebrand/bin/quill-menu" 'maild-menu-bundle-id:org.example.quill.menu'
fixture "$work/rebrand/bin/quill-ui" 'maild-ui-embedded:org.example.quill.ui'
rebrand_bundle="$work/rebrand/output/Quill & Ink.app"
QUILL_INK_BUNDLE_SHORT_VERSION=5.6.7 QUILL_INK_BUNDLE_BUILD_VERSION=8 \
    "$rebrand/build-bundle.sh" \
        --launcher-bin "$work/rebrand/bin/quill-launcher" \
        --agent-bin "$work/rebrand/bin/quill-agent" \
        --output "$rebrand_bundle" >/dev/null ||
    fail 'builder could not package an alternate brand.env'
QUILL_INK_BUNDLE_SHORT_VERSION=5.6.7 QUILL_INK_BUNDLE_BUILD_VERSION=8 \
    "$rebrand/verify-bundle.sh" "$rebrand_bundle" >/dev/null ||
    fail 'verifier rejected an alternate brand.env bundle'
rebrand_payload="$rebrand_bundle/Contents/Resources/frontends/org.example.quill.ui"
for path in \
    "Contents/MacOS/quill-launcher" \
    "Contents/MacOS/quill-ui" \
    "Contents/Resources/Quill Ink.icns" \
    "Contents/Library/LoginItems/Quill & Ink Agent.app/Contents/MacOS/quill-agent" \
    "Contents/Library/Helpers/Quill & Ink Menu.app/Contents/MacOS/quill-menu" \
    "Contents/Resources/frontends/org.example.quill.ui/Quill & Ink UI.app/Contents/MacOS/quill-ui"; do
    [ -f "$rebrand_bundle/$path" ] || fail "alternate brand bundle is missing $path"
done
[ "$(plutil -extract CFBundleDisplayName raw "$rebrand_bundle/Contents/Info.plist")" = 'Quill & Ink' ] ||
    fail 'alternate brand display name was not XML-escaped and restored'
[ "$(plutil -extract CFBundleIdentifier raw "$rebrand_bundle/Contents/Info.plist")" = \
    org.example.quill.desktop ] || fail 'alternate brand app identifier drifted'
[ "$(plutil -extract CFBundleIconFile raw "$rebrand_bundle/Contents/Info.plist")" = 'Quill Ink' ] ||
    fail 'alternate brand icon name drifted'
grep -Fqx 'path = "Quill & Ink UI.app"' "$rebrand_payload/frontend.toml" ||
    fail 'alternate brand manifest launch path drifted'
grep -Fqx 'version = "5.6.7"' "$rebrand_payload/frontend.toml" ||
    fail 'alternate brand manifest version drifted'
if LC_ALL=C grep -qi -e maild -e "$BRAND_SLUG" "$rebrand_bundle/Contents/Info.plist" \
    "$rebrand_bundle/Contents/Library/LoginItems/Quill & Ink Agent.app/Contents/Info.plist" \
    "$rebrand_bundle/Contents/Library/Helpers/Quill & Ink Menu.app/Contents/Info.plist" \
    "$rebrand_payload/Quill & Ink UI.app/Contents/Info.plist"; then
    fail 'alternate brand plists still name the repository brand'
fi
# Comments may cite repository paths (crates/maild-ui); the manifest values may not.
if grep -v '^#' "$rebrand_payload/frontend.toml" | LC_ALL=C grep -qi -e maild -e "$BRAND_SLUG"; then
    fail 'alternate brand manifest still names the repository brand'
fi
# Identity manifests that drifted from brand.env (sync-brand.sh not re-run) are rejected.
printf 'org.example.stale.agent\n' >"$rebrand/agent-bundle-id.txt"
if QUILL_INK_BUNDLE_SHORT_VERSION=5.6.7 QUILL_INK_BUNDLE_BUILD_VERSION=8 \
    "$rebrand/verify-bundle.sh" "$rebrand_bundle" >/dev/null 2>"$work/rebrand-drift.stderr"; then
    fail 'verifier accepted *-bundle-id.txt drift from brand.env'
fi
grep -F 'agent-bundle-id.txt differs from brand.env' "$work/rebrand-drift.stderr" >/dev/null ||
    fail 'identity manifest drift failed at the wrong gate'

"$packaging_dir/dry-run.sh" >/dev/null
printf 'test_bundle: PASS\n'
