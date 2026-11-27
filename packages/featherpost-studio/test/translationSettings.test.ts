import { applyEffectiveLocales } from "@featherpost/i18n";
import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import { BridgeError } from "../src/bridge/errors";
import {
  BUILTIN_OPENAI_PROVIDER_ID,
  type TranslationProbeResult,
  type UiTranslationProviderEntry,
} from "../src/bridge/translation";
import { buildCommandPaletteCatalog } from "../src/commandPaletteCatalog";
import { Host } from "../src/host/host";
import { permissionLabel } from "../src/i18n";
import { MailboxController } from "../src/mailbox/controller";
import { Extensions } from "../src/settings/Extensions";
import { Translation } from "../src/settings/Translation.tsx";
import { SettingsController } from "../src/settings/controller";
import { settingsModule } from "../src/settings/module";
import {
  buildOpenAiUpdate,
  canReuseSavedApiKey,
  inspectTranslationEndpoint,
  requiresApiKeyRebind,
  validateOpenAiDraft,
} from "../src/settings/translation";
import { FakeBridge, deferred, flush, online } from "./fakeBridge";
import { FakeSettings, preview } from "./fakeSettings";
import {
  defaultBuiltinEntry,
  defaultTranslationPreference,
} from "./fakeTranslation";

const CJK = /[㐀-鿿豈-﫿　-〿＀-￯]/;

afterEach(() => {
  applyEffectiveLocales([]);
});

function externalEntry(
  overrides: Partial<UiTranslationProviderEntry> = {},
): UiTranslationProviderEntry {
  return {
    id: "com.example.deepl",
    kind: "external",
    name: "DeepL Translator",
    description: "External DeepL plugin",
    l10n: {
      "zh-CN": { name: "DeepL 翻译插件", description: "通过外部插件调用 DeepL" },
      en: { name: "DeepL Plugin", description: "Translate via DeepL plugin" },
    },
    version: "1.2.0",
    lifecycle: "supervised",
    enabled: true,
    state: "running",
    has_ui: true,
    registered: true,
    configured: true,
    proxy_ready: true,
    generation: 3,
    supported_languages: ["zh-CN", "en", "ja"],
    max_batch_items: 20,
    max_batch_chars: 10000,
    max_concurrency: 2,
    supports_cancel: true,
    variant_revision: "deepl-v2",
    ...overrides,
  };
}

function setup(configure?: (bridge: FakeSettings) => void, isOnline = true) {
  const bridge = new FakeSettings();
  configure?.(bridge);
  const controller = new SettingsController(bridge);
  controller.connection(
    isOnline ? online : { ...online, phase: "disconnected", connection_epoch: 0 },
  );
  return { bridge, controller };
}

function capture(render: () => ReactNode) {
  let root: ReactNode;
  function Probe() {
    root = render();
    return root;
  }
  const html = renderToStaticMarkup(createElement(Probe));
  const elements: ReactElement<Record<string, unknown>>[] = [];
  const visit = (node: ReactNode) => {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node || typeof node !== "object" || !("props" in node)) return;
    const el = node as ReactElement<Record<string, unknown>>;
    elements.push(el);
    visit(el.props.children as ReactNode);
  };
  visit(root!);
  return { html, elements };
}

