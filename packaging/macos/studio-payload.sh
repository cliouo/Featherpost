#!/bin/bash
# Shared packaging identities for the independently managed Studio frontend.
STUDIO_FRONTEND_ID=dev.featherpost.studio
STUDIO_APP='Featherpost Studio.app'
STUDIO_BIN=featherpost-studio

studio_payload_path() {
    printf '%s/Contents/Resources/frontends/%s' "$1" "$STUDIO_FRONTEND_ID"
}

studio_host_path() {
    printf '%s/Contents/MacOS/%s' "$1" "$STUDIO_BIN"
}

studio_payload_version() {
    sed -nE 's/^version = "([^"]+)"$/\1/p' "$script_dir/studio/frontend.toml"
}

# Caller has validated the binary and is building an unpublished private staging tree.
copy_studio_payload() {
    local payload app
    payload=$(studio_payload_path "$1")
    app="$payload/$STUDIO_APP"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp -X -- "$2" "$app/Contents/MacOS/$STUDIO_BIN"
    cp -X -- "$script_dir/studio/frontend.toml" "$payload/frontend.toml"
    cp -X -- "$script_dir/studio/Info.plist" "$app/Contents/Info.plist"
    cp -X -- "$script_dir/assets/AppIcon.icns" "$app/Contents/Resources/$BRAND_ICON_NAME.icns"
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string $BRAND_ICON_NAME" "$app/Contents/Info.plist"
    chmod 0755 "$app/Contents/MacOS/$STUDIO_BIN"
}

# Called by verify-bundle.sh: keep the allowlist and integrity gates closed over this payload.
verify_studio_payload() {
    local payload app exe host_exe rel version
    payload=$(studio_payload_path "$1")
    host_exe=$(studio_host_path "$1")
    [ -e "$payload" ] || [ -L "$payload" ] || return 0
    app="$payload/$STUDIO_APP"
    exe="$app/Contents/MacOS/$STUDIO_BIN"
    rel="./Contents/Resources/frontends/$STUDIO_FRONTEND_ID"
    version=$(studio_payload_version)
    require_directory "$payload"
    require_directory "$app"
    require_directory "$app/Contents"
    require_directory "$app/Contents/MacOS"
    require_directory "$app/Contents/Resources"
    require_file "$host_exe"
    require_file "$exe"
    require_file "$payload/frontend.toml"
    require_file "$payload/recovery.sha256"
    require_file "$app/Contents/Info.plist"
    require_file "$app/Contents/Resources/$BRAND_ICON_NAME.icns"
    [ -x "$host_exe" ] || die 'host Studio executable bit is missing'
    [ -x "$exe" ] || die 'Studio executable bit is missing'
    [ "$(binary_architectures "Studio (host)" "$host_exe")" = "$launcher_architectures" ] ||
        die 'host Studio architectures differ'
    [ "$(binary_architectures Studio "$exe")" = "$launcher_architectures" ] || die 'Studio architectures differ'
    require_compiled_identity "Studio (host)" "$host_exe" 'maild-ui-embedded:' "$STUDIO_FRONTEND_ID"
    require_compiled_identity Studio "$exe" 'maild-ui-embedded:' "$STUDIO_FRONTEND_ID"
    [ "$(cat "$payload/frontend.toml")" = "$(cat "$script_dir/studio/frontend.toml")" ] || die 'Studio manifest drifted'
    [ "$(wc -c <"$payload/recovery.sha256" | tr -d ' ')" = 65 ] || die 'Studio checksum must be 65 bytes'
    [ "$(cat "$payload/recovery.sha256")" = "$(shasum -a 256 "$exe" | awk '{print $1}')" ] || die 'Studio checksum mismatch'
    plutil -lint "$app/Contents/Info.plist" >/dev/null
    expect_plist_string "$app/Contents/Info.plist" CFBundleIdentifier "$STUDIO_FRONTEND_ID"
    expect_plist_string "$app/Contents/Info.plist" CFBundleExecutable "$STUDIO_BIN"
    expect_plist_string "$app/Contents/Info.plist" CFBundleIconFile "$BRAND_ICON_NAME"
    expect_plist_string "$app/Contents/Info.plist" CFBundleShortVersionString "$version"
    expect_plist_string "$app/Contents/Info.plist" CFBundleVersion "$version"
    expect_frontend_localizations "$app/Contents/Info.plist"
    expected_layout=$(printf '%s\n%s\n' "$expected_layout" "$(cat <<EOF
./Contents/MacOS/$STUDIO_BIN
$rel
$rel/frontend.toml
$rel/recovery.sha256
$rel/$STUDIO_APP
$rel/$STUDIO_APP/Contents
$rel/$STUDIO_APP/Contents/Info.plist
$rel/$STUDIO_APP/Contents/MacOS
$rel/$STUDIO_APP/Contents/MacOS/$STUDIO_BIN
$rel/$STUDIO_APP/Contents/Resources
$rel/$STUDIO_APP/Contents/Resources/$BRAND_ICON_NAME.icns
EOF
)")
    if [ -e "$main_signature" ] || [ -e "$app/Contents/_CodeSignature" ]; then
        require_directory "$app/Contents/_CodeSignature"
        require_file "$app/Contents/_CodeSignature/CodeResources"
        expected_layout=$(printf '%s\n%s\n%s\n' "$expected_layout" \
            "$rel/$STUDIO_APP/Contents/_CodeSignature" "$rel/$STUDIO_APP/Contents/_CodeSignature/CodeResources")
    fi
}
