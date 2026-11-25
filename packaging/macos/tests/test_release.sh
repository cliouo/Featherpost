#!/bin/bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    printf 'test_release: SKIP (macOS required)\n'
    exit 0
fi

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
packaging_dir=$(CDPATH= cd -- "$test_dir/.." && pwd -P)
# shellcheck source=../brand.env
. "$packaging_dir/brand.env"
work=$(mktemp -d "${TMPDIR:-/tmp}/$BRAND_SLUG-release-test.XXXXXX")
cleanup() {
    rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

fail() {
    printf 'test_release: %s\n' "$*" >&2
    exit 1
}

for tool in cc codesign ditto git lipo perl plutil; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
done

hostile_bin="$work/hostile-bin"
hostile_marker="$work/hostile-tool-used"
mkdir -m 0700 -- "$hostile_bin"
for tool in codesign ditto git lipo plutil stat xcrun; do
    printf '#!/bin/sh\nprintf used >%s\nexit 99\n' "$hostile_marker" >"$hostile_bin/$tool"
    chmod 0755 "$hostile_bin/$tool"
done

mkdir -p -- "$work/bin" "$work/output"
agent_bundle_id=$BRAND_AGENT_BUNDLE_ID
menu_bundle_id=$BRAND_MENU_BUNDLE_ID
ui_id=$BRAND_UI_BUNDLE_ID
launcher_name=$BRAND_LAUNCHER_BIN
agent_name=$BRAND_AGENT_BIN
menu_name=$BRAND_MENU_BIN
ui_name=$BRAND_UI_BIN
app_bundle=$BRAND_APP_BUNDLE
agent_rel="Contents/Library/LoginItems/$BRAND_AGENT_BUNDLE"
menu_rel="Contents/Library/Helpers/$BRAND_MENU_BUNDLE"
ui_rel="Contents/Resources/frontends/$ui_id/$BRAND_UI_BUNDLE"
host_ui_rel="Contents/MacOS/$ui_name"
icon_rel="Contents/Resources/$BRAND_ICON_NAME.icns"
versions=("${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION=0.1.0" "${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION=0.1.0")

# Usage: universal_fixture OUTPUT MARKER; an empty MARKER builds an unmarked (dev-form) binary.
universal_fixture() {
    local output=$1
    local marker=$2
    local architecture
    for architecture in arm64 x86_64; do
        if [ -n "$marker" ]; then
            printf '%s\n' \
                "__attribute__((used)) static const char identity[] = \"$marker\";" \
                'int main(void) { return identity[0] == 0; }'
        else
            printf '%s\n' 'int main(void) { return 0; }'
        fi | cc -arch "$architecture" -x c - -o "$output-$architecture"
    done
    lipo -create "$output-arm64" "$output-x86_64" -output "$output"
    chmod 0755 "$output"
}
universal_fixture "$work/bin/$launcher_name" "maild-agent-bundle-id:$agent_bundle_id"
universal_fixture "$work/bin/$agent_name" "maild-agent-bundle-id:$agent_bundle_id"
universal_fixture "$work/bin/$menu_name" "maild-menu-bundle-id:$menu_bundle_id"
universal_fixture "$work/bin/$ui_name" "maild-ui-embedded:$ui_id"
universal_fixture "$work/bin/$ui_name-dev-form" ''

bundle="$work/output/unsigned/$app_bundle"
env "${versions[@]}" \
    "$packaging_dir/build-bundle.sh" \
        --launcher-bin "$work/bin/$launcher_name" \
        --agent-bin "$work/bin/$agent_name" \
        --output "$bundle" >/dev/null

partial="$work/output/partial/$app_bundle"
mkdir -p -- "$(dirname -- "$partial")"
ditto --noqtn --noextattr --noacl "$bundle" "$partial"
partial_agent="$partial/$agent_rel"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$partial_agent" >/dev/null
if env "${versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$partial" >/dev/null 2>&1; then
    fail 'structural verifier accepted a partially signed bundle'
fi

signed="$work/output/signed/$app_bundle"
mkdir -p -- "$(dirname -- "$signed")"
ditto --noqtn --noextattr --noacl "$bundle" "$signed"
signed_agent="$signed/$agent_rel"
signed_menu="$signed/$menu_rel"
signed_ui="$signed/$ui_rel"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$signed_ui" >/dev/null
# Signing rewrote the UI Mach-O: the stale recovery checksum must be rejected until resealed.
if env "${versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$signed" >/dev/null 2>&1; then
    fail 'structural verifier accepted a signed UI with a stale recovery checksum'
fi
"$packaging_dir/seal-recovery-payload.sh" "$signed"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$signed_agent" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$signed_menu" >/dev/null
/usr/bin/codesign --force --sign - --identifier "$ui_id" --options runtime --timestamp=none \
    "$signed/$host_ui_rel" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$signed" >/dev/null
host_ui_details=$(/usr/bin/codesign --display --verbose=4 "$signed/$host_ui_rel" 2>&1)
printf '%s\n' "$host_ui_details" | LC_ALL=C grep -Eq \
    '^CodeDirectory .* flags=.*[(].*runtime.*[)]' ||
    fail 'single-host UI was not signed with the hardened runtime'
printf '%s\n' "$host_ui_details" | LC_ALL=C grep -Fqx "Identifier=$ui_id" ||
    fail "single-host UI signing identifier is not $ui_id"
signed_details=$(/usr/bin/codesign --display --verbose=4 "$signed_agent" 2>&1)
printf '%s\n' "$signed_details" | LC_ALL=C grep -Eq \
    '^CodeDirectory .* flags=.*[(].*runtime.*[)]' ||
    fail 'codesign details did not expose the hardened runtime flag'
entitlements_file="$work/extracted-entitlements.plist"
: >"$entitlements_file"
/usr/bin/codesign --display --entitlements "$entitlements_file" "$signed_agent" \
    >/dev/null 2>&1 || fail 'codesign could not extract entitlements to a file'
[ ! -s "$entitlements_file" ] || fail 'ad hoc fixture unexpectedly contains entitlements'
env "${versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$signed" >/dev/null
/usr/bin/codesign --verify --deep --strict=all --verbose=4 "$signed" >/dev/null 2>&1 ||
    fail 'inside-out ad hoc fixture did not verify'

# Copy the sealed fixture, apply one mutation, and require the deep verifier to reject it.
reject_signed_variant() {
    local name=$1
    local description=$2
    local variant="$work/output/$name/$app_bundle"
    shift 2
    mkdir -p -- "$(dirname -- "$variant")"
    ditto --noqtn --noextattr --noacl "$signed" "$variant"
    "$@" "$variant"
    if /usr/bin/codesign --verify --deep --strict=all --verbose=4 "$variant" \
        >/dev/null 2>&1; then
        fail "deep signature verification accepted $description"
    fi
}
tamper_host_ui() { printf 'x' >>"$1/$host_ui_rel"; }
unsign_host_ui() { /usr/bin/codesign --remove-signature "$1/$host_ui_rel"; }
swap_dev_form_host_ui() {
    cp -X -- "$work/bin/$ui_name-dev-form" "$1/$host_ui_rel"
    chmod 0755 "$1/$host_ui_rel"
}
drop_app_icon() { rm -- "$1/$icon_rel"; }
reject_signed_variant tampered-host-ui "a tampered $host_ui_rel" tamper_host_ui
reject_signed_variant unsigned-host-ui "an unsigned $host_ui_rel" unsign_host_ui
reject_signed_variant dev-form-host-ui "a substituted non-embedded $ui_name" \
    swap_dev_form_host_ui
if env "${versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$work/output/dev-form-host-ui/$app_bundle" \
    >/dev/null 2>&1; then
    fail "structural verifier accepted a non-embedded signed $host_ui_rel"
fi
reject_signed_variant missing-icon "a removed $icon_rel" drop_app_icon
if env "${versions[@]}" \
    "$packaging_dir/verify-bundle.sh" "$work/output/missing-icon/$app_bundle" \
    >/dev/null 2>&1; then
    fail "structural verifier accepted a signed bundle without $BRAND_ICON_NAME.icns"
fi

bad_order="$work/output/bad-order/$app_bundle"
mkdir -p -- "$(dirname -- "$bad_order")"
ditto --noqtn --noextattr --noacl "$signed" "$bad_order"
bad_agent="$bad_order/$agent_rel"
plutil -replace CFBundleVersion -string 0.1.1 "$bad_agent/Contents/Info.plist"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none "$bad_agent" >/dev/null
if /usr/bin/codesign --verify --deep --strict=all --verbose=4 "$bad_order" \
    >/dev/null 2>&1; then
    fail 'top-level signature survived mutation of the nested Agent'
fi

if PATH="$hostile_bin:$PATH" "$packaging_dir/verify-release.sh" \
    --app "$signed" --version 0.1.0 \
    >"$work/verify-release.stdout" 2>"$work/verify-release.stderr"; then
    fail 'release verifier accepted a provisional ad hoc bundle'
fi
grep -F 'provisional App bundle identifier cannot be released' \
    "$work/verify-release.stderr" >/dev/null ||
    fail 'release verifier did not fail at the provisional identity gate'
[ ! -e "$hostile_marker" ] || fail 'release verifier trusted an injected Apple tool path'

fixture_repo="$work/repo"
mkdir -p -- \
    "$fixture_repo/packaging" \
    "$fixture_repo/crates/maild" \
    "$fixture_repo/crates/maild-launcher" \
    "$fixture_repo/crates/maild-menu" \
    "$fixture_repo/crates/maild-ui"
cp -R -- "$packaging_dir" "$fixture_repo/packaging/macos"
printf '[package]\nname = "maild"\nversion = "0.1.0"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild/Cargo.toml"
printf '[package]\nname = "maild-launcher"\nversion = "0.1.0"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild-launcher/Cargo.toml"
printf '[package]\nname = "maild-menu"\nversion = "0.1.0"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild-menu/Cargo.toml"
printf '[package]\nname = "maild-ui"\nversion = "0.1.0"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild-ui/Cargo.toml"
git -C "$fixture_repo" init -q
git -C "$fixture_repo" config user.name 'Packaging Test'
git -C "$fixture_repo" config user.email 'packaging-test@example.invalid'
git -C "$fixture_repo" add -A
git -C "$fixture_repo" commit -qm fixture
git -C "$fixture_repo" tag v0.1.0

if PATH="$hostile_bin:$PATH" "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$bundle" --version 0.1.0 --preflight-only \
    >"$work/preflight.stdout" 2>"$work/preflight.stderr"; then
    fail 'release preflight accepted provisional identities'
fi
grep -F 'provisional App bundle identifier cannot be released' \
    "$work/preflight.stderr" >/dev/null ||
    fail 'release preflight did not reach the provisional identity gate'
[ ! -e "$hostile_marker" ] || fail 'release preflight trusted an injected Apple tool path'

printf 'dirty\n' >"$fixture_repo/untracked"
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$bundle" --version 0.1.0 --preflight-only \
    >/dev/null 2>"$work/dirty.stderr"; then
    fail 'release preflight accepted a dirty worktree'
fi
grep -F 'release requires a clean worktree' "$work/dirty.stderr" >/dev/null ||
    fail 'dirty worktree failed at the wrong gate'
rm -- "$fixture_repo/untracked"

git -C "$fixture_repo" tag -d v0.1.0 >/dev/null
git -C "$fixture_repo" tag release-0.1.0
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$bundle" --version 0.1.0 --preflight-only \
    >/dev/null 2>"$work/tag.stderr"; then
    fail 'release preflight accepted a non-vX.Y.Z tag'
fi
grep -F 'HEAD must have the exact release tag v0.1.0' "$work/tag.stderr" >/dev/null ||
    fail 'non-release tag failed at the wrong gate'

# Freeze non-provisional IDs (what editing brand.toml + sync-brand.sh produces) and rewrite
# the same-length identity marker in both fixture slices so a successful formal preflight can
# reach later security gates. The repository identity must still be the provisional
# dev.<slug>.* namespace, and dev. -> com. keeps every marker the same length.
git -C "$fixture_repo" tag -d release-0.1.0 >/dev/null
for provisional in "$BRAND_APP_BUNDLE_ID" "$BRAND_AGENT_BUNDLE_ID" "$BRAND_MENU_BUNDLE_ID"; do
    case "$provisional" in
        "dev.$BRAND_SLUG".*) ;;
        *) fail "repository bundle identifier is not provisional dev.$BRAND_SLUG.*: $provisional" ;;
    esac
