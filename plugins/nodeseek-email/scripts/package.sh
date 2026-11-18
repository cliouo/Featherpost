#!/bin/sh
set -eu

plugin_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
workspace_root=$(CDPATH= cd -- "$plugin_root/../.." && pwd)
package_target="$workspace_root/target"
dist_root="$plugin_root/dist"
package_root="$dist_root/nodeseek-email"

require_owned_directory() {
  directory=$1
  if [ ! -d "$directory" ] || [ -L "$directory" ]; then
    echo "refusing to use unsafe directory: $directory" >&2
    exit 1
  fi
  wrong_owner=$(find "$directory" -prune ! -user "$(id -u)" -print -quit)
  writable_by_others=$(find "$directory" -prune \( -perm -020 -o -perm -002 \) -print -quit)
  if [ -n "$wrong_owner" ] || [ -n "$writable_by_others" ]; then
    echo "refusing to use unowned or group/other-writable directory: $directory" >&2
    exit 1
  fi
}

CARGO_TARGET_DIR="$package_target" cargo build \
  --manifest-path "$plugin_root/Cargo.toml" \
  --release \
  --locked \
  --features ui \
  --bins

if [ -e "$dist_root" ] || [ -L "$dist_root" ]; then
  require_owned_directory "$dist_root"
else
  install -d -m 0755 "$dist_root"
  require_owned_directory "$dist_root"
fi
stage_root=$(mktemp -d "$dist_root/.nodeseek-email-package.XXXXXX")
stage_package="$stage_root/nodeseek-email"
cleanup() {
  rm -rf -- "$stage_root"
}
trap cleanup EXIT HUP INT TERM

install -d -m 0755 "$stage_package/bin"
install -m 0644 "$plugin_root/plugin.toml" "$stage_package/plugin.toml"
install -m 0755 \
  "$package_target/release/nodeseek-email-provider" \
  "$stage_package/bin/nodeseek-email-provider"
install -m 0755 \
  "$package_target/release/nodeseek-email" \
  "$stage_package/bin/nodeseek-email"
install -m 0755 \
  "$package_target/release/nodeseek-email-ui" \
  "$stage_package/bin/nodeseek-email-ui"

# Keep the designated requirement's identifier stable across upgrades. The management UI refreshes
# the Keychain trusted-application ACL on every foreground save, while this identifier prevents
# unnecessary requirement churn for installations that retain the same signed binary.
codesign --force --sign - --identifier nodeseek_email_provider-fe80b4512117ab7a \
  "$stage_package/bin/nodeseek-email-provider"
codesign --force --sign - --identifier nodeseek_email_ui-1e021218460ac216 \
  "$stage_package/bin/nodeseek-email-ui"
codesign --force --sign - --identifier nodeseek_email-4b5e325235369006 \
  "$stage_package/bin/nodeseek-email"

if [ -e "$package_root" ] || [ -L "$package_root" ]; then
  require_owned_directory "$package_root"
  if [ -e "$package_root/bin" ] || [ -L "$package_root/bin" ]; then
    require_owned_directory "$package_root/bin"
  fi

  unexpected=$(find "$package_root" -mindepth 1 \
    ! -path "$package_root/bin" \
    ! -path "$package_root/bin/nodeseek-email-provider" \
    ! -path "$package_root/bin/nodeseek-email" \
    ! -path "$package_root/bin/nodeseek-email-ui" \
    ! -path "$package_root/plugin.toml" \
    -print -quit)
  if [ -n "$unexpected" ]; then
    echo "refusing to discard unexpected package residue: $unexpected" >&2
    exit 1
  fi

  for path in \
    "$package_root/plugin.toml" \
    "$package_root/bin/nodeseek-email-provider" \
    "$package_root/bin/nodeseek-email" \
    "$package_root/bin/nodeseek-email-ui"
  do
    if [ -e "$path" ] || [ -L "$path" ]; then
      if [ ! -f "$path" ] || [ -L "$path" ]; then
        echo "refusing to replace unsafe package entry: $path" >&2
        exit 1
      fi
      rm -- "$path"
    fi
  done
  if [ -d "$package_root/bin" ]; then
    rmdir "$package_root/bin"
  fi
  rmdir "$package_root"
fi

mv "$stage_package" "$package_root"
rmdir "$stage_root"
trap - EXIT HUP INT TERM

echo "package ready: $package_root"
