import { applyEffectiveLocales } from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import { identityFailureText, packageText, providerName } from "../src/i18n";
import { operationDetail, operationDiscarded } from "../src/mailbox/activityController";
import type { Operation } from "../src/mailbox/types";
import { SettingsController } from "../src/settings/controller";
import { Extensions } from "../src/settings/Extensions";
import type { Frontend, PackagePreview } from "../src/settings/types";
import { FakeSettings } from "./fakeSettings";

afterEach(() => {
  applyEffectiveLocales([]);
});

// daemon 原样透传的 manifest 形状（任务 F 报告里的示例）。
const reader = {
  id: "org.example.reader",
  name: "Example Reader",
  description: "A reader for Featherpost",
  l10n: {
    en: { description: "A reader for Featherpost" },
    "zh-Hans": { name: "示例阅读器", description: "Featherpost 的阅读器" },
  },
};

describe("包名与说明按界面语言协商 manifest l10n（ADR-0048 决策 5）", () => {
  it("匹配界面语言的条目逐字段覆盖顶层，缺的字段回落顶层", () => {
    expect(packageText(reader)).toEqual({ name: "示例阅读器", description: "Featherpost 的阅读器" });
    applyEffectiveLocales(["en"]);
    // en 条目没有 name：回落顶层 name。
    expect(packageText(reader)).toEqual({ name: "Example Reader", description: "A reader for Featherpost" });
  });

  it("繁体与不支持的语言不算匹配；没有 l10n 或说明时用顶层", () => {
    const hant = { name: "Reader", l10n: { "zh-Hant-TW": { name: "閱讀器" }, ja: { name: "リーダー" } } };
    expect(packageText(hant)).toEqual({ name: "Reader", description: null });
    expect(packageText({ name: "Plain" })).toEqual({ name: "Plain", description: null });
    // 无书写系统的 zh-CN 标签与界面 zh-CN 匹配；空字符串条目不覆盖顶层。
    expect(packageText({ name: "R", l10n: { "zh-CN": { name: "读者", description: " " } }, description: "d" })).toEqual({
      name: "读者",
      description: "d",
    });
  });

  it("收信插件名按 ID 找到就本地化，找不到显示 ID", () => {
    expect(providerName([reader], "org.example.reader")).toBe("示例阅读器");
    expect(providerName([reader], "org.example.missing")).toBe("org.example.missing");
    expect(providerName(undefined, "x")).toBe("x");
  });

  it("设置 → 扩展与安装预览显示本地化名字与说明，预览对象本身不被改写", () => {
    const frontend: Frontend = {
      ...reader,
      version: "1.0.0",
      lifecycle: "independent",
      capabilities: [],
      permissions: [],
      builtin: false,
      has_identity: true,
    };
    const preview: PackagePreview = {
      ...reader,
      kind: "frontend",
      version: "1.0.0",
      lifecycle: "independent",
      capabilities: [],
      permissions: [],
      has_ui: false,
    };
    const snapshot = structuredClone(preview);
    const controller = new SettingsController(new FakeSettings());
    controller.store.patch({ packages: { default: "x", frontends: [frontend], plugins: [] } });
    let html = renderToStaticMarkup(createElement(Extensions, { controller }));
    expect(html).toContain("示例阅读器");
    expect(html).toContain("Featherpost 的阅读器");
    applyEffectiveLocales(["en"]);
    html = renderToStaticMarkup(createElement(Extensions, { controller }));
    expect(html).toContain("Example Reader");
    expect(html).not.toContain("示例阅读器");

    controller.store.patch({ choice: { token: "t", source: "/tmp/p", preview } });
    html = renderToStaticMarkup(createElement(Extensions, { controller }));
    expect(html).toContain("<h3>Example Reader</h3>");
    expect(html).toContain("A reader for Featherpost");
    expect(preview).toEqual(snapshot);
    controller.dispose();
  });
});

describe("内核与 actor 的状态文字按 code 成文（ADR-0048 决策 3）", () => {
  it("身份绑定失败：按 code 翻译，不显示 actor 拼的中文原因", () => {
    const failure = { identity_failure: "前端包 `x` 未安装或没有身份条目（插件未安装）", identity_failure_code: "not_installed" };
    expect(identityFailureText({ identity_failure: null, identity_failure_code: null })).toBeNull();
    applyEffectiveLocales(["en"]);
    expect(identityFailureText(failure)).toBe(m.common_identity_failure_not_installed());
    expect(identityFailureText(failure)).not.toContain("未安装");
    for (const code of ["rejected", "principal_mismatch", "unavailable"])
      expect(identityFailureText({ identity_failure: "x", identity_failure_code: code })).not.toBe(
        m.common_identity_failure_unknown(),
      );
    // 老壳没有 code、或认不出的 code：通用说明。
    expect(identityFailureText({ identity_failure: "x" })).toBe(m.common_identity_failure_unknown());
    expect(identityFailureText({ identity_failure: "x", identity_failure_code: "future" })).toBe(
      m.common_identity_failure_unknown(),
    );
  });

  const op = (extra: Partial<Operation>): Operation => ({
    account: "demo",
    folder: "INBOX",
    uid: "1",
    id: 1,
    kind: "move",
    state: "failed",
    attempts: 3,
    created_at: 1,
    updated_at: 1,
    detail: "",
    needs_decision: true,
    target: "Archive",
    flag: null,
    value: null,
    ...extra,
  });

  it("写队列摘要：内核固定句子按 detail_code 成文，老后台回落 detail 与放弃标记", () => {
    applyEffectiveLocales(["en"]);
    const rejected = op({ detail: "服务器明确拒绝或重试次数已达上限，请核对账户与目标文件夹。", detail_code: "rejected" });
    expect(operationDetail(rejected)).toBe(m.mailbox_activity_detail_rejected());
    expect(operationDetail(op({ state: "unknown", detail: "x", detail_code: "unconfirmed" }))).toBe(
      m.mailbox_activity_detail_unconfirmed(),
    );
    expect(operationDetail(op({ detail: "server said no" }))).toBe("server said no");
    expect(operationDiscarded(op({ detail: "用户放弃", detail_code: "discarded" }))).toBe(true);
    expect(operationDiscarded(op({ detail: "用户放弃" }))).toBe(true);
    expect(operationDiscarded(op({ detail: "用户放弃", detail_code: "rejected" }))).toBe(false);
    expect(operationDiscarded(op({ state: "unknown", detail_code: "discarded" }))).toBe(false);
  });
});
