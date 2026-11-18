# NodeSeek Email provider for Featherpost

This is a community-maintained adapter. It is not an official NodeSeek product and does not imply
NodeSeek endorsement.

The package contains three Rust binaries:

- `nodeseek-email-provider`: the supervised Featherpost transport provider;
- `nodeseek-email-ui`: the default native management window opened from Featherpost's Plugins page;
- `nodeseek-email`: the optional companion CLI for automation and recovery.

Production requests are pinned to `https://seek.li/openapi/v1`, do not use environment proxies,
do not follow redirects, and have fixed time and response-size limits. Upstream OpenAPI
specification: <https://seek.li/api/openapi.json> (keep any offline snapshot under gitignored
`reference/`, not in the repository). The NodeSeek API key is held
only in macOS Keychain. It is never accepted through argv, an environment variable, a config file,
or provider JSON-RPC.

## Build a package

From this directory:

```sh
sh scripts/package.sh
```

The installable directory is written to `dist/nodeseek-email/` with this layout:

```text
plugin.toml
bin/nodeseek-email-provider
bin/nodeseek-email-ui
bin/nodeseek-email
```

Packaging uses a temporary exact-layout stage. Existing `dist`, package, and `bin` directories must
be owned by the current user, must not be symlinks or group/other writable, and an old package must
contain only the three entries above. Unexpected residue fails closed instead of being copied or
discarded. All three binaries are built in the same locked release invocation.
On macOS, Featherpost preserves any `com.apple.quarantine` attribute while staging the package, so a
downloaded build remains subject to Gatekeeper when its management UI is opened.
The native window stack is isolated behind the crate's `ui` feature; the package script enables it
explicitly, while headless provider and CLI builds do not require GUI dependencies.

The release build reuses the workspace `target/` directory, so ordinary `cargo clean` reclaims it;
the script does not leave a second plugin-local target tree behind.

Install it through the generic external-plugin transaction:

```sh
featherpostctl plugin install --source dist/nodeseek-email
```

After installation open Featherpost, choose `插件`, then choose `管理` on NodeSeek Email. Configure the
API Key, view quota and mailboxes, create either `@seek.li` or `@nodeseek.org` addresses, and connect
an existing mailbox to Featherpost from that window. The key stays in the plugin's macOS Keychain item and
never crosses Featherpost RPC. CLI usage below is optional and kept for automation and recovery.

After upgrading from a pre-0.1.4 package, open the management window and save the API Key once more.
The credential item is recreated with the provider trusted at creation time. Background reads never
modify Keychain ACLs or request interactive authorization.

## Configure and inspect

Interactive setup uses a no-echo `/dev/tty` prompt:

```sh
NODESEEK_CLI=dist/nodeseek-email/bin/nodeseek-email
"$NODESEEK_CLI" configure
"$NODESEEK_CLI" key-status
"$NODESEEK_CLI" mailbox list
"$NODESEEK_CLI" quota
```

Automation may pipe exactly one line; putting the key on the command line or in the environment is
intentionally unsupported:

```sh
secret-producing-command | "$NODESEEK_CLI" configure --api-key-stdin
```

Remote mailbox creation is never retried and requires an explicit mutation acknowledgement:

```sh
"$NODESEEK_CLI" mailbox check desired@seek.li
"$NODESEEK_CLI" mailbox create desired@seek.li --confirm
```

Enable the plugin and add a Featherpost provider account. Its first receive/send establishes the
exact-address local mapping automatically:

```sh
featherpostctl plugin enable app.maild.provider.nodeseek-email
featherpostctl account provider-add --display-name NodeSeek --address desired@seek.li \
  --provider app.maild.provider.nodeseek-email
```

`mailbox import` remains available for an existing live Featherpost account. Before writing provider
state it connects through the platform-managed daemon, then requires an exact account ID, address,
and NodeSeek incoming/outgoing provider match; placeholder or archived accounts fail closed.
Provider transport requests use the same typed DTOs on both sides of the host/plugin boundary.

Featherpost account removal deliberately retains plugin state. Before reusing the same remote address,
archive the old account and then release only its local binding:

```sh
featherpostctl account remove acct_old
"$NODESEEK_CLI" mailbox unimport acct_old desired@seek.li --confirm
```

`unimport` never reads Keychain or contacts NodeSeek. It rejects Featherpost config, API endpoint/token,
and plugin-data environment overrides, then discovers both the daemon and private state from the
same platform-managed paths. The authenticated local Featherpost API must report the exact account ID as
archived. The requested canonical address must match the persisted plugin binding; that binding is
authoritative even if the archived profile was migrated to another provider or address. Only then
does one local transaction remove the binding and cascade that account's synthetic-UID mapping.
There is no offline destructive bypass: start or repair the managed daemon if verification is
unavailable. Host create/update also serialize and reject duplicate provider/address pairs before
probe, while a draft probe rejects a stale local owner. Existing-account probes check the current
binding or resolve an unbound remote address without importing it. Binding creation remains in
receive/send after account save, so cancelling an editor cannot reserve a mailbox.

There is deliberately no remote-delete command. Removing the package or account, and running
`unimport`, never deletes a NodeSeek mailbox.

## Tests

```sh
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

All normal tests use a loopback fake HTTP server and never require a real credential. The optional
live test is compiled only with `--features live-tests`, remains `#[ignore]`, reads its credential
from Keychain, and performs only mailbox-list and quota reads when
`NODESEEK_LIVE_READ_ONLY=1` is set.
