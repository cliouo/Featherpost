#!/usr/bin/env bash
# Propagate brand.toml (the single source of truth for product branding) into every derived file.
#
#   bash scripts/sync-brand.sh            rewrite derived files from brand.toml
#   bash scripts/sync-brand.sh --check    read-only: exit 1 and print a diff if anything drifted
#   bash scripts/sync-brand.sh --icons    (macOS) also re-render icons from assets/brand/icon-artwork.jpg
#
# Only the Python 3.11+ standard library (tomllib) is required. Output is deterministic and the
# write mode is idempotent, so `--check` right after a write always passes.
set -euo pipefail

usage() {
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
}

mode=write
icons=0
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        --icons) icons=1 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'sync-brand: unknown argument: %s\n' "$arg" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ "$mode" == check && "$icons" == 1 ]]; then
    printf 'sync-brand: --icons rewrites binary assets and cannot be combined with --check\n' >&2
    exit 2
fi

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if ! command -v python3 >/dev/null 2>&1; then
    printf 'sync-brand: python3 (3.11+, for tomllib) is required\n' >&2
    exit 1
fi

if [[ "$icons" == 1 ]]; then
    if [[ "$(uname -s)" != Darwin ]]; then
        printf 'sync-brand: --icons needs macOS (swift, sips, iconutil)\n' >&2
        exit 1
    fi
    artwork="$repo_root/assets/brand/icon-artwork.jpg"
    [[ -f "$artwork" ]] || { printf 'sync-brand: missing %s\n' "$artwork" >&2; exit 1; }
    work=$(mktemp -d)
    trap 'rm -rf -- "$work"' EXIT
    swift "$repo_root/scripts/render-macos-icon.swift" "$artwork" \
        "$repo_root/assets/brand/icon-1024.png" "$repo_root/packaging/macos/assets/AppIcon.icns"
    swift "$repo_root/scripts/render-menu-bar-icon.swift" \
        "$repo_root/assets/brand/menubar-template.pdf" >/dev/null
    sips -z 512 512 "$repo_root/assets/brand/icon-1024.png" \
        --out "$repo_root/crates/maild-ui/icons/icon.png" >/dev/null
    sips -z 256 256 "$repo_root/assets/brand/icon-1024.png" --out "$work/icon-256.png" >/dev/null
    sips -z 32 32 "$repo_root/assets/brand/icon-1024.png" --out "$work/icon-32.png" >/dev/null
    python3 -I - "$repo_root/crates/maild-ui/icons/icon.ico" "$work/icon-32.png" "$work/icon-256.png" <<'PY'
import struct, sys

out, *pngs = sys.argv[1:]
images = []
for path in pngs:
    data = open(path, "rb").read()
    width, height = struct.unpack(">II", data[16:24])
    images.append((width, height, data))
header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries, blobs = b"", b""
for width, height, data in images:
    entries += struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(data), offset)
    blobs += data
    offset += len(data)
with open(out, "wb") as handle:
    handle.write(header + entries + blobs)
PY
    rm -rf -- "$work"
    trap - EXIT
    printf 'sync-brand: re-rendered icons from %s\n' "assets/brand/icon-artwork.jpg"
fi

exec python3 -I - "$repo_root" "$mode" <<'PY'
import difflib
import json
import re
import shlex
import sys

if sys.version_info < (3, 11):
    sys.exit("sync-brand: python3 >= 3.11 is required (tomllib)")
import tomllib
from pathlib import Path

root = Path(sys.argv[1])
mode = sys.argv[2]

with open(root / "brand.toml", "rb") as handle:
    brand = tomllib.load(handle)

KEYS = [
    "product_name", "slug", "env_prefix",
    "cli_daemon", "cli_ctl", "launcher_bin", "agent_bin", "menu_bin", "ui_bin",
    "app_bundle_name", "agent_bundle_name", "menu_bundle_name", "ui_bundle_name", "icon_name",
    "app_bundle_id", "agent_bundle_id", "menu_bundle_id", "ui_bundle_id",
    "uri_scheme", "keychain_service",
]
errors = []
for key in KEYS:
    value = brand.get(key)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{key}: must be a non-empty string")
