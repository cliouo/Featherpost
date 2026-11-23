import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { MailboxController } from "../src/mailbox/controller";
import { MailboxNavigation } from "../../featherpost-studio/src/MailboxNavigation";
import { FakeBridge, flush, online } from "./fakeBridge";

it.each([
  { name: "demo@example.test", subtitle: false },
  { name: " demo@example.test ", subtitle: false },
  { name: "", subtitle: false },
  { name: "工作邮箱", subtitle: true },
])("shows the account address only when distinct from '$name'", async ({ name, subtitle }) => {
  const bridge = new FakeBridge();
  bridge.mailbox.profileRows = [{
    id: "demo",
    display_name: name,
    address: "demo@example.test",
    enabled: true,
  }];
  const mailbox = new MailboxController(bridge);
  try {
    mailbox.acceptSnapshot(online);
    await flush();
    // Both navigation modes share the same identity and tooltip rules.
    for (const view of ["standard", "unified"] as const) {
      const html = renderToStaticMarkup(
        createElement(MailboxNavigation, {
          mailbox,
          view,
          active: true,
          unread: 0,
          navigate: async (action) => action(),
          focusRef: null,
          folders: () => null,
          setup() {},
          manageAccounts() {},
        }),
      );
      const identity = html.match(/<span class="account-identity">(.*?)<\/span>/)?.[1];
      expect(identity).toBe(
        `<strong>${name || "demo@example.test"}</strong>${subtitle ? "<small>demo@example.test</small>" : ""}`,
      );
      expect(html).toContain(
        `title="${subtitle ? `${name}\ndemo@example.test` : name || "demo@example.test"}"`,
      );
    }
  } finally {
    mailbox.stop();
  }
});