done
formal_app_id="com.${BRAND_APP_BUNDLE_ID#dev.}"
formal_agent_id="com.${BRAND_AGENT_BUNDLE_ID#dev.}"
formal_menu_id="com.${BRAND_MENU_BUNDLE_ID#dev.}"
printf '%s\n' "$formal_app_id" >"$fixture_repo/packaging/macos/app-bundle-id.txt"
printf '%s\n' "$formal_agent_id" >"$fixture_repo/packaging/macos/agent-bundle-id.txt"
printf '%s\n' "$formal_menu_id" >"$fixture_repo/packaging/macos/menu-bundle-id.txt"
sed -i '' \
    -e "s/^BRAND_APP_BUNDLE_ID=.*/BRAND_APP_BUNDLE_ID=$formal_app_id/" \
    -e "s/^BRAND_AGENT_BUNDLE_ID=.*/BRAND_AGENT_BUNDLE_ID=$formal_agent_id/" \
    -e "s/^BRAND_MENU_BUNDLE_ID=.*/BRAND_MENU_BUNDLE_ID=$formal_menu_id/" \
    "$fixture_repo/packaging/macos/brand.env"
git -C "$fixture_repo" add packaging/macos/app-bundle-id.txt \
    packaging/macos/agent-bundle-id.txt packaging/macos/menu-bundle-id.txt \
    packaging/macos/brand.env
