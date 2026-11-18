#!/bin/bash
# Regenerate the bundled frontend's recovery.sha256 from its current executable bytes.
# Run after build-bundle publishes the payload and again after the nested UI bundle is
# signed (signing rewrites the Mach-O), always BEFORE the outer app bundle is signed.
# Bundle, binary and frontend names come from brand.env (generated from brand.toml).
set -euo pipefail

die() {
    printf 'seal-recovery-payload: %s\n' "$*" >&2
    exit 1
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
# shellcheck source=brand.env
. "$script_dir/brand.env"

[ "$#" -eq 1 ] || die "usage: seal-recovery-payload.sh PATH/$BRAND_APP_BUNDLE"
bundle=$1
payload="$bundle/Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID"
executable="$payload/$BRAND_UI_BUNDLE/Contents/MacOS/$BRAND_UI_BIN"
checksum="$payload/recovery.sha256"
[ "$(basename -- "$bundle")" = "$BRAND_APP_BUNDLE" ] ||
    die "bundle final component must be $BRAND_APP_BUNDLE"
[ ! -L "$executable" ] && [ -f "$executable" ] || die "missing bundled UI executable: $executable"
[ ! -L "$checksum" ] || die "checksum must not be a symbolic link: $checksum"
[ ! -e "$bundle/Contents/_CodeSignature" ] ||
    die "outer $BRAND_APP_BUNDLE is already signed; seal before signing it"
digest=$(/usr/bin/shasum -a 256 "$executable" | awk '{print $1}')
printf '%s\n' "$digest" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' || die 'cannot compute UI digest'
temporary=$(mktemp "$payload/.recovery.sha256.XXXXXX") || die 'cannot stage checksum'
printf '%s\n' "$digest" >"$temporary"
chmod 0644 "$temporary"
mv -f -- "$temporary" "$checksum"
