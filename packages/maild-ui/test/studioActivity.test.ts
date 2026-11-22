import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it } from "vitest";
import { Activity } from "../../featherpost-studio/src/Activity";
import {
  ActivityController,
  type ActivityState,
} from "../src/mailbox/activityController";
import { MailboxController } from "../src/mailbox/controller";
import type { Operation } from "../src/mailbox/types";
import { FakeBridge } from "./fakeBridge";

const controllers: MailboxController[] = [];
afterEach(() => controllers.splice(0).forEach((c) => c.stop()));
function operation(patch: Partial<Operation> = {}): Operation {
  return {
    id: 1,
    kind: "flag",
    state: "failed",
    attempts: 2,
    created_at: 1700000000,
    updated_at: 1700000000,
    detail: "",
    needs_decision: true,
    target: null,
    flag: "seen",
    value: true,
    account: "first",
    folder: "INBOX",
    uid: "3",
    subject: "Fixture subject",
    ...patch,
  };
}
function render(patch: Partial<ActivityState> = {}) {
  const bridge = new FakeBridge();
  const mailbox = new MailboxController(bridge);
  controllers.push(mailbox);
  const controller = new ActivityController(bridge.mailbox);
  controller.store.patch({ online: true, ...patch });
  return renderToStaticMarkup(
    createElement(Activity, { controller, mailbox, openMailbox: () => {} }),
  );
}

it("retains every operation state and only offers replay for unresolved failures", () => {
  const html = render({
    ops: ["pending", "in_flight", "done", "failed", "unknown"].map(
      (state, id) => operation({ state, id }),
    ),
    more: true,
  });
  for (const text of [
    "等待同步",
    "同步中",
    "已同步",
    "同步失败",
    "结果待核对",
    "已尝试 2 次",
    "已加载 5 项",
    "加载更早的操作",
    "Fixture subject",
    "邮件 3",
  ])
    expect(html).toContain(text);
  expect(html.match(/>不再重试<\/button>/g)).toHaveLength(2);
  expect(html.match(/>重试<\/button>/g)).toHaveLength(1);
  expect(html.match(/>核对后重试<\/button>/g)).toHaveLength(1);
});

it("never offers replay for discarded or unsupported operations and renders future identifiers safely", () => {
  const html = render({
    ops: [
      operation({ id: 1, detail: "用户放弃" }),
      operation({ id: 2, kind: "future_operation" }),
      operation({ id: 3, state: "constructor" }),
      operation({ id: 4, state: "done", flag: "constructor" }),
    ],
  });
  expect(html).toContain("已停止重试");
  expect(html).toContain("此版本无法处理的状态");
  expect(html).toContain("更新邮件标记");
  expect(html).toContain("请更新客户端后处理");
  expect(html).not.toMatch(/>(不再重试|重试|核对后重试)<\/button>/);
  expect(html).not.toContain("function Object");
});

it("keeps all write and pagination controls disabled while offline or resolving", () => {
  for (const state of [{ online: false }, { busy: 1 }]) {
    const html = render({ ops: [operation()], more: true, ...state });
    const buttons = html.match(/<button[^>]*>/g)!;
    expect(buttons.length).toBeGreaterThan(3);
    expect(buttons.every((button) => button.includes('disabled=""'))).toBe(
      true,
    );
  }
});

it("does not mistake loading, errors or a disconnected daemon for an empty history", () => {
  expect(render()).toContain("还没有同步操作");
  for (const state of [
    { loading: true },
    { error: "无法读取活动" },
    { online: false },
  ]) {
    const html = render(state);
    expect(html).not.toContain("还没有同步操作");
  }
  expect(render({ loading: true })).toContain("正在读取同步活动");
  expect(render({ error: "无法读取活动" })).toContain('role="alert"');
  expect(render({ online: false })).toContain("后台连接不可用");
});
