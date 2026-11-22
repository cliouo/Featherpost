# Featherpost Studio

A new React presentation for Featherpost, installed as `dev.featherpost.studio` alongside the original `dev.featherpost.ui`. The original UI, recovery payload, and default macOS app build remain unchanged.

Studio owns the shell, navigation, inbox, search, message list, reading layout, toolbar, and onboarding. It shares the existing typed daemon bridge, state controllers, secure HTML document renderer, account forms, draft editor, and package-management controls. This is not a rewrite of the mail engine or its authorization boundary.

## Install on macOS

Open the existing Featherpost app, unpack the universal ZIP, and double-click **安装 Studio.command**. The installer registers the frontend, switches the default only after installation succeeds, then opens the managed application. **切回原版.command** restores the original default without removing either frontend or touching mail data.

```sh
pnpm install --frozen-lockfile
bash packaging/macos/build-studio.sh
```

Output: `dist/studio/featherpost-studio-macos-universal.zip`. Requires the repository-pinned Rust toolchain and both macOS targets. The archive includes the CLI, which older DMGs do not provide. It is ad-hoc signed, not notarized.

## Development

```sh
pnpm --dir packages/featherpost-studio dev
# In another terminal, use the existing daemon after registering the Studio package:
TAURI_CONFIG="$(cat crates/maild-ui/tauri.studio.conf.json)" cargo run -p maild-ui --features studio
```

For an isolated seeded daemon, run the following, then pass its printed `--socket` argument to the UI command above. Production data is not required. The fixture includes offline cached mail and a local fake SMTP/IMAP account.

```sh
TAURI_CONFIG="$(cat crates/maild-ui/tauri.studio.conf.json)" cargo run -p maild-ui --example dev_stack --features studio
```

## Design reference

The source inbox placed global search and operations above the list/reader, with account folders in the app rail. Studio separates app navigation from list filtering and message operations, gives each message a sender/subject/preview hierarchy, and uses a separate reading surface. The structural reference is [Plain's thread workspace](https://mobbin.com/screens/6b03543c-d8bd-403c-a3eb-3c43783a0a23), a support inbox rather than a consumer mail client. Its list/reading separation is the example; the palette, Chinese copy, mail entities, and local-first behavior remain Featherpost's. No reference imagery is bundled.

The user requested a from-scratch frontend rather than design options, so implementation proceeds with one direction rather than a separate mockup selection round.