describe("Settings › Translation registration, order, and Command Palette", () => {
  it("registers maild.settings.translation at order 85 between Network (80) and Connection (90)", () => {
    const host = new Host(new FakeBridge());
    const bundle = settingsModule(host);
    try {
      const sorted = [...(bundle.settings ?? [])].sort(
        (a, b) => (a.order ?? 0) - (b.order ?? 0),
      );
      const ids = sorted.map((s) => s.id);
      const netIdx = ids.indexOf("maild.settings.network");
      const trIdx = ids.indexOf("maild.settings.translation");
      const connIdx = ids.indexOf("maild.settings.connection");

      expect(trIdx).toBeGreaterThan(netIdx);
      expect(connIdx).toBeGreaterThan(trIdx);
      expect(sorted[trIdx]?.order).toBe(85);

      const section = bundle.settings!.find(
        (s) => s.id === "maild.settings.translation",
      )!;
      expect(section.title).toBe("翻译");
      applyEffectiveLocales(["en"]);
      expect(section.title).toBe("Translation");
    } finally {
      bundle.dispose?.();
      host.stop();
    }
  });

  it("exposes Settings › Translation with bilingual keywords in Command Palette", () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const mailbox = new MailboxController(bridge);
    const settings = new SettingsController(bridge.settings);
    const bundle = settingsModule(host, settings);
    const opened: string[] = [];
    try {
      host.registerBuiltin(bundle);
      const catalog = buildCommandPaletteCatalog({
        host,
        mailbox,
        view: "standard",
        focusSearch: () => {},
        focusSidebarFilter: () => {},
        changeView: () => {},
        openMailbox: () => {},
        openSettingsSection: (id) => opened.push(id),
      });
      const nav = catalog.groups.find((g) => g.id === "navigation")!;
      const item = nav.items.find(
        (entry) =>
          entry.id === "studio.palette.settings-section:maild.settings.translation",
      );
      expect(item).toBeDefined();
      expect(item?.keywords).toContain("翻译");
      expect(item?.keywords).toContain("Translation");
      expect(item?.keywords).toContain("Ollama");
      item!.onSelect();
      expect(opened).toEqual(["maild.settings.translation"]);
    } finally {
      bundle.dispose?.();
      host.stop();
    }
  });
});

describe("Separated local preferences vs daemon provider loading", () => {
  it("loads local preferences and cache stats even when daemon is offline or listTranslationProviders fails", async () => {
    const { bridge, controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        display_mode: "translated_only",
        target_lang: "en",
        openai_compatible: {
          preset: "ollama",
          base_url: "http://127.0.0.1:11434/v1",
          normalized_identity: "http://127.0.0.1:11434/v1",
          transport: "loopback_http",
          plaintext_lan_transport: false,
          model: "qwen2.5:7b",
          has_api_key: false,
          bound_service_identity: null,
          allow_plaintext_lan: false,
        },
      });
      b.translation.providersError = new BridgeError({
        code: "offline",
        reason: "offline",
        message: "offline",
      });
    }, false);

    try {
      controller.activate("maild.settings.translation");
      await flush();

      const s = controller.store.getSnapshot();
      expect(s.online).toBe(false);
      expect(s.error).toBeNull();
      expect(s.translationPreference?.display_mode).toBe("translated_only");
      expect(s.translationDraft?.openaiModel).toBe("qwen2.5:7b");
      expect(s.translationCacheStats?.entry_count).toBe(12);
      expect(s.translationProvidersLoaded).toBe(true);
      expect(s.translationProvidersError).toBeTruthy();

      // Local edits and saves still work while offline.
      controller.editTranslation({ displayMode: "bilingual" });
      expect(controller.translationDirty).toBe(true);
      await controller.saveTranslation();
      expect(bridge.translation.savedUpdates).toHaveLength(1);
      expect(bridge.translation.savedUpdates[0]!.display_mode).toBe("bilingual");
      expect(controller.translationDirty).toBe(false);
    } finally {
      controller.dispose();
    }
  });

  it("handles older daemon (external_supported: false) cleanly while keeping built-in available", async () => {
    const { controller } = setup((b) => {
      b.translation.providersResponse = {
        catalog_generation: 0,
        external_supported: false,
        providers: [defaultBuiltinEntry()],
      };
    });
    try {
      controller.activate("maild.settings.translation");
      await flush();

      const s = controller.store.getSnapshot();
      expect(s.translationExternalSupported).toBe(false);
      expect(s.translationProviders).toHaveLength(1);
      const { html } = capture(() => createElement(Translation, { controller }));
      expect(html).toContain("当前后台版本不支持外部翻译扩展");
    } finally {
      controller.dispose();
    }
  });
});