git -C "$fixture_repo" commit -qm 'freeze fixture identities'
git -C "$fixture_repo" tag v0.1.0

formal_bundle="$work/output/formal/$app_bundle"
mkdir -p -- "$(dirname -- "$formal_bundle")"
ditto --noqtn --noextattr --noacl "$bundle" "$formal_bundle"
formal_agent="$formal_bundle/$agent_rel"
formal_menu="$formal_bundle/$menu_rel"
plutil -replace CFBundleIdentifier -string "$formal_app_id" \
    "$formal_bundle/Contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$formal_agent_id" \
    "$formal_agent/Contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$formal_menu_id" \
    "$formal_menu/Contents/Info.plist"
FROM_ID=$BRAND_AGENT_BUNDLE_ID TO_ID=$formal_agent_id perl -pi -e 's/\Q$ENV{FROM_ID}\E/$ENV{TO_ID}/g' \
    "$formal_bundle/Contents/MacOS/$launcher_name" \
    "$formal_agent/Contents/MacOS/$agent_name"
FROM_ID=$BRAND_MENU_BUNDLE_ID TO_ID=$formal_menu_id perl -pi -e 's/\Q$ENV{FROM_ID}\E/$ENV{TO_ID}/g' \
    "$formal_menu/Contents/MacOS/$menu_name"
