import { applyEffectiveLocales } from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";
import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import { BridgeError } from "../src/bridge/errors";
import { Host } from "../src/host/host";
import { NetworkProxy } from "../src/settings/NetworkProxy";
import { SettingsController } from "../src/settings/controller";
import { settingsModule as studioSettingsModule } from "../src/settings/module";
import {
  PROXY_TEST_PRESETS,
  defaultImapTarget,
  inferProxyFailureStage,
  parseProxyUrl,
  validateProxyDraft,
  validateTestTarget,
} from "../src/settings/proxy";
import { FakeBridge, flush, online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";

const CJK = /[㐀-鿿豈-﫿　-〿＀-￯]/;

afterEach(() => {
  applyEffectiveLocales([]);
});

function setup(configure?: (bridge: FakeSettings) => void) {
  const bridge = new FakeSettings();
  configure?.(bridge);
  const controller = new SettingsController(bridge);
  controller.connection(online);
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

describe("NetworkProxy settings section registration", () => {
  it("registers maild.settings.network at order 80 between Extensions (70) and Connection (90)", () => {
    const host = new Host(new FakeBridge());
    const studio = studioSettingsModule(host);
    try {
      const sorted = [...(studio.settings ?? [])].sort(
        (a, b) => (a.order ?? 0) - (b.order ?? 0),
      );
      const ids = sorted.map((s) => s.id);
      const extIdx = ids.indexOf("maild.settings.extensions");
      const netIdx = ids.indexOf("maild.settings.network");
      const connIdx = ids.indexOf("maild.settings.connection");
      expect(netIdx).toBeGreaterThan(extIdx);
      expect(connIdx).toBeGreaterThan(netIdx);
      expect(sorted[netIdx]?.order).toBe(80);

      // Titles switch dynamically between zh-CN and en
      const studioNet = studio.settings!.find((s) => s.id === "maild.settings.network")!;
      expect(studioNet.title).toBe("网络与代理");
      applyEffectiveLocales(["en"]);
      expect(studioNet.title).toBe("Network & proxy");
    } finally {
      studio.dispose?.();
      host.stop();
    }
  });
});

describe("Proxy URL parsing and validation", () => {
  it("auto-parses full proxy URLs including schemes, IPv6, default ports, and credentials", () => {
    expect(parseProxyUrl("socks5h://127.0.0.1:7890")).toEqual({
      protocol: "socks5h",
      host: "127.0.0.1",
      port: "7890",
      username: null,
      password: null,
    });
    expect(parseProxyUrl("HTTP://alice:p%40ss%3A1@proxy.example.com:8080/")).toEqual({
      protocol: "http",
      host: "proxy.example.com",
      port: "8080",
      username: "alice",
      password: "p@ss:1",
    });
    expect(parseProxyUrl("https://[::1]:8443")).toEqual({
      protocol: "https",
      host: "::1",
      port: "8443",
      username: null,
      password: null,
    });
    expect(parseProxyUrl("socks5://proxy.internal")).toEqual({
      protocol: "socks5",
      host: "proxy.internal",
      port: "1080",
      username: null,
      password: null,
    });
    expect(parseProxyUrl("127.0.0.1:7890")).toBeNull();
    expect(parseProxyUrl("ftp://127.0.0.1:21")).toBeNull();
    expect(parseProxyUrl("http://127.0.0.1:8080/path")).toBeNull();
    expect(parseProxyUrl("http://127.0.0.1:99999")).toBeNull();
  });

  it("validates custom proxy host, port, credentials, bypass list, and test target", () => {
    const base = {
      mode: "custom" as const,
      protocol: "socks5h" as const,
      host: "127.0.0.1",
      port: "7890",
      authEnabled: false,
      username: "",
      password: "",
      clearPassword: false,
      bypassText: "",
    };
    expect(validateProxyDraft(base)).toBeNull();
    expect(validateProxyDraft({ ...base, mode: "system", host: "", port: "" })).toBeNull();
    expect(validateProxyDraft({ ...base, host: "" })).toBe(m.settings_proxy_error_host_required());
    expect(validateProxyDraft({ ...base, host: "a".repeat(254) })).toBe(
      m.settings_proxy_error_host_invalid(),
    );
    expect(validateProxyDraft({ ...base, host: "bad host" })).toBe(
      m.settings_proxy_error_host_invalid(),
    );
    expect(validateProxyDraft({ ...base, port: "0" })).toBe(m.settings_proxy_error_port_invalid());
    expect(validateProxyDraft({ ...base, port: "65536" })).toBe(
      m.settings_proxy_error_port_invalid(),
    );
    expect(validateProxyDraft({ ...base, authEnabled: true, username: "a:b" })).toBe(
      m.settings_proxy_error_username_invalid(),
    );
    expect(
      validateProxyDraft({ ...base, authEnabled: true, username: "u".repeat(256) }),
    ).toBe(m.settings_proxy_error_username_invalid());
    expect(
      validateProxyDraft({ ...base, authEnabled: true, password: "line1\nline2" }),
    ).toBe(m.settings_proxy_error_password_invalid());
    expect(
      validateProxyDraft({
        ...base,
        bypassText: Array.from({ length: 257 }, (_, i) => `h${i}.local`).join("\n"),
      }),
    ).toBe(m.settings_proxy_error_bypass_too_many());
    expect(validateProxyDraft({ ...base, bypassText: "bad host.local" })).toBe(
      m.settings_proxy_error_bypass_invalid(),
    );

    expect(validateTestTarget("imap.gmail.com:993")).toBeNull();
    expect(validateTestTarget("[::1]:993")).toBeNull();
    expect(validateTestTarget("")).toBe(m.settings_proxy_error_target_required());
    expect(validateTestTarget("imap.gmail.com")).toBe(m.settings_proxy_error_target_invalid());
    expect(validateTestTarget("imap.gmail.com:0")).toBe(m.settings_proxy_error_target_invalid());
  });
});

describe("NetworkProxy controller and UI interactions", () => {
  it("renders system mode summary card, PAC notice banner, and direct mode note", async () => {
    const { controller } = setup((bridge) => {
      bridge.proxyState = {
        config: { mode: "system", custom: null },
        has_password: false,
        effective_system: {
          http: null,
          https: null,
          socks5: { protocol: "socks5h", host: "127.0.0.1", port: 7890 },
          bypass: ["*.local"],
          exclude_simple_hostnames: false,
          pac_url: "http://corp.example/proxy.pac",
          source: "macos",
        },
        persisted: true,
      };
    });
    controller.activate("maild.settings.network");
    await flush();

    const sysHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(sysHtml).toContain("SOCKS5 · 127.0.0.1:7890");
    expect(sysHtml).toContain("http://corp.example/proxy.pac");
    expect(sysHtml).toContain("macOS 系统设置");

    // Empty system proxy shows Direct summary
    controller.applyProxyEvent({
      config: { mode: "system", custom: null },
      has_password: false,
      effective_system: {
        http: null,
        https: null,
        socks5: null,
        bypass: [],
        exclude_simple_hostnames: false,
        pac_url: null,
        source: "none",
      },
      persisted: true,
      generation: 1,
    });
    const directSysHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(directSysHtml).toContain("未检测到系统代理（直连）");

    // Switching to direct mode shows env-var note
    controller.editProxy({ mode: "direct" });
    const directHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(directHtml).toContain("HTTP_PROXY");
    expect(directHtml).toContain("HTTPS_PROXY");
    expect(directHtml).toContain("ALL_PROXY");
    controller.dispose();
  });

  it("auto-fills protocol, host, port, and auth when pasting a full proxy URL", async () => {
    const { controller } = setup();
    controller.activate("maild.settings.network");
    await flush();
    controller.editProxy({ mode: "custom" });

    // Paste event on host input auto-fills all fields
    const { elements } = capture(() => NetworkProxy({ controller }));
    const hostInput = elements.find(
      (el) => el.type === "input" && el.props.placeholder === m.settings_proxy_host_placeholder(),
    );
    expect(hostInput).toBeDefined();
    let prevented = false;
    (hostInput!.props.onPaste as (e: unknown) => void)({
      clipboardData: { getData: () => "socks5h://bob:secret@127.0.0.1:7890" },
      preventDefault: () => {
        prevented = true;
      },
    });
    expect(prevented).toBe(true);
    const draft = controller.store.getSnapshot().proxyDraft!;
    expect(draft.protocol).toBe("socks5h");
    expect(draft.host).toBe("127.0.0.1");
    expect(draft.port).toBe("7890");
    expect(draft.authEnabled).toBe(true);
    expect(draft.username).toBe("bob");
    expect(draft.password).toBe("secret");
    expect(controller.store.getSnapshot().proxyUrlNotice).toBe(m.settings_proxy_url_parsed());
    controller.dispose();
  });

  it("never prefills saved passwords, supports keep/clear, and clears in-memory password on save, cancel, and disconnect", async () => {
    const { bridge, controller } = setup((b) => {
      b.proxyState = {
        config: {
          mode: "custom",
          custom: {
            protocol: "socks5h",
            host: "127.0.0.1",
            port: 7890,
            username: "alice",
            bypass: ["*.internal"],
          },
        },
        has_password: true,
        effective_system: b.proxyState.effective_system,
        persisted: true,
      };
    });
    controller.activate("maild.settings.network");
    await flush();

    // Never prefill saved password; hint + clear button rendered
    expect(controller.store.getSnapshot().proxyDraft?.password).toBe("");
    const html = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(html).toContain(m.settings_proxy_password_saved_hint());
    expect(html).toContain(m.settings_proxy_password_clear());
    expect(html).toContain("localhost / 127.0.0.1 / ::1");

    // Updating port without entering password preserves saved password on daemon
    controller.editProxy({ port: "7891" });
    await controller.saveProxy();
    expect(bridge.proxyWrites[0]).toEqual({
      mode: "custom",
      custom: {
        protocol: "socks5h",
        host: "127.0.0.1",
        port: 7891,
        username: "alice",
        bypass: ["*.internal"],
      },
    });
    expect(controller.store.getSnapshot().proxyHasPassword).toBe(true);
    expect(controller.store.getSnapshot().proxyDraft?.password).toBe("");

    // Typing a new password and clicking discard clears in-memory password
    controller.editProxy({ password: "temp-password" });
    expect(controller.dirty).toBe(true);
    controller.discard();
    expect(controller.store.getSnapshot().proxyDraft?.password).toBe("");
    expect(controller.dirty).toBe(false);

    // Typing a new password and disconnecting clears in-memory password
    controller.editProxy({ password: "disconnect-secret" });
    controller.connection({ ...online, phase: "retry_waiting", connection_epoch: 2 });
    expect(controller.store.getSnapshot().proxyDraft?.password).toBe("");
    controller.connection({ ...online, connection_epoch: 3 });

    // Typing a new password and saving clears in-memory password even on success
    controller.editProxy({ password: "new-saved-secret" });
    await controller.saveProxy();
    expect(bridge.proxyWrites[1]?.custom?.password).toBe("new-saved-secret");
    expect(controller.store.getSnapshot().proxyDraft?.password).toBe("");

    // Explicitly clearing saved password sends clear_password: true
    controller.clearSavedProxyPassword();
    expect(controller.dirty).toBe(true);
    await controller.saveProxy();
    expect(bridge.proxyWrites[2]?.custom?.clear_password).toBe(true);
    expect(bridge.proxyWrites[2]?.custom?.password).toBeUndefined();
    expect(controller.store.getSnapshot().proxyHasPassword).toBe(false);
    controller.dispose();
  });

  it("tests unsaved drafts with use_saved_password, uses default IMAP target or manual presets, and renders .probe-line stages", async () => {
    const { bridge, controller } = setup((b) => {
      b.proxyState = {
        config: {
          mode: "custom",
          custom: {
            protocol: "socks5h",
            host: "127.0.0.1",
            port: 7890,
            username: "alice",
            bypass: [],
          },
        },
        has_password: true,
        effective_system: b.proxyState.effective_system,
        persisted: true,
      };
      b.proxyTestResult = {
        ok: true,
        route: "socks5h://alice@127.0.0.1:7891",
        stage: "ready",
        latency_ms: 85,
        detail_code: null,
        detail: "",
      };
    });
    controller.activate("maild.settings.network");
    await flush();

    // Default target derived from first enabled IMAP account
    expect(defaultImapTarget(controller.store.getSnapshot().profiles)).toBe(
      "imap.example.test:993",
    );

    // Edit port (unsaved draft) without typing password -> testProxy uses use_saved_password: true
    controller.editProxy({ port: "7891" });
    await controller.testProxy();
    expect(bridge.proxyTests[0]).toEqual({
      config: {
        mode: "custom",
        custom: {
          protocol: "socks5h",
          host: "127.0.0.1",
          port: 7891,
          username: "alice",
          bypass: [],
        },
      },
      use_saved_password: true,
    });
    const okHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(okHtml).toContain("probe-line");
    expect(okHtml).toContain("socks5h://alice@127.0.0.1:7891");
    expect(okHtml).toContain("85 ms");

    // Test failure stages (proxy_auth, proxy_tls inferred from TLS detail, proxy_dns inferred from DNS detail)
    bridge.proxyTestResult = {
      ok: false,
      route: "socks5h://alice@127.0.0.1:7891",
      stage: "proxy_auth",
      latency_ms: 19,
      detail_code: "proxy_auth_failed",
      detail: "SOCKS5 auth rejected",
    };
    await controller.testProxy();
    const failHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(failHtml).toContain("probe-line failed");
    expect(failHtml).toContain(m.settings_proxy_stage_proxy_auth());
    expect(failHtml).toContain(m.settings_proxy_hint_proxy_auth());
    expect(failHtml).toContain("SOCKS5 auth rejected");

    expect(
      inferProxyFailureStage({
        ok: false,
        route: "https://proxy:443",
        stage: "proxy_connect",
        latency_ms: 10,
        detail: "HTTPS TLS handshake failed",
      }),
    ).toBe("proxy_tls");
    expect(
      inferProxyFailureStage({
        ok: false,
        route: "http://proxy:8080",
        stage: "proxy_connect",
        latency_ms: 10,
        detail: "proxy DNS resolution failed",
      }),
    ).toBe("proxy_dns");

    // No enabled account -> presets rendered and required target validated
    bridge.profileRows = [{ ...account(), enabled: false }];
    controller.discard();
    await controller.load();
    const noAcctHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    for (const preset of PROXY_TEST_PRESETS) {
      expect(noAcctHtml).toContain(preset);
    }
    await controller.testProxy();
    expect(controller.store.getSnapshot().error).toBe(
      m.settings_proxy_error_target_required(),
    );
    controller.setProxyTestTarget("1.1.1.1:443");
    await controller.testProxy();
    expect(bridge.proxyTests.at(-1)).toEqual({ target: "1.1.1.1:443" });
    controller.dispose();
  });

  it("integrates with SettingsController.dirty leave guard", async () => {
    const { controller } = setup();
    controller.activate("maild.settings.network");
    await flush();
    expect(controller.dirty).toBe(false);

    controller.editProxy({ mode: "direct" });
    expect(controller.dirty).toBe(true);

    let left = false;
    const allowed = controller.requestLeave(() => {
      left = true;
    });
    expect(allowed).toBe(false);
    expect(left).toBe(false);
    expect(controller.store.getSnapshot().confirm?.title).toBe(
      m.settings_leave_confirm_title(),
    );

    controller.cancelConfirmation();
    expect(controller.store.getSnapshot().proxyDraft?.mode).toBe("direct");

    controller.requestLeave(() => {
      left = true;
    });
    controller.confirm();
    expect(left).toBe(true);
    expect(controller.dirty).toBe(false);
    expect(controller.store.getSnapshot().proxyDraft?.mode).toBe("system");
    controller.dispose();
  });

  it("shows read-only warning and disables save when persisted=false", async () => {
    const { bridge, controller } = setup((b) => {
      b.proxyState = { ...b.proxyState, persisted: false };
    });
    controller.activate("maild.settings.network");
    await flush();

    const html = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(html).toContain(m.settings_proxy_readonly_banner());
    expect(html).toContain('role="alert"');
    expect(html).toMatch(/<fieldset[^>]*disabled=""/);

    // Edits and saves are ignored when read-only
    controller.editProxy({ mode: "direct" });
    expect(controller.store.getSnapshot().proxyDraft?.mode).toBe("system");
    await controller.saveProxy();
    expect(bridge.proxyWrites).toHaveLength(0);
    controller.dispose();
  });

  it("shows clear unsupported notice when daemon returns -32601 (proxy_unsupported)", async () => {
    const { controller } = setup((b) => {
      b.proxyError = new BridgeError({
        code: "failed",
        reason: "proxy_unsupported",
        message: "后台版本不支持全局代理设置。",
      });
    });
    controller.activate("maild.settings.network");
    await flush();

    const state = controller.store.getSnapshot();
    expect(state.proxyUnsupported).toBe(true);
    expect(state.error).toBeNull();
    const html = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(html).toContain(m.settings_proxy_unsupported_banner());
    expect(html).not.toContain(m.settings_proxy_save());
    controller.dispose();
  });

  it("refreshes live state on preferences/proxy push without clobbering active unsaved edits", async () => {
    const { bridge, controller } = setup();
    controller.activate("maild.settings.network");
    await flush();

    // Clean form updates live on push
    bridge.pushProxy({
      config: { mode: "direct", custom: null },
      has_password: false,
      effective_system: {
        http: null,
        https: null,
        socks5: { protocol: "socks5h", host: "127.0.0.1", port: 1080 },
        bypass: [],
        exclude_simple_hostnames: false,
        pac_url: null,
        source: "macos",
      },
      persisted: true,
      generation: 1,
    });
    expect(controller.store.getSnapshot().proxyDraft?.mode).toBe("direct");
    expect(controller.store.getSnapshot().proxySystem?.socks5?.port).toBe(1080);
    expect(controller.dirty).toBe(false);

    // Dirty form preserves user edits while updating live system snapshot
    controller.editProxy({ mode: "custom", host: "10.0.0.9", port: "8080" });
    expect(controller.dirty).toBe(true);
    bridge.pushProxy({
      config: { mode: "system", custom: null },
      has_password: false,
      effective_system: {
        http: null,
        https: null,
        socks5: { protocol: "socks5h", host: "127.0.0.1", port: 7890 },
        bypass: [],
        exclude_simple_hostnames: false,
        pac_url: null,
        source: "macos",
      },
      persisted: true,
      generation: 2,
    });
    expect(controller.store.getSnapshot().proxyDraft?.mode).toBe("custom");
    expect(controller.store.getSnapshot().proxyDraft?.host).toBe("10.0.0.9");
    expect(controller.store.getSnapshot().proxySystem?.socks5?.port).toBe(7890);
    expect(controller.dirty).toBe(true);
    controller.dispose();
  });

  it("renders completely in English with zero CJK leaks when locale is en", async () => {
    applyEffectiveLocales(["en"]);
    const { controller } = setup((b) => {
      b.proxyState = {
        config: {
          mode: "custom",
          custom: {
            protocol: "socks5h",
            host: "127.0.0.1",
            port: 7890,
            username: "alice",
            bypass: ["*.local"],
          },
        },
        has_password: true,
        effective_system: {
          http: null,
          https: null,
          socks5: { protocol: "socks5h", host: "127.0.0.1", port: 7890 },
          bypass: [],
          exclude_simple_hostnames: false,
          pac_url: "http://example.com/proxy.pac",
          source: "macos",
        },
        persisted: true,
      };
      b.proxyTestResult = {
        ok: false,
        route: "socks5h://alice@127.0.0.1:7890",
        stage: "proxy_tunnel",
        latency_ms: 120,
        detail_code: "proxy_tunnel_failed",
        detail: "CONNECT returned 403",
      };
    });
    controller.activate("maild.settings.network");
    await flush();
    await controller.testProxy();

    const customHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(customHtml).toContain("Network and proxy");
    expect(customHtml).toContain("Password saved; leave blank to keep unchanged.");
    expect(customHtml).toContain("Test failed · Target tunnel");
    expect(customHtml).not.toMatch(CJK);

    controller.editProxy({ mode: "system" });
    const sysHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(sysHtml).toContain("Detected system proxy");
    expect(sysHtml).not.toMatch(CJK);

    controller.editProxy({ mode: "direct" });
    const directHtml = renderToStaticMarkup(createElement(NetworkProxy, { controller }));
    expect(directHtml).toContain("Direct mode ignores OS system proxy");
    expect(directHtml).not.toMatch(CJK);
    controller.dispose();
  });
});
