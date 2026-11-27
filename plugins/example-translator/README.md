# Example Translation Provider Plugin

Minimal runnable `translation_provider` plugin for Featherpost (ADR-0050).

## What it demonstrates

- `plugin.toml` with `schema_version = 2`, `protocol_version = 1`, `capabilities = ["translation_provider"]`, and `[l10n."zh-Hans"]`.
- Reading the real daemon-injected environment variables:
  - `FEATHERPOST_PLUGIN_TOKEN`
  - `FEATHERPOST_PLUGIN_DATA_DIR`
  - `FEATHERPOST_CONFIG`
- Connecting over same-UID local streaming IPC, calling `authenticate` and `translationProviderRegister`.
- Non-blocking read/write split so `translation/cancel { request_id }` and concurrent batches can be handled while a slow `translation/translate` request is still in flight.

## Build & package layout

```sh
cargo build -p featherpost-example-translator
mkdir -p /tmp/example-translator-pkg/bin
cp plugins/example-translator/plugin.toml /tmp/example-translator-pkg/plugin.toml
cp target/debug/example-translator /tmp/example-translator-pkg/bin/example-translator
chmod 700 /tmp/example-translator-pkg /tmp/example-translator-pkg/bin /tmp/example-translator-pkg/bin/example-translator
chmod 600 /tmp/example-translator-pkg/plugin.toml
```

Then install and enable it through the control CLI:

```sh
featherpostctl plugin install --source /tmp/example-translator-pkg
featherpostctl plugin enable org.featherpost.example-translator
```

For `lifecycle = "independent"` development, omit `executable`, run `featherpostctl plugin-token org.featherpost.example-translator`, and export `FEATHERPOST_PLUGIN_TOKEN` before launching `example-translator`.