describe("Hot-update translation/providers merging without clobbering drafts", () => {
  it("merges by monotonic catalog_generation and never overwrites unsaved form drafts", async () => {
    const { bridge, controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        openai_compatible: {
          preset: "openai",
          base_url: "https://api.openai.com/v1",
          normalized_identity: "https://api.openai.com:443/v1",
          transport: "public_https",
          plaintext_lan_transport: false,
          model: "gpt-4o-mini",
          has_api_key: true,
          bound_service_identity: "https://api.openai.com:443/v1",
          allow_plaintext_lan: false,
        },
      });
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();

      // User starts editing unsaved draft.
      controller.editTranslation({
        openaiModel: "gpt-4.1-mini",
        openaiApiKey: "sk-unsaved-secret",
      });
      expect(controller.translationDirty).toBe(true);

      // Newer provider hot-update arrives (generation 2).
      const ext = externalEntry({ generation: 2 });
      bridge.translation.pushProviders({
        catalog_generation: 2,
        external_supported: true,
        providers: [defaultBuiltinEntry({ configured: true }), ext],
      });

      let s = controller.store.getSnapshot();
      expect(s.translationCatalogGeneration).toBe(2);
      expect(s.translationProviders.map((p) => p.id)).toEqual([
        BUILTIN_OPENAI_PROVIDER_ID,
        ext.id,
      ]);
      // Unsaved draft MUST remain intact!
      expect(s.translationDraft?.openaiModel).toBe("gpt-4.1-mini");
      expect(s.translationDraft?.openaiApiKey).toBe("sk-unsaved-secret");

      // Stale older generation (1 < 2) MUST be ignored.
      bridge.translation.pushProviders({
        catalog_generation: 1,
        external_supported: true,
        providers: [defaultBuiltinEntry()],
      });
      s = controller.store.getSnapshot();
      expect(s.translationCatalogGeneration).toBe(2);
      expect(s.translationProviders).toHaveLength(2);
    } finally {
      controller.dispose();
    }
  });
});