for key in sorted(set(brand) - set(KEYS)):
    errors.append(f"{key}: unknown key")


def check(key, pattern, what):
    value = brand.get(key)
    if isinstance(value, str) and not re.fullmatch(pattern, value):
        errors.append(f"{key} = {value!r}: {what}")


check("slug", r"[a-z][a-z0-9-]*", "lowercase letters, digits and '-'")
check("env_prefix", r"[A-Z][A-Z0-9_]*", "uppercase letters, digits and '_'")
for key in ("cli_daemon", "cli_ctl", "launcher_bin", "agent_bin", "menu_bin", "ui_bin"):
    check(key, r"[a-z][a-z0-9_-]*", "a Cargo binary name (lowercase, digits, '-', '_')")
for key in ("app_bundle_name", "agent_bundle_name", "menu_bundle_name", "ui_bundle_name"):
    check(key, r"[^/\x00-\x1f\"\\]+\.app", "a bundle file name ending in .app")
check("product_name", r"[^\x00-\x1f\"\\]+", "printable, no quotes or backslashes")
check("icon_name", r"[A-Za-z0-9 ._-]+", "a plain file stem")
for key in ("app_bundle_id", "agent_bundle_id", "menu_bundle_id", "ui_bundle_id"):
    check(key, r"[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+", "a reverse-DNS bundle identifier")
check("uri_scheme", r"[a-z][a-z0-9+.-]*", "an RFC 3986 URI scheme")
check("keychain_service", r"[A-Za-z0-9._-]+", "letters, digits, '.', '_' and '-'")
if errors:
    sys.exit("sync-brand: invalid brand.toml\n  " + "\n  ".join(errors))

b = brand
env = b["env_prefix"]
GENERATED = "Generated by scripts/sync-brand.sh from brand.toml. Do not edit by hand."