chmod 0755 "$formal_bundle/Contents/MacOS/$launcher_name" \
    "$formal_agent/Contents/MacOS/$agent_name" "$formal_menu/Contents/MacOS/$menu_name"
env "${versions[@]}" \
    "$fixture_repo/packaging/macos/verify-bundle.sh" "$formal_bundle" >/dev/null
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 --preflight-only \
    >/dev/null 2>"$work/team-unset.stderr"; then
    fail 'release preflight accepted an UNSET trusted Team ID'
fi
grep -F 'trusted Team ID is UNSET' "$work/team-unset.stderr" >/dev/null ||
    fail 'UNSET trusted Team ID failed at the wrong gate'

printf 'ABCDE12345\nSECONDTEAM\n' >"$fixture_repo/packaging/macos/team-id.txt"
git -C "$fixture_repo" add packaging/macos/team-id.txt
git -C "$fixture_repo" commit -qm 'add malformed fixture Team ID'
git -C "$fixture_repo" tag -f v0.1.0 >/dev/null
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 --preflight-only \
    >/dev/null 2>"$work/team-lines.stderr"; then
    fail 'release preflight accepted multiple trusted Team ID lines'
fi
grep -F 'trusted Team ID manifest must contain exactly one line' \
    "$work/team-lines.stderr" >/dev/null ||
    fail 'multi-line trusted Team ID failed at the wrong gate'

printf 'ABCDE12345\n' >"$fixture_repo/packaging/macos/team-id.txt"
git -C "$fixture_repo" add packaging/macos/team-id.txt
git -C "$fixture_repo" commit -qm 'freeze fixture Team ID'
git -C "$fixture_repo" tag -f v0.1.0 >/dev/null
"$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 --preflight-only >/dev/null