describe("Normalized-endpoint credential binding, clear+set guard, and LAN HTTP notice", () => {
  it("normalizes endpoints identically to Rust and enforces credential rebind on Base URL changes", () => {
    const savedPref = defaultTranslationPreference({
      active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      openai_compatible: {
        preset: "custom",
        base_url: "https://gw.example.com/team-a/v1",
        normalized_identity: "https://gw.example.com:443/team-a/v1",
        transport: "public_https",
        plaintext_lan_transport: false,
        model: "gpt-4o-mini",
        has_api_key: true,
        bound_service_identity: "https://gw.example.com:443/team-a/v1",
        allow_plaintext_lan: false,
      },
    });

    // Same endpoint (even with trailing /chat/completions or explicit :443) reuses saved key.
    expect(
      inspectTranslationEndpoint("https://GW.EXAMPLE.COM:443/team-a/v1/chat/completions"),
    ).toMatchObject({
      ok: true,
      canonicalKey: "https://gw.example.com:443/team-a/v1",
      allowsKeyless: false,
      isPlaintextLan: false,
    });

    const sameUrlDraft = {
      activeProviderId: BUILTIN_OPENAI_PROVIDER_ID,
      displayMode: "bilingual" as const,
      targetLang: "auto",
      detectBannerEnabled: true,
      autoTranslate: false,
      autoTranslateProviderId: null,
      ignoredLanguages: [],
      openaiPreset: "custom" as const,
      openaiBaseUrl: "https://gw.example.com/team-a/v1/",
      openaiModel: "gpt-4o-mini",
      openaiApiKey: "",
      openaiClearApiKey: false,
      openaiAllowPlaintextLan: false,
    };
    expect(canReuseSavedApiKey(sameUrlDraft, savedPref)).toBe(true);
    expect(requiresApiKeyRebind(sameUrlDraft, savedPref)).toBe(false);
    expect(validateOpenAiDraft(sameUrlDraft, savedPref)).toBeNull();

    // Changing tenant path /team-a/v1 -> /team-b/v1 MUST require new API key!
    const changedPathDraft = {
      ...sameUrlDraft,
      openaiBaseUrl: "https://gw.example.com/team-b/v1",
    };
    expect(canReuseSavedApiKey(changedPathDraft, savedPref)).toBe(false);
    expect(requiresApiKeyRebind(changedPathDraft, savedPref)).toBe(true);
    expect(validateOpenAiDraft(changedPathDraft, savedPref)).toBeTruthy();

    // Switching to keyless loopback Ollama does NOT require a key and does NOT flag rebind.
    const ollamaDraft = {
      ...sameUrlDraft,
      openaiPreset: "ollama" as const,
      openaiBaseUrl: "http://127.0.0.1:11434/v1",
      openaiModel: "qwen2.5:7b",
    };
    expect(requiresApiKeyRebind(ollamaDraft, savedPref)).toBe(false);
    expect(canReuseSavedApiKey(ollamaDraft, savedPref)).toBe(false);
    expect(validateOpenAiDraft(ollamaDraft, savedPref)).toBeNull();

    // Simultaneous clear_api_key + non-empty api_key MUST be rejected.
    const clearAndSet = {
      ...sameUrlDraft,
      openaiApiKey: "sk-new-key",
      openaiClearApiKey: true,
    };
    expect(validateOpenAiDraft(clearAndSet, savedPref)).toBeTruthy();
    expect(buildOpenAiUpdate(clearAndSet)).toMatchObject({
      api_key: "sk-new-key",
      clear_api_key: false,
    });

    // Plaintext LAN HTTP requires explicit confirmation; public HTTP is rejected.
    const lanDraft = {
      ...sameUrlDraft,
      openaiBaseUrl: "http://192.168.1.50:8080/v1",
      openaiApiKey: "sk-lan-key",
      openaiAllowPlaintextLan: false,
    };
    expect(inspectTranslationEndpoint(lanDraft.openaiBaseUrl)).toMatchObject({
      ok: true,
      transport: "lan_http",
      isPlaintextLan: true,
    });
    expect(validateOpenAiDraft(lanDraft, savedPref)).toBeTruthy();
    expect(
      validateOpenAiDraft({ ...lanDraft, openaiAllowPlaintextLan: true }, savedPref),
    ).toBeNull();

    expect(inspectTranslationEndpoint("http://api.openai.com/v1")).toEqual({
      ok: false,
      reason: "public_http_forbidden",
    });
  });

  it("never pre-fills saved API key, blocks simultaneous clear+set in controller, and shows rebind & LAN notices in UI", async () => {
    const { controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        openai_compatible: {
          preset: "openai",
          base_url: "https://api.openai.com/v1",
          normalized_identity: "https://api.openai.com:443/v1",
          transport: "public_https",
          plaintext_lan_transport: false,
          model: "gpt-4o-mini",
          has_api_key: true,
          bound_service_identity: "https://api.openai.com:443/v1",
          allow_plaintext_lan: false,
        },
      });
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();

      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe("");
      expect(controller.translationDirty).toBe(false);

      // Clear key, then typing a new key automatically unsets clearApiKey.
      controller.clearSavedTranslationApiKey();
      expect(controller.store.getSnapshot().translationDraft?.openaiClearApiKey).toBe(
        true,
      );
      controller.editTranslation({ openaiApiKey: "sk-replacement" });
      expect(controller.store.getSnapshot().translationDraft?.openaiClearApiKey).toBe(
        false,
      );

      // Change Base URL away from saved endpoint without key -> UI renders rebind notice, NOT "Saved (leave empty to keep)".
      controller.editTranslation({
        openaiBaseUrl: "https://api.deepseek.com/v1",
        openaiApiKey: "",
      });
      let html = capture(() => createElement(Translation, { controller })).html;
      expect(html).toContain("不会沿用旧 API 密钥");
      expect(html).not.toContain("已保存（留空以继续沿用）");

      // Enter plaintext LAN HTTP endpoint -> UI renders calm LAN transport notice & checkbox.
      controller.editTranslation({
        openaiBaseUrl: "http://192.168.1.20:11434/v1",
      });
      html = capture(() => createElement(Translation, { controller })).html;
      expect(html).toContain("局域网明文 HTTP");
    } finally {
      controller.dispose();
    }
  });
});