# --------------------------------------------------------------------------- derived values
consts = [
    ("PRODUCT_NAME", b["product_name"], "User-visible product name."),
    ("SLUG", b["slug"], "Lowercase slug for directories and file stems."),
    ("ENV_PREFIX", env, "Prefix of every production environment variable."),
    ("CLI_DAEMON", b["cli_daemon"], "Daemon binary / CLI name."),
    ("CLI_CTL", b["cli_ctl"], "Control CLI binary name."),
    ("LAUNCHER_BIN", b["launcher_bin"], "Native launcher binary (the app bundle executable)."),
    ("AGENT_BIN", b["agent_bin"], "Agent binary."),
    ("MENU_BIN", b["menu_bin"], "Menu-bar companion binary."),
    ("UI_BIN", b["ui_bin"], "Default Tauri frontend binary."),
    ("APP_BUNDLE_NAME", b["app_bundle_name"], "Main macOS app bundle."),
    ("AGENT_BUNDLE_NAME", b["agent_bundle_name"], "Nested Agent bundle."),
    ("MENU_BUNDLE_NAME", b["menu_bundle_name"], "Nested menu-bar bundle."),
    ("UI_BUNDLE_NAME", b["ui_bundle_name"], "Nested default frontend bundle."),
    ("ICON_NAME", b["icon_name"], "Icon file stem (`<ICON_NAME>.icns`)."),
    ("APP_BUNDLE_ID", b["app_bundle_id"], "Main app bundle identifier."),
    ("AGENT_BUNDLE_ID", b["agent_bundle_id"], "Agent bundle identifier."),
    ("MENU_BUNDLE_ID", b["menu_bundle_id"], "Menu-bar bundle identifier."),
    ("UI_BUNDLE_ID", b["ui_bundle_id"], "Default frontend bundle identifier."),
    ("BUNDLED_FRONTEND_ID", b["ui_bundle_id"], "Frontend package id of the bundled default UI (= `UI_BUNDLE_ID`)."),
    ("URI_SCHEME", b["uri_scheme"], "Isolated mail reader URI scheme."),
    ("KEYCHAIN_SERVICE", b["keychain_service"], "Keychain service holding account credentials."),
    ("CONFIG_FILE_NAME", f"{b['slug']}.toml", "Configuration file name."),
    ("DATABASE_FILE_NAME", f"{b['slug']}.sqlite3", "SQLite database file name."),
    ("SOCKET_FILE_NAME", f"{b['slug']}.sock", "Local API socket file name."),
    ("CONFIG_ENV", f"{env}_CONFIG", "Overrides the configuration file path."),
    ("SOCKET_ENV", f"{env}_SOCKET", "Overrides the local API socket path."),
    ("URL_ENV", f"{env}_URL", "Remote API URL."),
    ("TOKEN_ENV", f"{env}_TOKEN", "API bearer token."),
    ("TOKEN_FILE_ENV", f"{env}_TOKEN_FILE", "File holding the API bearer token."),
    ("PLUGIN_TOKEN_ENV", f"{env}_PLUGIN_TOKEN", "Plugin API token."),
    ("PLUGIN_TOKEN_FILE_ENV", f"{env}_PLUGIN_TOKEN_FILE", "File holding the plugin API token."),
    ("PLUGIN_DATA_ENV", f"{env}_PLUGIN_DATA_DIR", "Per-plugin data directory."),
    ("MAINTENANCE_LOCK_FD_ENV", f"{env}_MAINTENANCE_LOCK_FD", "Inherited maintenance lock descriptor."),
    ("MAINTENANCE_VALIDATOR_ENV", f"{env}_MAINTENANCE_VALIDATOR", "Maintenance validator handshake."),
    ("BUNDLE_SHORT_VERSION_ENV", f"{env}_BUNDLE_SHORT_VERSION", "Packaging: CFBundleShortVersionString."),
    ("BUNDLE_BUILD_VERSION_ENV", f"{env}_BUNDLE_BUILD_VERSION", "Packaging: CFBundleVersion."),
    ("UI_ORIGIN_ENV", f"{env}_UI_ORIGIN", "Dev-time origin of the default frontend webview."),
    ("AGENT_CONTRACT_PREFIX", "maild-agent-bundle-id:", "Stable binary-scan marker before the Agent bundle id."),
    ("MENU_CONTRACT_PREFIX", "maild-menu-bundle-id:", "Stable binary-scan marker before the menu bundle id."),
    ("UI_EMBEDDED_CONTRACT_PREFIX", "maild-ui-embedded:", "Stable binary-scan marker before the embedded frontend id."),
]
values = dict((name, value) for name, value, _ in consts)


def rust_str(value):
    out = []
    for ch in value:
        if ch in '"\\':
            out.append("\\" + ch)
        elif ch == "\0":
            out.append("\\0")
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            out.append(f"\\u{{{ord(ch):x}}}")
        else:
            out.append(ch)
    return '"' + "".join(out) + '"'


def toml_str(value):
    return json.dumps(value, ensure_ascii=False)


# --------------------------------------------------------------------------- generated.rs
rs = [f"// @generated {GENERATED}", "//", "// Brand constants and compile-time literal macros for every workspace crate.", ""]
for name, value, doc in consts:
    rs.append(f"/// {doc}")
    rs.append(f"pub const {name}: &str = {rust_str(value)};")
rs.append("")
macros = [
    ("bin_exe_daemon", f'env!({rust_str("CARGO_BIN_EXE_" + b["cli_daemon"])})',
     "Path of the daemon binary in this package's integration tests (`CARGO_BIN_EXE_<cli_daemon>`)."),
    ("bin_exe_ctl", f'env!({rust_str("CARGO_BIN_EXE_" + b["cli_ctl"])})',
     "Path of the control CLI in this package's integration tests (`CARGO_BIN_EXE_<cli_ctl>`)."),
    ("bin_exe_agent", f'env!({rust_str("CARGO_BIN_EXE_" + b["agent_bin"])})',
     "Path of the Agent binary in this package's integration tests (`CARGO_BIN_EXE_<agent_bin>`)."),
    ("agent_id_contract", rust_str("\0" + values["AGENT_CONTRACT_PREFIX"] + b["agent_bundle_id"] + "\0"),
     "NUL-delimited Agent bundle-id marker that packaging scans compiled binaries for."),
    ("menu_id_contract", rust_str("\0" + values["MENU_CONTRACT_PREFIX"] + b["menu_bundle_id"] + "\0"),
     "NUL-delimited menu bundle-id marker that packaging scans compiled binaries for."),
    ("ui_embedded_contract", rust_str("\0" + values["UI_EMBEDDED_CONTRACT_PREFIX"] + b["ui_bundle_id"] + "\0"),
     "NUL-delimited embedded-frontend marker that packaging scans compiled binaries for."),
]
for name, body, doc in macros:
    rs += [f"/// {doc}", "#[macro_export]", f"macro_rules! {name} {{", f"    () => {{", f"        {body}", "    };", "}", ""]
