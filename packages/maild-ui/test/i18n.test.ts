import { expect, it } from "vitest";
import { folderLabel, lifecycleLabel, permissionLabel } from "../src/i18n";

it("localizes standard system folders without mutating their protocol identity", () => {
  const folder = { name: "INBOX", display_name: "INBOX", role: "inbox" };
  expect(folderLabel(folder)).toBe("收件箱");
  expect(folder).toEqual({
    name: "INBOX",
    display_name: "INBOX",
    role: "inbox",
  });
  expect(folderLabel({ ...folder, display_name: "Inbox" })).toBe("收件箱");
  for (const [name, role, label] of [
    ["Sent Mail", "sent", "已发送"],
    ["Drafts", "drafts", "草稿"],
    ["Archive", "archive", "归档"],
    ["Deleted Items", "trash", "已删除"],
    ["Junk E-mail", "junk", "垃圾邮件"],
    ["All Mail", "all", "全部邮件"],
  ])
    expect(folderLabel({ name, display_name: name, role })).toBe(label);
});

it("keeps custom names, custom display labels and unknown roles intact", () => {
  for (const folder of [
    { name: "Archive", display_name: "Private Archive", role: "archive" },
    { name: "Projects", display_name: "Sent Mail", role: "sent" },
    { name: "Archive", display_name: "Archive", role: "custom" },
    { name: "INBOX/Finance", display_name: "INBOX/Finance", role: "inbox" },
    { name: "设计项目", display_name: "设计项目", role: "archive" },
    { name: "Projects", display_name: "Projects", role: "constructor" },
  ])
    expect(folderLabel(folder)).toBe(folder.display_name);
});

it("preserves namespaces and distinguishes leaf display names from complete paths", () => {
  expect(
    folderLabel({
      name: "[Gmail]/Sent Mail",
      display_name: "[Gmail]/Sent Mail",
      role: "sent",
    }),
  ).toBe("[Gmail]/已发送");
  expect(
    folderLabel({
      name: "[Gmail]/Sent Mail",
      display_name: "Sent Mail",
      role: "sent",
    }),
  ).toBe("已发送");
  expect(folderLabel({ name: "INBOX", display_name: "INBOX", role: "" })).toBe(
    "收件箱",
  );
  expect(folderLabel(undefined, "Projects")).toBe("Projects");
});

it("does not describe an unknown package lifecycle as independent", () => {
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