describe("Dirty guard, selectSection, discard, close confirmation, epoch, stale probe, and key cleanup", () => {
  it("wires translationDirty into dirty, selectSection leave-guard, beforeClose, discard, and wipes draft API key", async () => {
    const { controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        openai_compatible: {
          preset: "ollama",
          base_url: "http://127.0.0.1:11434/v1",
          normalized_identity: "http://127.0.0.1:11434/v1",
          transport: "loopback_http",
          plaintext_lan_transport: false,
          model: "qwen2.5:7b",
          has_api_key: false,
          bound_service_identity: null,
          allow_plaintext_lan: false,
        },
      });
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();
      expect(controller.dirty).toBe(false);
      expect(controller.translationDirty).toBe(false);

      // Edit model + enter API key -> dirty becomes true.
      controller.editTranslation({
        openaiModel: "llama3.2",
        openaiApiKey: "sk-ephemeral-draft-key",
      });
      expect(controller.translationDirty).toBe(true);
      expect(controller.dirty).toBe(true);

      // selectSection is guarded and opens confirmation dialog.
      expect(controller.selectSection("maild.settings.extensions")).toBe(false);
      expect(controller.store.getSnapshot().confirm).not.toBeNull();
      expect(controller.store.getSnapshot().section).toBe(
        "maild.settings.translation",
      );

      // Cancelling keeps us on translation with edits intact.
      controller.cancelConfirmation();
      expect(controller.store.getSnapshot().section).toBe(
        "maild.settings.translation",
      );
      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe(
        "sk-ephemeral-draft-key",
      );

      // beforeClose also prompts confirmation when translationDirty is true.
      const closePromise = controller.beforeClose();
      expect(controller.store.getSnapshot().confirm).not.toBeNull();
      controller.cancelConfirmation();
      expect(await closePromise).toBe(false);

      // Confirming leave discards draft AND clears API key from memory!
      expect(controller.selectSection("maild.settings.extensions")).toBe(false);
      controller.confirm();
      expect(controller.store.getSnapshot().section).toBe(
        "maild.settings.extensions",
      );
      expect(controller.translationDirty).toBe(false);
      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe(
        "",
      );
      expect(controller.store.getSnapshot().translationDraft?.openaiModel).toBe(
        "qwen2.5:7b",
      );
    } finally {
      controller.dispose();
    }
  });

  it("wipes draft API key on save failure, connection_epoch change, and dispose", async () => {
    const { bridge, controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        openai_compatible: {
          preset: "openai",
          base_url: "https://api.openai.com/v1",
          normalized_identity: "https://api.openai.com:443/v1",
          transport: "public_https",
          plaintext_lan_transport: false,
          model: "gpt-4o-mini",
          has_api_key: false,
          bound_service_identity: null,
          allow_plaintext_lan: false,
        },
      });
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();

      // 1. Save failure clears in-memory API key.
      bridge.translation.saveError = new BridgeError({
        code: "conflict",
        reason: "translation_revision_conflict",
        message: "conflict",
      });
      controller.editTranslation({ openaiApiKey: "sk-secret-on-fail" });
      await controller.saveTranslation();
      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe(
        "",
      );
      expect(controller.store.getSnapshot().error).toBeTruthy();

      // 2. connection_epoch change clears in-memory API key.
      controller.editTranslation({ openaiApiKey: "sk-secret-on-epoch" });
      controller.connection({ ...online, connection_epoch: 2 });
      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe(
        "",
      );

      // 3. dispose clears in-memory API key and unsubscribes provider listener.
      controller.editTranslation({ openaiApiKey: "sk-secret-on-dispose" });
      expect(bridge.translation.providerHandlers.size).toBe(1);
      controller.dispose();
      expect(controller.store.getSnapshot().translationDraft?.openaiApiKey).toBe(
        "",
      );
      expect(bridge.translation.providerHandlers.size).toBe(0);
    } finally {
      controller.dispose();
    }
  });

  it("cancels and discards stale external probe results when plugin is disabled, switched, or epoch changes", async () => {
    const ext = externalEntry({ generation: 5 });
    const { bridge, controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: ext.id,
      });
      b.translation.providersResponse = {
        catalog_generation: 2,
        external_supported: true,
        providers: [defaultBuiltinEntry(), ext],
      };
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();

      // Case A: Plugin gets disabled mid-probe via hot-update -> stale response ignored.
      const pendingA = deferred<TranslationProbeResult>();
      bridge.translation.testTranslationProvider = async (req) => {
        bridge.translation.testRequests.push(req);
        return pendingA.promise;
      };
      const runA = controller.testTranslationProvider();
      expect(controller.store.getSnapshot().translationTesting).toBe(true);

      bridge.translation.pushProviders({
        catalog_generation: 3,
        external_supported: true,
        providers: [
          defaultBuiltinEntry(),
          { ...ext, enabled: false, registered: false, state: "disabled" },
        ],
      });
      expect(controller.store.getSnapshot().translationTesting).toBe(false);

      pendingA.resolve({
        ok: true,
        stage: "done",
        reason: "ok",
        latency_ms: 42,
        route: "ipc://daemon",
        plaintext_lan_transport: false,
      });
      await runA;
      expect(controller.store.getSnapshot().translationTestResult).toBeNull();

      // Re-enable plugin at generation 6.
      bridge.translation.pushProviders({
        catalog_generation: 4,
        external_supported: true,
        providers: [
          defaultBuiltinEntry(),
          { ...ext, enabled: true, registered: true, state: "running", generation: 6 },
        ],
      });

      // Case B: Switching provider mid-probe discards stale response.
      const pendingB = deferred<TranslationProbeResult>();
      bridge.translation.testTranslationProvider = async () => pendingB.promise;
      const runB = controller.testTranslationProvider();
      expect(controller.store.getSnapshot().translationTesting).toBe(true);

      controller.selectTranslationProvider(BUILTIN_OPENAI_PROVIDER_ID);
      expect(controller.store.getSnapshot().translationTesting).toBe(false);

      pendingB.resolve({
        ok: true,
        stage: "done",
        reason: "ok",
        latency_ms: 99,
        route: "ipc://daemon",
        plaintext_lan_transport: false,
      });
      await runB;
      expect(controller.store.getSnapshot().translationTestResult).toBeNull();
    } finally {
      controller.dispose();
    }
  });
});

