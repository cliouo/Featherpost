# Featherpost Studio

The sole built-in React frontend for Featherpost (`dev.featherpost.studio`, see [ADR-0049](../../docs/decisions/0049-retire-legacy-ui-studio-sole-builtin.md)). Third-party pluggable frontends remain supported through the managed frontend catalog.

Studio owns the shell, navigation, inbox, search, message list, reading layout, toolbar, onboarding, settings, compose, and typed daemon bridge without changing the mail engine or its authorization boundary.

## Build on macOS

Studio is built and bundled directly into `Featherpost.app` (single-Dock host + recovery payload):

```sh
pnpm install --frozen-lockfile
packaging/macos/build-universal.sh --output "$PWD/dist/macos/Featherpost.app"
```

## Development

```sh
pnpm --dir packages/featherpost-studio dev
# In another terminal, use the existing daemon after registering crates/maild-ui/package:
cargo run -p maild-ui
```

For an isolated seeded daemon, run the following, then pass its printed `--socket` argument to `cargo run -p maild-ui -- --socket <path>`. Production data is not required. The fixture includes offline cached mail and a local fake SMTP/IMAP account.

```sh
cargo run -p maild-ui --example dev_stack
```

## Design reference

The source inbox placed global search and operations above the list/reader, with account folders in the app rail. Studio separates app navigation from list filtering and message operations, gives each message a sender/subject/preview hierarchy, and uses a separate reading surface. The structural reference is [Plain's thread workspace](https://mobbin.com/screens/6b03543c-d8bd-403c-a3eb-3c43783a0a23), a support inbox rather than a consumer mail client. Its list/reading separation is the example; the palette, Chinese copy, mail entities, and local-first behavior remain Featherpost's. No reference imagery is bundled.

The user requested a from-scratch frontend rather than design options, so implementation proceeds with one direction rather than a separate mockup selection round.
