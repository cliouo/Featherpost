// @vitest-environment jsdom
import { applyEffectiveLocales } from "@featherpost/i18n";
import { act, createElement as h, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { BridgeError } from "../src/bridge/errors";
import {
  BUILTIN_OPENAI_PROVIDER_ID,
  type UiHtmlTranslationResult,
  type UiPlainTranslationResult,
  type UiTranslationProviderEntry,
} from "../src/bridge/translation";
import type { UiMessage } from "../src/bridge/types";
import { buildCommandPaletteCatalog } from "../src/commandPaletteCatalog";
import { ComposeController } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { Store } from "../src/host/store";
import { studioMailbox, MAIL } from "../src/mailbox";
import { MailboxController } from "../src/mailbox/controller";
import { DocumentController } from "../src/mailbox/document";
import {
  TranslationController,
  shouldShowDetectionBanner,
} from "../src/mailbox/translationController";
import { ReadingPane } from "../src/ReadingPane";
import { SettingsController } from "../src/settings/controller";
import { SETTINGS_VIEW, settingsModule } from "../src/settings/module";
import { buildShortcutCatalog, STUDIO_SHORTCUT_KEYS } from "../src/shortcuts";
import { StudioActionsContext } from "../src/studioActions";
import { StudioTooltipProvider } from "../src/ui";
import { Workspace } from "../src/Workspace";
import {
  FakeBridge,
  deferred,
  document as doc,
  flush,
  folder,
  message,
  online,
  summary,
} from "./fakeBridge";
import {
  defaultBuiltinEntry,
  defaultTranslationPreference,
} from "./fakeTranslation";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

beforeAll(() => {
  if (typeof globalThis.matchMedia !== "function") {
    globalThis.matchMedia = () =>
      ({
        matches: false,
        media: "",
        onchange: null,
        addListener() {},
        removeListener() {},
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: () => false,
      }) as MediaQueryList;
  }
  if (typeof globalThis.ResizeObserver === "undefined") {
    globalThis.ResizeObserver = class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];
const cleanups: Array<() => void> = [];

function mount(node: ReactNode) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  mountedRoots.push({ root, container });
  return { container };
}

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  while (cleanups.length) {
    cleanups.pop()!();
  }
  applyEffectiveLocales([]);
  document.body.innerHTML = "";
});

function configuredBuiltinPreference(
  overrides: Partial<ReturnType<typeof defaultTranslationPreference>> = {},
) {
  return defaultTranslationPreference({
    revision: 1,
    active_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
    display_mode: "bilingual",
    target_lang: "zh-CN",
    detect_banner_enabled: true,
    auto_translate: false,
    auto_translate_provider_id: null,
    ignored_languages: [],
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
    ...overrides,
  });
}

function readyBuiltinEntry(
  overrides: Partial<UiTranslationProviderEntry> = {},
): UiTranslationProviderEntry {
  return defaultBuiltinEntry({
    state: "ready",
    configured: true,
    proxy_ready: true,
    generation: 1,
    variant_revision: "openai-v1",
    ...overrides,
  });
}

function externalEntry(
  overrides: Partial<UiTranslationProviderEntry> = {},
): UiTranslationProviderEntry {
  return {
    id: "org.example.deepl",
    kind: "external",
    name: "DeepL Plugin",
    description: "External DeepL",
    version: "1.0.0",
    lifecycle: "supervised",
    enabled: true,
    state: "running",
    has_ui: true,
    registered: true,
    configured: true,
    proxy_ready: true,
    generation: 3,
    supported_languages: ["*"],
    max_batch_items: 16,
    max_batch_chars: 12_000,
    max_concurrency: 2,
    supports_cancel: true,
    variant_revision: "deepl-v1",
    ...overrides,
  };
}

function htmlMail(uid: string, extra: Partial<UiMessage> = {}): UiMessage {
  return message("INBOX", uid, {
    subject: `Hello Subject ${uid}`,
    has_html: true,
    html_bytes: 256,
    ...extra,
  });
}

function plainMail(uid: string, extra: Partial<UiMessage> = {}): UiMessage {
  return message("INBOX", uid, {
    subject: `Plain Subject ${uid}`,
    body: `First paragraph ${uid}\n\nSecond paragraph ${uid}`,
    has_html: false,
    html_bytes: 0,
    ...extra,
  });
}