describe("Auto-translate provider binding, built-in draft probe, external UI open, and cache clear", () => {
  it("pauses auto-translate on provider switch and requires explicit re-confirmation for the new provider", async () => {
    const ext = externalEntry();
    const { bridge, controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        auto_translate: true,
        auto_translate_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        openai_compatible: {
          preset: "ollama",
          base_url: "http://127.0.0.1:11434/v1",
          normalized_identity: "http://127.0.0.1:11434/v1",
          transport: "loopback_http",
          plaintext_lan_transport: false,
          model: "qwen2.5:7b",
          has_api_key: false,
          bound_service_identity: null,
          allow_plaintext_lan: false,
        },
      });
      b.translation.providersResponse = {
        catalog_generation: 1,
        external_supported: true,
        providers: [defaultBuiltinEntry({ configured: true }), ext],
      };
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();

      expect(controller.store.getSnapshot().translationDraft?.autoTranslate).toBe(
        true,
      );

      // Switching provider pauses autoTranslate and raises re-confirmation notice.
      controller.selectTranslationProvider(ext.id);
      let s = controller.store.getSnapshot();
      expect(s.translationDraft?.autoTranslate).toBe(false);
      expect(s.translationDraft?.autoTranslateProviderId).toBeNull();
      expect(s.translationAutoSwitchNotice).toBe(true);

      const html = capture(() => createElement(Translation, { controller })).html;
      expect(html).toContain("已自动暂停自动翻译");
      expect(html).toContain("DeepL 翻译插件");
      expect(html).toContain("打开插件设置");

      // Re-confirming binds auto_translate_provider_id to the new provider.
      controller.toggleTranslationAutoTranslate(true);
      await controller.saveTranslation();
      expect(bridge.translation.savedUpdates[0]).toMatchObject({
        active_provider_id: ext.id,
        auto_translate: true,
        auto_translate_provider_id: ext.id,
      });

      // Open external plugin settings reuses packageAction({ action: "open", id }).
      await controller.openProvider(ext.id);
      expect(bridge.packageWrites).toContainEqual({
        action: "open",
        id: ext.id,
      });
    } finally {
      controller.dispose();
    }
  });

  it("tests unsaved built-in draft values, resolves auto target language, and clears cache with confirmation", async () => {
    const { bridge, controller } = setup();
    try {
      controller.activate("maild.settings.translation");
      await flush();

      controller.selectTranslationProvider(BUILTIN_OPENAI_PROVIDER_ID);
      controller.applyTranslationPreset("ollama");
      controller.editTranslation({ openaiModel: "qwen2.5:14b" });
      await controller.testTranslationProvider();

      expect(bridge.translation.testRequests).toHaveLength(1);
      expect(bridge.translation.testRequests[0]).toMatchObject({
        provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        target_lang: "zh-CN",
        draft_openai: {
          preset: "ollama",
          base_url: "http://127.0.0.1:11434/v1",
          model: "qwen2.5:14b",
          api_key: null,
          clear_api_key: false,
        },
      });
      expect(controller.store.getSnapshot().translationTestResult?.ok).toBe(true);

      // Add & remove never-translate languages (case-insensitive dedupe).
      expect(controller.addIgnoredTranslationLanguage("ja")).toBe(true);
      expect(controller.addIgnoredTranslationLanguage("JA")).toBe(true);
      expect(controller.addIgnoredTranslationLanguage("bad tag!")).toBe(false);
      expect(
        controller.store.getSnapshot().translationDraft?.ignoredLanguages,
      ).toEqual(["ja"]);
      controller.removeIgnoredTranslationLanguage("ja");
      expect(
        controller.store.getSnapshot().translationDraft?.ignoredLanguages,
      ).toEqual([]);

      // Clear translation cache goes through confirmation dialog.
      bridge.translation.clearReclaimSucceeded = false;
      controller.confirmClearTranslationCache();
      expect(controller.store.getSnapshot().confirm?.danger).toBe(true);
      controller.confirm();
      await flush();
      expect(bridge.translation.clearCalls).toBe(1);
      expect(controller.store.getSnapshot().translationCacheStats?.entry_count).toBe(
        0,
      );
      expect(controller.store.getSnapshot().notice).toContain("12");
    } finally {
      controller.dispose();
    }
  });

  it("renders English UI with zero CJK characters and renders unhealthy fail-closed alert", async () => {
    applyEffectiveLocales(["en"]);
    const { controller } = setup((b) => {
      b.translation.preference = defaultTranslationPreference({
        state_healthy: false,
      });
      b.translation.providersResponse = {
        catalog_generation: 1,
        external_supported: true,
        providers: [defaultBuiltinEntry(), externalEntry()],
      };
    });

    try {
      controller.activate("maild.settings.translation");
      await flush();
      const { html } = capture(() => createElement(Translation, { controller }));
      expect(html).toContain("Email translation");
      expect(html).toContain("DeepL Plugin");
      expect(html).toContain("Open plugin settings");
      expect(html).toContain("corrupted");
      expect(CJK.test(html)).toBe(false);
    } finally {
      controller.dispose();
    }
  });
});

