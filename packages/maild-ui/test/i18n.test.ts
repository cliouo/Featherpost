import {
  applyEffectiveLocales,
  getUiLocale,
  negotiateLocale,
  subscribeLocale,
} from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import {
  folderLabel,
  lifecycleLabel,
  permissionLabel,
  uiLocale,
  useUiLocale,
} from "../src/i18n";
import { dateOnly } from "../src/mailbox/format";

type Locale = "zh-CN" | "en";
const LOCALES: Locale[] = ["zh-CN", "en"];

/** 在指定语言下运行断言；结束后回到默认语言，不把状态泄漏给其它用例。 */
function inLocale(locale: Locale, run: () => void) {
  applyEffectiveLocales([locale]);
  try {
    run();
  } finally {
    applyEffectiveLocales([]);
  }
}

afterEach(() => {
  applyEffectiveLocales([]);
});

const folders: Record<Locale, Record<string, string>> = {
  "zh-CN": {
    inbox: "收件箱",
    sent: "已发送",
    drafts: "草稿箱",
    archive: "归档",
    trash: "已删除",
    junk: "垃圾邮件",
    all: "全部邮件",
  },
  en: {
    inbox: "Inbox",
    sent: "Sent",
    drafts: "Drafts",
    archive: "Archive",
    trash: "Trash",
    junk: "Junk",
    all: "All Mail",
  },
};

describe.each(LOCALES)("folderLabel（%s）", (locale) => {
  const label = folders[locale];

  it("localizes standard system folders without mutating their protocol identity", () => {
    inLocale(locale, () => {
      const folder = { name: "INBOX", display_name: "INBOX", role: "inbox" };
      expect(folderLabel(folder)).toBe(label.inbox);
      expect(folder).toEqual({
        name: "INBOX",
        display_name: "INBOX",
        role: "inbox",
      });
      expect(folderLabel({ ...folder, display_name: "Inbox" })).toBe(
        label.inbox,
      );
      for (const [name, role] of [
        ["Sent Mail", "sent"],
        ["Drafts", "drafts"],
        ["Archive", "archive"],
        ["Deleted Items", "trash"],
        ["Junk E-mail", "junk"],
        ["Spam", "spam"],
        ["All Mail", "all"],
      ])
        expect(folderLabel({ name, display_name: name, role })).toBe(
          label[role === "spam" ? "junk" : role],
        );
    });
  });

  it("keeps custom names, custom display labels and unknown roles intact", () => {
    inLocale(locale, () => {
      for (const folder of [
        { name: "Archive", display_name: "Private Archive", role: "archive" },
        { name: "Projects", display_name: "Sent Mail", role: "sent" },
        { name: "Archive", display_name: "Archive", role: "custom" },
        { name: "INBOX/Finance", display_name: "INBOX/Finance", role: "inbox" },
        { name: "设计项目", display_name: "设计项目", role: "archive" },
        { name: "草稿", display_name: "草稿", role: "drafts" },
        { name: "Projects", display_name: "Projects", role: "constructor" },
      ])
        expect(folderLabel(folder)).toBe(folder.display_name);
    });
  });

  it("preserves namespaces and distinguishes leaf display names from complete paths", () => {
    inLocale(locale, () => {
      expect(
        folderLabel({
          name: "[Gmail]/Sent Mail",
          display_name: "[Gmail]/Sent Mail",
          role: "sent",
        }),
      ).toBe(`[Gmail]/${label.sent}`);
      expect(
        folderLabel({
          name: "[Gmail]/Sent Mail",
          display_name: "Sent Mail",
          role: "sent",
        }),
      ).toBe(label.sent);
      expect(
        folderLabel({ name: "INBOX", display_name: "INBOX", role: "" }),
      ).toBe(label.inbox);
      expect(folderLabel(undefined, "Projects")).toBe("Projects");
    });
  });
});