async function setupControllerHarness(configured = true) {
  const bridge = new FakeBridge();
  if (configured) {
    bridge.settings.translation.preference = configuredBuiltinPreference();
    bridge.settings.translation.providersResponse = {
      catalog_generation: 1,
      external_supported: true,
      providers: [readyBuiltinEntry()],
    };
  }
  const hostState = new Store({
    activeViewId: MAIL,
    snapshot: online,
  });
  const mailboxStore = new Store<{ detail: UiMessage | null }>({ detail: null });
  const documents = new DocumentController(bridge);
  const stopDocs = documents.bindTo(mailboxStore);
  const translation = new TranslationController(bridge);
  const stopTr = translation.bindTo(mailboxStore, documents.store, hostState);
  await translation.start();

  cleanups.push(() => {
    stopTr();
    stopDocs();
    translation.stop();
    documents.stop();
  });

  return { bridge, hostState, mailboxStore, documents, translation };
}

describe("TranslationController & ReadingPane minimalist translation UX (PR 5)", () => {
  it("supports single-button toggle: translate → cancel → translate → show original → instant cached restore with zero outbound request", async () => {
    const { bridge, mailboxStore, translation } = await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    // 1. Open HTML email
    mailboxStore.patch({ detail: htmlMail("1") });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-1"));
    await flush();

    // Detection banner should offer translation
    expect(shouldShowDetectionBanner(translation.store.getSnapshot())).toBe(true);

    // 2. Start translation and hold response pending
    const firstPending = deferred<UiHtmlTranslationResult>();
    vi.spyOn(tr, "translateHtmlDocument").mockImplementationOnce((req) => {
      tr.htmlRequests.push(structuredClone(req));
      return firstPending.promise;
    });

    const firstRun = translation.toggleTranslate();
    await flush();
    expect(translation.store.getSnapshot().phase).toBe("translating");

    // Emit progress with job_id
    tr.pushProgress({
      job_id: "job-cancel-1",
      token: "tok-1",
      completed_batches: 1,
      total_batches: 3,
      status: "running",
    });
    expect(translation.store.getSnapshot().progress).toEqual({
      jobId: "job-cancel-1",
      completedBatches: 1,
      totalBatches: 3,
    });

    // Click same single button while translating -> cancels immediately
    await translation.toggleTranslate();
    expect(tr.cancelledJobs).toEqual(["job-cancel-1"]);
    expect(translation.store.getSnapshot().phase).toBe("idle");

    // Late resolution of cancelled job is cleanly dropped
    firstPending.resolve({
      job_id: "job-cancel-1",
      token: "tok-1",
      rev: 1,
      bilingual_url: "featherpost-mail://localhost/doc/tok-1?translate=bilingual&rev=1",
      translated_url: "featherpost-mail://localhost/doc/tok-1?translate=translated&rev=1",
      bilingual_dark_url:
        "featherpost-mail://localhost/doc/tok-1?translate=bilingual&rev=1&appearance=dark",
      translated_dark_url:
        "featherpost-mail://localhost/doc/tok-1?translate=translated&rev=1&appearance=dark",
      translated_subject: "迟到译文",
      source_lang: "en",
      target_lang: "zh-CN",
      provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      provider_generation: 1,
      from_cache: false,
    });
    await firstRun;
    await flush();
    expect(translation.store.getSnapshot().phase).toBe("idle");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();

    // 3. Click again -> translates cleanly
    await translation.toggleTranslate();
    expect(tr.htmlRequests).toHaveLength(2);
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(translation.store.getSnapshot().htmlResult?.token).toBe("tok-1");

    // 4. Click again -> toggles back to original while retaining cached result
    await translation.toggleTranslate();
    expect(translation.store.getSnapshot().phase).toBe("original");
    expect(translation.store.getSnapshot().htmlResult?.token).toBe("tok-1");

    // 5. Click again -> instant restore with ZERO new outbound translation request!
    await translation.toggleTranslate();
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(tr.htmlRequests).toHaveLength(2);
  });

  it("hides detection banner when unconfigured, shows it when configured, supports ignore language and dismiss", async () => {
    // Unconfigured provider -> banner MUST be hidden completely (zero noise)
    const unconfigured = await setupControllerHarness(false);
    unconfigured.mailboxStore.patch({ detail: htmlMail("1") });
    unconfigured.bridge.prepareRequests[0]!.d.resolve(doc("tok-u1"));
    await flush();
    expect(unconfigured.bridge.settings.translation.detectRequests).toHaveLength(0);
    expect(shouldShowDetectionBanner(unconfigured.translation.store.getSnapshot())).toBe(
      false,
    );

    // Configured provider -> banner shown, dismiss & ignore work
    const { bridge, mailboxStore, translation } = await setupControllerHarness(true);
    mailboxStore.patch({ detail: htmlMail("2") });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-2"));
    await flush();
    expect(shouldShowDetectionBanner(translation.store.getSnapshot())).toBe(true);

    // Dismiss hides for current message
    translation.dismissBanner();
    expect(shouldShowDetectionBanner(translation.store.getSnapshot())).toBe(false);

    // Open another email -> banner shows again, clicking "ignore language" persists to ignored_languages
    mailboxStore.patch({ detail: htmlMail("3") });
    bridge.prepareRequests[1]!.d.resolve(doc("tok-3"));
    await flush();
    expect(shouldShowDetectionBanner(translation.store.getSnapshot())).toBe(true);

    await translation.ignoreDetectedLanguage();
    expect(bridge.settings.translation.savedUpdates).toHaveLength(1);
    expect(bridge.settings.translation.savedUpdates[0]!.ignored_languages).toEqual([
      "en",
    ]);
    // Preserves saved OpenAI key by passing openai_compatible: null
    expect(bridge.settings.translation.savedUpdates[0]!.openai_compatible).toBeNull();
    expect(shouldShowDetectionBanner(translation.store.getSnapshot())).toBe(false);
  });

  it("only auto-translates when auto_translate is enabled AND bound to the active configured provider", async () => {
    const { bridge, mailboxStore, translation } = await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    // Auto-translate enabled, but bound to a DIFFERENT provider ID -> MUST NOT auto-translate!
    translation.applyPreference(
      configuredBuiltinPreference({
        revision: 2,
        auto_translate: true,
        auto_translate_provider_id: "org.example.other",
      }),
    );
    mailboxStore.patch({ detail: plainMail("1") });
    await flush();
    expect(tr.plainRequests).toHaveLength(0);

    // Now bound to the active provider -> auto-translates!
    translation.applyPreference(
      configuredBuiltinPreference({
        revision: 3,
        auto_translate: true,
        auto_translate_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      }),
    );
    mailboxStore.patch({ detail: plainMail("2") });
    await flush();
    expect(tr.plainRequests).toHaveLength(1);
    expect(translation.store.getSnapshot().phase).toBe("translated");

    // Ignored language -> MUST NOT auto-translate!
    translation.applyPreference(
      configuredBuiltinPreference({
        revision: 4,
        auto_translate: true,
        auto_translate_provider_id: BUILTIN_OPENAI_PROVIDER_ID,
        ignored_languages: ["en"],
      }),
    );
    mailboxStore.patch({ detail: plainMail("3") });
    await flush();
    expect(tr.plainRequests).toHaveLength(1);
  });

  it("handles A→B→A rapid switching by cancelling in-flight work and dropping late responses", async () => {
    const { bridge, mailboxStore, translation } = await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    const pendingA = deferred<UiHtmlTranslationResult>();
    vi.spyOn(tr, "translateHtmlDocument").mockImplementationOnce((req) => {
      tr.htmlRequests.push(structuredClone(req));
      return pendingA.promise;
    });

    // Open A and begin translating
    mailboxStore.patch({ detail: htmlMail("A") });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-A1"));
    await flush();

    const runA = translation.translate(false);
    await flush();
    tr.pushProgress({
      job_id: "job-A1",
      token: "tok-A1",
      completed_batches: 0,
      total_batches: 2,
      status: "running",
    });

    // Switch A -> B
    mailboxStore.patch({ detail: htmlMail("B") });
    bridge.prepareRequests[1]!.d.resolve(doc("tok-B1"));
    await flush();
    expect(tr.cancelledJobs).toEqual(["job-A1"]);

    // Switch B -> A (DocumentController issues fresh token tok-A2)
    mailboxStore.patch({ detail: htmlMail("A") });
    bridge.prepareRequests[2]!.d.resolve(doc("tok-A2"));
    await flush();

    // Old job-A1 progress & final result arrive late -> MUST be ignored!
    tr.pushProgress({
      job_id: "job-A1",
      token: "tok-A1",
      completed_batches: 2,
      total_batches: 2,
      status: "running",
    });
    pendingA.resolve({
      job_id: "job-A1",
      token: "tok-A1",
      rev: 1,
      bilingual_url: "featherpost-mail://localhost/doc/tok-A1?translate=bilingual&rev=1",
      translated_url: "featherpost-mail://localhost/doc/tok-A1?translate=translated&rev=1",
      bilingual_dark_url:
        "featherpost-mail://localhost/doc/tok-A1?translate=bilingual&rev=1&appearance=dark",
      translated_dark_url:
        "featherpost-mail://localhost/doc/tok-A1?translate=translated&rev=1&appearance=dark",
      translated_subject: "Stale A",
      source_lang: "en",
      target_lang: "zh-CN",
      provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      provider_generation: 1,
      from_cache: false,
    });
    await runA;
    await flush();

    expect(translation.store.getSnapshot().documentToken).toBe("tok-A2");
    expect(translation.store.getSnapshot().phase).toBe("idle");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();
  });

  it("handles HTML→plain-text switching cleanly (including DocumentController generation bump and zero HTML injection)", async () => {
    const { bridge, mailboxStore, documents, translation } =
      await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    // 1. Open HTML mail (still preparing) -> switch immediately to plain-text mail
    mailboxStore.patch({ detail: htmlMail("H1") });
    expect(bridge.prepareRequests).toHaveLength(1);
    mailboxStore.patch({ detail: plainMail("P1") });

    // Stale HTML prepare resolves late -> DocumentController drops & releases it
    bridge.prepareRequests[0]!.d.resolve(doc("tok-stale-H1"));
    await flush();
    expect(bridge.released).toEqual(["tok-stale-H1"]);
    expect(documents.store.getSnapshot().document).toBeNull();
    expect(translation.store.getSnapshot().contentKind).toBe("plain");
    expect(translation.store.getSnapshot().documentToken).toBeNull();

    // 2. Translate plain-text message; stale HTML progress must be ignored
    const pendingPlain = deferred<UiPlainTranslationResult>();
    vi.spyOn(tr, "translatePlainTextMessage").mockImplementationOnce((req) => {
      tr.plainRequests.push(structuredClone(req));
      return pendingPlain.promise;
    });
    const runPlain = translation.translate(false);
    await flush();

    // Stale HTML progress event arrives while translating plain-text -> ignored!
    tr.pushProgress({
      job_id: "job-stale-html",
      token: "tok-stale-H1",
      completed_batches: 1,
      total_batches: 1,
      status: "running",
    });
    expect(translation.store.getSnapshot().progress?.jobId).toBeNull();

    // Valid plain-text progress (token: null) updates progress
    tr.pushProgress({
      job_id: "job-plain-1",
      token: null,
      completed_batches: 1,
      total_batches: 2,
      status: "running",
    });
    expect(translation.store.getSnapshot().progress).toEqual({
      jobId: "job-plain-1",
      completedBatches: 1,
      totalBatches: 2,
    });

    pendingPlain.resolve({
      job_id: "job-plain-1",
      original_subject: "Plain Subject P1",
      translated_subject: "纯文本主题 <img src=x onerror=alert(1)>",
      blocks: [
        {
          id: 1,
          original: "First paragraph P1",
          translated: "第一段 <script>alert(1)</script>",
        },
      ],
      source_lang: "en",
      target_lang: "zh-CN",
      provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      provider_generation: 1,
      from_cache: false,
    });
    await runPlain;
    await flush();
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(translation.store.getSnapshot().plainResult?.blocks[0]?.translated).toBe(
      "第一段 <script>alert(1)</script>",
    );
  });

  it("cancels and discards translation on same-message HTML token rotation (remote-image allowOnce)", async () => {
    const { bridge, mailboxStore, documents, translation } =
      await setupControllerHarness(true);

    const mail = htmlMail("R1");
    mailboxStore.patch({ detail: mail });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-blocked", { remote_refs: 2 }));
    await flush();

    // Translate under tok-blocked
    await translation.translate(false);
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(translation.store.getSnapshot().htmlResult?.token).toBe("tok-blocked");

    // User clicks "Load remote images for this message" -> DocumentController rotates token!
    documents.allowOnce(mail);
    expect(bridge.released).toEqual(["tok-blocked"]);
    expect(translation.store.getSnapshot().phase).toBe("idle");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();

    bridge.prepareRequests[1]!.d.resolve(
      doc("tok-allowed", { remote_refs: 2, allow_remote: true }),
    );
    await flush();
    expect(translation.store.getSnapshot().documentToken).toBe("tok-allowed");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();
  });

  it("redraws in-place with ZERO outbound translation requests when ONLY display_mode changes in Settings", async () => {
    const { bridge, hostState, mailboxStore, translation } =
      await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    // 1. Open and translate HTML email in bilingual mode
    mailboxStore.patch({ detail: htmlMail("M1") });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-M1"));
    await flush();
    await translation.translate(false);
    expect(tr.htmlRequests).toHaveLength(1);
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(translation.displayMode).toBe("bilingual");

    // 2. User navigates to Settings, changes ONLY display_mode -> translated_only, returns to MAIL
    hostState.patch({ activeViewId: SETTINGS_VIEW });
    tr.preference = configuredBuiltinPreference({
      revision: 2,
      display_mode: "translated_only",
    });
    hostState.patch({ activeViewId: MAIL });
    await flush();

    // Phase stays translated, displayMode updated, ZERO new outbound translate calls!
    expect(translation.store.getSnapshot().phase).toBe("translated");
    expect(translation.displayMode).toBe("translated_only");
    expect(translation.store.getSnapshot().htmlResult?.token).toBe("tok-M1");
    expect(tr.htmlRequests).toHaveLength(1);
  });

  it("discards late translation response when target language or provider / provider generation changes mid-flight", async () => {
    const { bridge, mailboxStore, translation } = await setupControllerHarness(true);
    const tr = bridge.settings.translation;

    // Use external plugin with generation 3
    tr.preference = defaultTranslationPreference({
      revision: 5,
      active_provider_id: "org.example.deepl",
      target_lang: "zh-CN",
    });
    tr.providersResponse = {
      catalog_generation: 2,
      external_supported: true,
      providers: [readyBuiltinEntry(), externalEntry({ generation: 3 })],
    };
    await translation.refreshState();

    mailboxStore.patch({ detail: htmlMail("G1") });
    bridge.prepareRequests[0]!.d.resolve(doc("tok-G1"));
    await flush();

    // 1. Mid-flight target language change discards late response
    const pendingTarget = deferred<UiHtmlTranslationResult>();
    vi.spyOn(tr, "translateHtmlDocument").mockImplementationOnce((req) => {
      tr.htmlRequests.push(structuredClone(req));
      return pendingTarget.promise;
    });
    const runTarget = translation.translate(false);
    await flush();
    tr.pushProgress({
      job_id: "job-old-target",
      token: "tok-G1",
      completed_batches: 0,
      total_batches: 1,
      status: "running",
    });

    // Change target_lang zh-CN -> ja
    translation.applyPreference(
      defaultTranslationPreference({
        revision: 6,
        active_provider_id: "org.example.deepl",
        target_lang: "ja",
      }),
    );
    expect(tr.cancelledJobs).toContain("job-old-target");

    pendingTarget.resolve({
      job_id: "job-old-target",
      token: "tok-G1",
      rev: 1,
      bilingual_url: "featherpost-mail://localhost/doc/tok-G1?translate=bilingual&rev=1",
      translated_url: "featherpost-mail://localhost/doc/tok-G1?translate=translated&rev=1",
      bilingual_dark_url:
        "featherpost-mail://localhost/doc/tok-G1?translate=bilingual&rev=1&appearance=dark",
      translated_dark_url:
        "featherpost-mail://localhost/doc/tok-G1?translate=translated&rev=1&appearance=dark",
      translated_subject: "Old target result",
      source_lang: "en",
      target_lang: "zh-CN",
      provider_id: "org.example.deepl",
      provider_generation: 3,
      from_cache: false,
    });
    await runTarget;
    await flush();
    expect(translation.store.getSnapshot().phase).toBe("idle");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();

    // 2. Mid-flight provider generation upgrade (gen 3 -> gen 4) cancels and discards late gen 3 result
    const pendingGen = deferred<UiHtmlTranslationResult>();
    vi.spyOn(tr, "translateHtmlDocument").mockImplementationOnce((req) => {
      tr.htmlRequests.push(structuredClone(req));
      return pendingGen.promise;
    });
    const runGen = translation.translate(false);
    await flush();
    tr.pushProgress({
      job_id: "job-old-gen",
      token: "tok-G1",
      completed_batches: 0,
      total_batches: 1,
      status: "running",
    });

    tr.pushProviders({
      catalog_generation: 3,
      external_supported: true,
      providers: [readyBuiltinEntry(), externalEntry({ generation: 4 })],
    });
    expect(tr.cancelledJobs).toContain("job-old-gen");

    pendingGen.resolve({
      job_id: "job-old-gen",
      token: "tok-G1",
      rev: 2,
      bilingual_url: "featherpost-mail://localhost/doc/tok-G1?translate=bilingual&rev=2",
      translated_url: "featherpost-mail://localhost/doc/tok-G1?translate=translated&rev=2",
      bilingual_dark_url:
        "featherpost-mail://localhost/doc/tok-G1?translate=bilingual&rev=2&appearance=dark",
      translated_dark_url:
        "featherpost-mail://localhost/doc/tok-G1?translate=translated&rev=2&appearance=dark",
      translated_subject: "Old gen result",
      source_lang: "en",
      target_lang: "ja",
      provider_id: "org.example.deepl",
      provider_generation: 3,
      from_cache: false,
    });
    await runGen;
    await flush();
    expect(translation.store.getSnapshot().phase).toBe("idle");
    expect(translation.store.getSnapshot().htmlResult).toBeNull();
  });

  it("renders ReadingPane minimalist 1-click UI: no mode dropdown, banner states, subject/plain-text modes, zero HTML injection, and guarded Settings navigation", async () => {
    const bridge = new FakeBridge();
    const tr = bridge.settings.translation;
    const host = new Host(bridge);
    const controller = new MailboxController(bridge);
    const documents = new DocumentController(bridge);
    const translation = new TranslationController(bridge);
    const composer = new ComposeController(bridge.compose, () => {}, () => {});

    const stopDocs = documents.bindTo(controller.store);
    const stopTr = translation.bindTo(controller.store, documents.store, host.state);
    await translation.start();

    cleanups.push(() => {
      stopTr();
      stopDocs();
      translation.stop();
      documents.stop();
      controller.stop();
      composer.stop();
      host.stop();
    });

    // Populate mailbox with one plain-text email
    controller.acceptSnapshot(online);
    for (const req of bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    const row = summary("INBOX", "1", "Quarterly Report");
    bridge.mailbox.queries[0]?.d.resolve({ messages: [row], total: 1 });
    await flush();
    controller.selectMessage(row);
    bridge.detailRequests[0]!.d.resolve(
      plainMail("1", {
        subject: "Quarterly Report",
        body: "Revenue grew 20%.",
      }),
    );
    await flush();

    const openedSections: string[] = [];
    const { container } = mount(
      h(
        StudioActionsContext.Provider,
        {
          value: {
            search() {},
            copy() {},
            openSettingsSection: (sectionId: string) =>
              openedSections.push(sectionId),
          },
        },
        h(
          StudioTooltipProvider,
          null,
          h(ReadingPane, {
            host,
            controller,
            documents,
            composer,
            translation,
          }),
        ),
      ),
    );

    // 1. Unconfigured: NO detection banner, single toolbar button visible without native title=, NO mode dropdown!
    expect(container.querySelector(".translation-banner")).toBeNull();
    expect(container.querySelectorAll("button.translate-button")).toHaveLength(1);
    const btn = container.querySelector<HTMLButtonElement>("button.translate-button")!;
    expect(btn.hasAttribute("title")).toBe(false);
    expect(btn.getAttribute("aria-pressed")).toBe("false");

    // Clicking toolbar button while unconfigured -> navigates via guarded openSettingsSection('maild.settings.translation') & shows error banner
    await act(async () => {
      btn.click();
      await flush();
    });
    expect(openedSections).toEqual(["maild.settings.translation"]);
    const errBanner = container.querySelector(".translation-banner.error")!;
    expect(errBanner).not.toBeNull();
    expect(errBanner.textContent).toContain("前往翻译设置");

    // Clicking "前往翻译设置" in error banner triggers guarded navigation again
    const openSettingsBtn = Array.from(errBanner.querySelectorAll("button")).find(
      (b) => b.textContent === "前往翻译设置",
    )!;
    act(() => {
      openSettingsBtn.click();
    });
    expect(openedSections).toEqual([
      "maild.settings.translation",
      "maild.settings.translation",
    ]);

    // 2. Configure provider -> detection banner appears
    tr.preference = configuredBuiltinPreference();
    tr.providersResponse = {
      catalog_generation: 2,
      external_supported: true,
      providers: [readyBuiltinEntry()],
    };
    await act(async () => {
      await translation.refreshState();
      await flush();
    });

    const detectBanner = container.querySelector(".translation-banner.detected")!;
    expect(detectBanner).not.toBeNull();
    expect(detectBanner.textContent).toContain("检测到 英语 邮件");

    // 3. Click 1-click translate in banner -> renders bilingual subject & plain-text via safe React text nodes (NO XSS!)
    vi.spyOn(tr, "translatePlainTextMessage").mockResolvedValueOnce({
      job_id: "tr-p1",
      original_subject: "Quarterly Report",
      translated_subject: "季度报告",
      blocks: [
        {
          id: 1,
          original: "Revenue grew 20%.",
          translated: "营收增长了 20%。<script>window.__xss=1</script>",
        },
      ],
      source_lang: "en",
      target_lang: "zh-CN",
      provider_id: BUILTIN_OPENAI_PROVIDER_ID,
      provider_generation: 1,
      from_cache: false,
    });

    const translateActionBtn = detectBanner.querySelector<HTMLButtonElement>(
      ".translation-banner-primary",
    )!;
    await act(async () => {
      translateActionBtn.click();
      await flush();
    });

    // Toolbar button is active (aria-pressed="true")
    expect(btn.getAttribute("aria-pressed")).toBe("true");
    // Compact translated banner with NO mode-switcher dropdown
    const doneBanner = container.querySelector(".translation-banner.translated")!;
    expect(doneBanner).not.toBeNull();
    expect(doneBanner.textContent).toContain("已翻译（英语 → 简体中文）");
    expect(doneBanner.textContent).toContain("显示原文");
    expect(doneBanner.textContent).toContain("重新翻译");
    expect(container.querySelector("select")).toBeNull();

    // Bilingual subject & plain-text rendered safely with zero script injection
    expect(container.querySelector(".letter-heading h2")?.textContent).toBe(
      "Quarterly Report",
    );
    expect(
      container.querySelector(".letter-subject-translation")?.textContent,
    ).toBe("季度报告");
    expect(container.querySelector(".letter-plain-original")?.textContent).toBe(
      "Revenue grew 20%.",
    );
    expect(container.querySelector(".letter-plain-translation")?.textContent).toBe(
      "营收增长了 20%。<script>window.__xss=1</script>",
    );
    expect(container.querySelector("script")).toBeNull();

    // 4. Switch ONLY display_mode to translated_only -> redraws in-place with ZERO outbound requests!
    act(() => {
      translation.applyPreference(
        configuredBuiltinPreference({
          revision: 2,
          display_mode: "translated_only",
        }),
      );
    });
    expect(container.querySelector(".letter-heading h2")?.textContent).toBe("季度报告");
    expect(container.querySelector(".letter-subject-translation")).toBeNull();
    expect(container.querySelector(".letter-plain-original")).toBeNull();
    expect(container.querySelector(".letter-plain-translation")?.textContent).toBe(
      "营收增长了 20%。<script>window.__xss=1</script>",
    );

    // 5. Simulate rate-limit 429 error -> error banner shows Retry button (not open_settings)
    vi.spyOn(tr, "translatePlainTextMessage").mockRejectedValueOnce(
      new BridgeError({
        code: "limit",
        reason: "translation_rate_limited",
        message: "rate limited",
      }),
    );
    const retranslateBtn = Array.from(doneBanner.querySelectorAll("button")).find(
      (b) => b.textContent === "重新翻译",
    )!;
    await act(async () => {
      retranslateBtn.click();
      await flush();
    });
    const retryBanner = container.querySelector(".translation-banner.error")!;
    expect(retryBanner).not.toBeNull();
    expect(retryBanner.textContent).toContain("重试");
  });

  it("wires ⌘K maild.mailbox.translate and maild.mailbox.retranslate in selection allowlist and Shift+T shortcut", async () => {
    const bridge = new FakeBridge();
    bridge.settings.translation.preference = configuredBuiltinPreference();
    bridge.settings.translation.providersResponse = {
      catalog_generation: 1,
      external_supported: true,
      providers: [readyBuiltinEntry()],
    };
    const host = new Host(bridge);
    const composer = new ComposeController(
      bridge.compose,
      (id) => host.openView(id),
      (text) => host.notify(text),
    );
    const settings = new SettingsController(bridge.settings, () => {});
    const openedSections: string[] = [];
    const mailbox = studioMailbox(
      host,
      composer,
      () => {},
      (sectionId) => openedSections.push(sectionId),
    );
    const bundles = [
      mailbox.bundle,
      composeModule(host, composer),
      settingsModule(host, settings),
    ];
    bundles.forEach((b) => host.registerBuiltin(b));
    host.finishAssembly();
    host.state.patch({ snapshot: online });

    cleanups.push(() => {
      bundles.forEach((b) => b.dispose?.());
      host.stop();
    });

    for (const req of bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    const row = summary("INBOX", "42", "Release Notes");
    bridge.mailbox.queries[0]?.d.resolve({ messages: [row], total: 1 });
    await flush();

    // Select message and resolve detail
    mailbox.controller.selectMessage(row);
    bridge.detailRequests[0]!.d.resolve(plainMail("42", { subject: "Release Notes" }));
    await flush();

    const catalog = buildCommandPaletteCatalog({
      host,
      mailbox: mailbox.controller,
      view: "standard",
      focusSearch: () => {},
      focusSidebarFilter: () => {},
      changeView: () => {},
      openMailbox: (fn) => fn(),
      openSettingsSection: (id) => openedSections.push(id),
    });

    const selectionGroup = catalog.groups.find((g) => g.id === "selection");
    expect(selectionGroup).toBeDefined();
    const translateCmd = selectionGroup!.items.find(
      (i) => i.id === "maild.mailbox.translate",
    );
    const retranslateCmd = selectionGroup!.items.find(
      (i) => i.id === "maild.mailbox.retranslate",
    );
    expect(translateCmd).toBeDefined();
    expect(translateCmd!.disabled).toBe(false);
    expect(translateCmd!.shortcut).toBe(STUDIO_SHORTCUT_KEYS.translateMessage);
    expect(retranslateCmd).toBeDefined();
    expect(retranslateCmd!.disabled).toBe(false);

    // Execute ⌘K translate -> translates via shared controller
    translateCmd!.onSelect();
    await flush();
    expect(bridge.settings.translation.plainRequests).toHaveLength(1);
    expect(bridge.settings.translation.plainRequests[0]!.force_refresh).toBe(false);
    expect(mailbox.translation.store.getSnapshot().phase).toBe("translated");

    // Execute ⌘K retranslate -> sets force_refresh: true
    retranslateCmd!.onSelect();
    await flush();
    expect(bridge.settings.translation.plainRequests).toHaveLength(2);
    expect(bridge.settings.translation.plainRequests[1]!.force_refresh).toBe(true);

    // Shortcut Center catalog includes Shift+T for maild.mailbox.translate
    const shortcutItems = buildShortcutCatalog(host.registry.getSnapshot()).flatMap(
      (g) => g.items,
    );
    expect(
      shortcutItems.find((item) => item.commandId === "maild.mailbox.translate"),
    ).toMatchObject({
      keys: [STUDIO_SHORTCUT_KEYS.translateMessage],
    });

    // Shift+T in Workspace toggles back to original via shared controller
    const documents = new DocumentController(bridge);
    cleanups.push(() => documents.stop());
    mount(
      h(
        StudioTooltipProvider,
        null,
        h(Workspace, {
          host,
          controller: mailbox.controller,
          documents,
          composer,
          setup: () => {},
          translation: mailbox.translation,
        }),
      ),
    );
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "T",
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    await flush();
    expect(mailbox.translation.store.getSnapshot().phase).toBe("original");
  });
});