describe("Extension install preview translation_provider capability i18n", () => {
  it("localizes translation_provider in zh-CN and en in permissionLabel and Extensions install preview", () => {
    expect(permissionLabel("translation_provider")).toBe("提供邮件翻译服务");

    const controller = new SettingsController(new FakeSettings());
    controller.store.patch({
      choice: {
        token: "tok-tr",
        source: "/tmp/deepl-plugin",
        preview: {
          ...preview(),
          id: "com.example.deepl",
          name: "DeepL Translator",
          capabilities: ["translation_provider"],
          permissions: [],
          has_ui: true,
        },
      },
    });
    const zhHtml = renderToStaticMarkup(
      createElement(Extensions, { controller }),
    );
    expect(zhHtml).toContain("提供邮件翻译服务");
    expect(zhHtml).toContain("<code>translation_provider</code>");

    applyEffectiveLocales(["en"]);
    expect(permissionLabel("translation_provider")).toBe(
      "Provide email translation",
    );
    const enHtml = renderToStaticMarkup(
      createElement(Extensions, { controller }),
    );
    expect(enHtml).toContain("Provide email translation");
    expect(enHtml).toContain("<code>translation_provider</code>");
    expect(CJK.test(enHtml)).toBe(false);
    controller.dispose();
  });
});