describe("lifecycle and permission labels", () => {
  it("does not describe an unknown package lifecycle as independent (zh-CN)", () => {
    expect(lifecycleLabel("independent")).toBe("独立运行");
    expect(lifecycleLabel("on_demand")).toBe("按需启动");
    expect(lifecycleLabel("supervised")).toBe("由后台托管运行");
    expect(lifecycleLabel("future")).toBe("未知运行方式（future）");
    expect(lifecycleLabel("")).toBe("未声明运行方式");
    expect(lifecycleLabel("toString")).toBe("未知运行方式（toString）");
    expect(permissionLabel("drafts_read")).toBe("读取草稿");
    expect(permissionLabel("future_permission")).toBe("未识别的功能或权限");
    expect(permissionLabel("constructor")).toBe("未识别的功能或权限");
  });

  it("does not describe an unknown package lifecycle as independent (en)", () => {
    inLocale("en", () => {
      expect(lifecycleLabel("independent")).toBe("Runs on its own");
      expect(lifecycleLabel("on_demand")).toBe("Starts on demand");
      expect(lifecycleLabel("supervised")).toBe("Run by the background service");
      expect(lifecycleLabel("future")).toBe("Unknown run mode (future)");
      expect(lifecycleLabel("")).toBe("Run mode not declared");
      expect(lifecycleLabel("toString")).toBe("Unknown run mode (toString)");
      expect(permissionLabel("drafts_read")).toBe("Read drafts");
      expect(permissionLabel("future_permission")).toBe(
        "Unrecognized capability or permission",
      );
      expect(permissionLabel("constructor")).toBe(
        "Unrecognized capability or permission",
      );
    });
  });
});

describe("negotiateLocale（ADR-0048 决策 8）", () => {
  it.each([
    [["zh-Hans-CN", "en-US"], "zh-CN"],
    [["zh-Hans"], "zh-CN"],
    [["zh-CN"], "zh-CN"],
    [["zh"], "zh-CN"],
    [["zh-SG"], "zh-CN"],
    [["zh_CN"], "zh-CN"],
    [["ZH-hans-sg"], "zh-CN"],
    [["en-US", "zh-Hans"], "en"],
    [["en"], "en"],
    [["en-GB"], "en"],
    [["EN-Latn-US"], "en"],
    // 繁体与繁体地区不匹配，继续看下一项。
    [["zh-Hant-TW", "en-US"], "en"],
    [["zh-Hant"], "zh-CN"],
    [["zh-TW", "en"], "en"],
    [["zh-HK", "en"], "en"],
    [["zh-MO", "en"], "en"],
    [["zh-Hant-HK", "zh-Hans"], "zh-CN"],
    // 不支持的语言跳过；全部不匹配回落 zh-CN。
    [["ja-JP", "en-US"], "en"],
    [["ja-JP", "fr"], "zh-CN"],
    [["english", "zhuang"], "zh-CN"],
    [[""], "zh-CN"],
    [[], "zh-CN"],
  ] as const)("%j → %s", (effective, expected) => {
    expect(negotiateLocale(effective)).toBe(expected);
  });
});

describe("applyEffectiveLocales", () => {
  it("defaults to zh-CN and switches messages, Intl locale and subscribers without reload", () => {
    expect(getUiLocale()).toBe("zh-CN");
    expect(uiLocale()).toBe("zh-CN");
    expect(m.mailbox_folder_inbox()).toBe("收件箱");
    expect(dateOnly(1_705_320_000)).toBe("2024/1/15");

    let notified = 0;
    const unsubscribe = subscribeLocale(() => {
      notified += 1;
    });
    expect(applyEffectiveLocales(["zh-Hant-TW", "en-US"])).toBe("en");
    expect(notified).toBe(1);
    expect(uiLocale()).toBe("en");
    expect(m.mailbox_folder_inbox()).toBe("Inbox");
    expect(m.mailbox_account_folders_label({ account: "Work" })).toBe(
      "Work folders",
    );
    // Intl 格式化跟随协商结果（2024-01-15 12:00 UTC，任何时区都仍是 1 月 15 日）。
    expect(dateOnly(1_705_320_000)).toBe("1/15/2024");

    // 协商结果不变时不通知。
    expect(applyEffectiveLocales(["en-GB"])).toBe("en");
    expect(notified).toBe(1);

    expect(applyEffectiveLocales(["fr"])).toBe("zh-CN");
    expect(notified).toBe(2);
    expect(m.mailbox_folder_inbox()).toBe("收件箱");
    // 显式指定语言的调用不受当前语言影响。
    expect(m.mailbox_folder_inbox({}, { locale: "en" })).toBe("Inbox");

    unsubscribe();
    applyEffectiveLocales(["en"]);
    expect(notified).toBe(2);
  });

  it("renders React components in the negotiated locale", () => {
    function Probe() {
      const locale = useUiLocale();
      return createElement("span", { lang: locale }, m.mailbox_nav_label());
    }
    expect(renderToStaticMarkup(createElement(Probe))).toBe(
      '<span lang="zh-CN">账户与文件夹</span>',
    );
    inLocale("en", () => {
      expect(renderToStaticMarkup(createElement(Probe))).toBe(
        '<span lang="en">Accounts and folders</span>',
      );
    });
  });
});