formal_signed="$work/output/formal-signed/$app_bundle"
mkdir -p -- "$(dirname -- "$formal_signed")"
ditto --noqtn --noextattr --noacl "$formal_bundle" "$formal_signed"
formal_signed_agent="$formal_signed/$agent_rel"
formal_signed_menu="$formal_signed/$menu_rel"
formal_signed_ui="$formal_signed/$ui_rel"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$formal_signed_ui" >/dev/null
"$fixture_repo/packaging/macos/seal-recovery-payload.sh" "$formal_signed"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$formal_signed_agent" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$formal_signed_menu" >/dev/null
formal_unsigned_host="$work/output/formal-unsigned-host/$app_bundle"
mkdir -p -- "$(dirname -- "$formal_unsigned_host")"
ditto --noqtn --noextattr --noacl "$formal_signed" "$formal_unsigned_host"
/usr/bin/codesign --force --sign - --identifier "$ui_id" --options runtime \
    --timestamp=none "$formal_signed/$host_ui_rel" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$formal_signed" >/dev/null
if "$fixture_repo/packaging/macos/verify-release.sh" \
    --app "$formal_signed" --version 0.1.0 \
    >/dev/null 2>"$work/trusted-team.stderr"; then
    fail 'release verifier accepted a candidate outside the trusted Team ID'
fi
grep -F 'TeamIdentifier does not match ABCDE12345' "$work/trusted-team.stderr" >/dev/null ||
    fail 'candidate Team mismatch failed at the wrong gate'

# Skipping the explicit host UI signature leaves only a plain (linker-style) ad hoc
# signature without the hardened runtime; the release verifier must name the host UI.
/usr/bin/codesign --force --sign - "$formal_unsigned_host/$host_ui_rel" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$formal_unsigned_host" >/dev/null
if "$fixture_repo/packaging/macos/verify-release.sh" \
    --app "$formal_unsigned_host" --version 0.1.0 \
    >/dev/null 2>"$work/unsigned-host.stderr"; then
    fail 'release verifier accepted a host UI without its explicit signature'
fi
grep -F "$BRAND_PRODUCT_NAME host UI signing identifier does not match $ui_id" \
    "$work/unsigned-host.stderr" >/dev/null ||
    fail 'host UI missing explicit signature failed at the wrong gate'

formal_stripped_host="$work/output/formal-stripped-host/$app_bundle"
mkdir -p -- "$(dirname -- "$formal_stripped_host")"
ditto --noqtn --noextattr --noacl "$formal_signed" "$formal_stripped_host"
/usr/bin/codesign --remove-signature "$formal_stripped_host/$host_ui_rel"
if "$fixture_repo/packaging/macos/verify-release.sh" \
    --app "$formal_stripped_host" --version 0.1.0 \
    >/dev/null 2>"$work/stripped-host.stderr"; then
    fail 'release verifier accepted a host UI whose signature was stripped after sealing'
fi
grep -F "$BRAND_PRODUCT_NAME host UI code signature is invalid" "$work/stripped-host.stderr" >/dev/null ||
    fail 'stripped host UI signature failed at the wrong gate'

# Studio single-host release gate: a Studio bundle must sign Contents/MacOS/featherpost-studio
# explicitly with hardened runtime + dev.featherpost.studio identifier; skipping it is caught.
universal_fixture "$work/bin/featherpost-studio" "maild-ui-embedded:dev.featherpost.studio"
studio_formal="$work/output/formal-studio/$app_bundle"
env "${versions[@]}" \
    "$fixture_repo/packaging/macos/build-bundle.sh" \
        --launcher-bin "$formal_bundle/Contents/MacOS/$launcher_name" \
        --agent-bin "$formal_agent/Contents/MacOS/$agent_name" \
        --menu-bin "$formal_menu/Contents/MacOS/$menu_name" \
        --ui-bin "$work/bin/$ui_name" \
        --studio-bin "$work/bin/featherpost-studio" \
        --output "$studio_formal" >/dev/null
studio_payload_rel="Contents/Resources/frontends/dev.featherpost.studio"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$studio_formal/$ui_rel" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$studio_formal/$studio_payload_rel/Featherpost Studio.app" >/dev/null
"$fixture_repo/packaging/macos/seal-recovery-payload.sh" "$studio_formal"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$studio_formal/$agent_rel" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$studio_formal/$menu_rel" >/dev/null
/usr/bin/codesign --force --sign - --identifier "$ui_id" --options runtime \
    --timestamp=none "$studio_formal/$host_ui_rel" >/dev/null
# Leave host Studio with a plain linker-style ad hoc signature (no identifier / no runtime).
/usr/bin/codesign --force --sign - "$studio_formal/Contents/MacOS/featherpost-studio" >/dev/null
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$studio_formal" >/dev/null
if "$fixture_repo/packaging/macos/verify-release.sh" \
    --app "$studio_formal" --version 0.1.0 \
    >/dev/null 2>"$work/unsigned-host-studio.stderr"; then
    fail 'release verifier accepted a host Studio without its explicit signature'