generated_rs = "\n".join(rs).rstrip("\n") + "\n"

# --------------------------------------------------------------------------- brand.ts
ts_fields = [
    ("productName", b["product_name"]),
    ("slug", b["slug"]),
    ("cliDaemon", b["cli_daemon"]),
    ("cliCtl", b["cli_ctl"]),
    ("bundledFrontendId", b["ui_bundle_id"]),
    ("appBundleName", b["app_bundle_name"]),
    ("uriScheme", b["uri_scheme"]),
]
brand_ts = f"// @generated {GENERATED}\n\nexport const BRAND = {{\n"
brand_ts += "".join(f"  {k}: {json.dumps(v, ensure_ascii=False)},\n" for k, v in ts_fields)
brand_ts += "} as const;\n"

# --------------------------------------------------------------------------- brand.env
env_fields = [
    ("BRAND_PRODUCT_NAME", b["product_name"]),
    ("BRAND_SLUG", b["slug"]),
    ("BRAND_ENV_PREFIX", env),
    ("BRAND_CLI_DAEMON", b["cli_daemon"]),
    ("BRAND_CLI_CTL", b["cli_ctl"]),
    ("BRAND_LAUNCHER_BIN", b["launcher_bin"]),
    ("BRAND_AGENT_BIN", b["agent_bin"]),
    ("BRAND_MENU_BIN", b["menu_bin"]),
    ("BRAND_UI_BIN", b["ui_bin"]),
    ("BRAND_APP_BUNDLE", b["app_bundle_name"]),
    ("BRAND_AGENT_BUNDLE", b["agent_bundle_name"]),
    ("BRAND_MENU_BUNDLE", b["menu_bundle_name"]),
    ("BRAND_UI_BUNDLE", b["ui_bundle_name"]),
    ("BRAND_ICON_NAME", b["icon_name"]),
    ("BRAND_APP_BUNDLE_ID", b["app_bundle_id"]),
    ("BRAND_AGENT_BUNDLE_ID", b["agent_bundle_id"]),
    ("BRAND_MENU_BUNDLE_ID", b["menu_bundle_id"]),
    ("BRAND_UI_BUNDLE_ID", b["ui_bundle_id"]),
]
brand_env = f"# shellcheck shell=bash\n# {GENERATED}\n"
brand_env += "".join(f"{k}={shlex.quote(v)}\n" for k, v in env_fields)

# --------------------------------------------------------------------------- patched files
problems = []


def read(rel):
    return (root / rel).read_text(encoding="utf-8")


def patch_tauri(rel):
    text = read(rel)
    conf = json.loads(text)
    conf["productName"] = b["product_name"]
    conf["identifier"] = b["ui_bundle_id"]
    csp = conf["app"]["security"]["csp"]
    new_csp, count = re.subn(r"(frame-src\s+)[A-Za-z][A-Za-z0-9+.-]*:", lambda m: m.group(1) + b["uri_scheme"] + ":", csp)
    if count != 1:
        problems.append(f"{rel}: expected exactly one `frame-src <scheme>:` in the CSP, found {count}")
    conf["app"]["security"]["csp"] = new_csp
    return json.dumps(conf, indent=2, ensure_ascii=False) + "\n"