fi
grep -F "$BRAND_PRODUCT_NAME host Studio" "$work/unsigned-host-studio.stderr" >/dev/null ||
    fail 'host Studio missing explicit signature failed at the wrong gate'

printf '[package]\nname = "maild-launcher"\nversion = "0.1.1"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild-launcher/Cargo.toml"
git -C "$fixture_repo" add crates/maild-launcher/Cargo.toml
git -C "$fixture_repo" commit -qm 'drift launcher version'
git -C "$fixture_repo" tag -f v0.1.0 >/dev/null
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 --preflight-only \
    >/dev/null 2>"$work/version-drift.stderr"; then
    fail 'release preflight accepted package version drift'
fi
grep -F 'package version does not match v0.1.0' "$work/version-drift.stderr" >/dev/null ||
    fail 'package version drift failed at the wrong gate'

printf '[package]\nname = "maild-launcher"\nversion = "0.1.0"\nedition = "2021"\n' \
    >"$fixture_repo/crates/maild-launcher/Cargo.toml"
git -C "$fixture_repo" add crates/maild-launcher/Cargo.toml
git -C "$fixture_repo" commit -qm 'restore launcher version'
git -C "$fixture_repo" tag -f v0.1.0 >/dev/null

credentials="$work/credentials"
mkdir -m 0700 -- "$credentials"
printf 'fixture\n' >"$credentials/signing.keychain-db"
printf 'fixture\n' >"$credentials/AuthKey_FIXTURE.p8"
chmod 0644 "$credentials/signing.keychain-db"
chmod 0600 "$credentials/AuthKey_FIXTURE.p8"
mkdir -m 0700 -- "$work/release-output"
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 \
    --identity fixture \
    --keychain "$credentials/signing.keychain-db" \
    --notary-key "$credentials/AuthKey_FIXTURE.p8" \
    --notary-key-id ABCDE12345 \
    --notary-issuer 00000000-0000-0000-0000-000000000000 \
    --output-dir "$work/release-output" \
    >/dev/null 2>"$work/keychain-mode.stderr"; then
    fail 'release accepted a group-readable signing keychain'
fi
grep -F 'signing keychain permissions must not allow group/other access' \
    "$work/keychain-mode.stderr" >/dev/null ||
    fail 'insecure signing keychain failed at the wrong gate'

chmod 0600 "$credentials/signing.keychain-db"
ln -s -- "$credentials/AuthKey_FIXTURE.p8" "$credentials/notary-link.p8"
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 \
    --identity fixture \
    --keychain "$credentials/signing.keychain-db" \
    --notary-key "$credentials/notary-link.p8" \
    --notary-key-id ABCDE12345 \
    --notary-issuer 00000000-0000-0000-0000-000000000000 \
    --output-dir "$work/release-output" \
    >/dev/null 2>"$work/notary-symlink.stderr"; then
    fail 'release accepted a symlinked notary API key'
fi
grep -F 'notary API key must not be a symbolic link' \
    "$work/notary-symlink.stderr" >/dev/null ||
    fail 'symlinked notary API key failed at the wrong gate'

mkdir -m 0700 -- "$work/release-real" "$work/release-real/output"
ln -s -- "$work/release-real" "$work/release-link"
if "$fixture_repo/packaging/macos/release-bundle.sh" \
    --app "$formal_bundle" --version 0.1.0 \
    --identity fixture \
    --keychain "$credentials/signing.keychain-db" \
    --notary-key "$credentials/AuthKey_FIXTURE.p8" \
    --notary-key-id ABCDE12345 \
    --notary-issuer 00000000-0000-0000-0000-000000000000 \
    --output-dir "$work/release-link/output" \
    >/dev/null 2>"$work/output-symlink.stderr"; then
    fail 'release accepted a symlinked output ancestor'
fi
grep -F 'output path ancestor must not be a symbolic link' \
    "$work/output-symlink.stderr" >/dev/null ||
    fail 'symlinked output ancestor failed at the wrong gate'

"$packaging_dir/build-universal.sh" --help >/dev/null
"$packaging_dir/verify-release.sh" --help >/dev/null
"$packaging_dir/release-bundle.sh" --help >/dev/null
printf 'test_release: PASS\n'