def patch_frontend_manifest(rel):
    text = read(rel)
    # Only the top-level table (before the first `[section]`) carries the identity keys.
    match = re.search(r"^\[", text, re.M)
    head, tail = (text[: match.start()], text[match.start():]) if match else (text, "")
    wanted = {
        "id": b["ui_bundle_id"],
        "name": b["product_name"],
        # Not part of the dev manifest's schema today (unknown keys are rejected); synced only
        # if a future manifest schema adds them.
        "macos_app": b["ui_bundle_name"],
        "executable": f"bin/{b['ui_bin']}",
    }
    for key, value in wanted.items():
        pattern = re.compile(rf"^{key}\s*=\s*\"[^\"\n]*\"[ \t]*$", re.M)
        found = len(pattern.findall(head))
        if found > 1 or (found == 0 and key in ("id", "name")):
            problems.append(f"{rel}: expected one top-level `{key} = \"...\"` line, found {found}")
            continue
        head = pattern.sub(lambda _m: f"{key} = {toml_str(value)}", head)
    return head + tail


BEGIN = "# BEGIN GENERATED BRAND BINS"
END = "# END GENERATED BRAND BINS"


def patch_bins(rel, bins):
    text = read(rel)
    if text.count(BEGIN) != 1 or text.count(END) != 1 or text.index(BEGIN) > text.index(END):
        problems.append(f"{rel}: needs exactly one `{BEGIN}` .. `{END}` block")
        return text
    lines = [BEGIN, f"# {GENERATED}"]
    for name, path in bins:
        lines += ["[[bin]]", f"name = {toml_str(name)}", f"path = {toml_str(path)}", ""]
    lines[-1] = END
    start = text.index(BEGIN)
    end = text.index(END) + len(END)
    return text[:start] + "\n".join(lines) + text[end:]


outputs = {
    "crates/maild-brand/src/generated.rs": generated_rs,
    "packages/maild-ui/src/brand.ts": brand_ts,
    "packaging/macos/brand.env": brand_env,
    "packaging/macos/app-bundle-id.txt": b["app_bundle_id"] + "\n",
    "packaging/macos/agent-bundle-id.txt": b["agent_bundle_id"] + "\n",
    "packaging/macos/menu-bundle-id.txt": b["menu_bundle_id"] + "\n",
    "crates/maild-ui/tauri.conf.json": patch_tauri("crates/maild-ui/tauri.conf.json"),
    "crates/maild-ui/package/frontend.toml": patch_frontend_manifest("crates/maild-ui/package/frontend.toml"),
    "crates/maild/Cargo.toml": patch_bins("crates/maild/Cargo.toml", [
        (b["cli_daemon"], "src/bin/maild.rs"),
        (b["agent_bin"], "src/bin/maild-agent.rs"),
        (b["cli_ctl"], "src/bin/maildctl.rs"),
    ]),
    "crates/maild-launcher/Cargo.toml": patch_bins("crates/maild-launcher/Cargo.toml", [(b["launcher_bin"], "src/main.rs")]),
    "crates/maild-menu/Cargo.toml": patch_bins("crates/maild-menu/Cargo.toml", [(b["menu_bin"], "src/main.rs")]),
    "crates/maild-ui/Cargo.toml": patch_bins("crates/maild-ui/Cargo.toml", [(b["ui_bin"], "src/main.rs")]),
}

if problems:
    sys.exit("sync-brand: cannot sync\n  " + "\n  ".join(problems))

drift = []
for rel, new in outputs.items():
    path = root / rel
    old = path.read_text(encoding="utf-8") if path.exists() else None
    if old == new:
        continue
    drift.append(rel)
    if mode == "check":
        sys.stdout.writelines(difflib.unified_diff(
            (old or "").splitlines(keepends=True), new.splitlines(keepends=True),
            fromfile=f"a/{rel}", tofile=f"b/{rel}"))
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(new, encoding="utf-8")
        print(f"sync-brand: updated {rel}")

if mode == "check":
    if drift:
        print(f"\nsync-brand: {len(drift)} file(s) out of sync with brand.toml; run `bash scripts/sync-brand.sh`", file=sys.stderr)
        sys.exit(1)
    print(f"sync-brand: {len(outputs)} derived files match brand.toml ({b['product_name']})")
elif not drift:
    print(f"sync-brand: already in sync ({len(outputs)} derived files)")
PY
