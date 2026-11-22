import { m } from "@featherpost/i18n/messages";

import {followMailIntent} from './navigation';
import type { ComposeController } from "../compose/controller";
import { useEffect } from "react";
import { SearchBar } from "./SearchBar";
import { Actions } from "./Actions";
import { Activity } from "./Activity";
import { ActivityController } from "./activityController";
import "./mailbox.css";

import type { Host } from "../host/host";
import { useStore } from "../host/hooks";
import type { Bundle, ViewProps } from "../host/types";
import { FolderNav } from "./FolderNav";
import { MessageList } from "./MessageList";
import { Reader } from "./Reader";
import { ReadingPrivacySection } from "./ReadingPrivacy";
import { scopeTitle, MailboxController } from "./controller";
import { DocumentController } from "./document";
import { Store } from "../host/store";
import { compatBlocksWrites } from "../bridge/types";
import { shortcutFor, type TargetLike } from "./shortcuts";
import { runShortcut } from "./keyboard";
import { ShortcutHelp } from "./ShortcutHelp";
import { Welcome } from "./Welcome";
import type { AccountSetup } from "./types";
import { BRAND } from "../brand";
import { useUiLocale } from "../i18n";

export const MAILBOX_VIEW = "maild.mailbox.workspace";
export const ACTIVITY_VIEW = "maild.mailbox.activity";

/** 内置邮箱模块：和第三方一样经注册底座装配（ADR-0031 决策 5）。 */
export function mailboxModule(
  host: Host,
  composer?: ComposeController,
  setup?: AccountSetup,
): Bundle {
  const ui = new Store({ help: false });
  const openSetup: AccountSetup = (request) => {
    if (setup) setup(request);
    else host.openRole("settings");
  };
  const controller = new MailboxController(host.bridge, (selection) =>
    host.setSelection(selection),
  );
  // ADR-0033：隔离文档跟着详情走——换信释放旧令牌、重取详情重新准备。
  const activity = new ActivityController(host.bridge.mailbox);
  const documents = new DocumentController(host.bridge);
  const stopDocuments = documents.bindTo(controller.store);
  void documents.start();
  let seen = { revision: -2, epoch: -1 };
  let lastView=host.state.getSnapshot().activeViewId;
  const stopState = host.state.subscribe(() => {
    const hostState=host.state.getSnapshot();
    if(hostState.activeViewId!==lastView){lastView=hostState.activeViewId;controller.invalidateNavigation();}
    const snapshot = hostState.snapshot;
    if (
      snapshot.revision === seen.revision &&
      snapshot.connection_epoch === seen.epoch
    )
      return;
    seen = { revision: snapshot.revision, epoch: snapshot.connection_epoch };
    controller.acceptSnapshot(snapshot);
    activity.setConnection(
      snapshot.phase === "online",
      snapshot.connection_epoch,
    );
  });
  const stopIntent = host.onIntent((intent) => {
    if (intent.route === "background") {
      host.openView(ACTIVITY_VIEW);
      return;
    }
    if (intent.route !== "inbox" && intent.route !== "message") return;
    void followMailIntent(controller,intent,()=>composer?.flush()??Promise.resolve(),()=>host.openView(MAILBOX_VIEW),text=>host.notify(text),()=>host.approveNavigation(MAILBOX_VIEW));
  });
  const search = () => {
    host.openView(MAILBOX_VIEW);
    requestAnimationFrame(() =>
      document.querySelector<HTMLInputElement>("[data-mail-search]")?.focus(),
    );
  };
  const focusSearch = () =>
    document.querySelector<HTMLInputElement>("[data-mail-search]")?.focus();
  const sync = () => {
    void host.bridge.refresh().catch(() => {});
    void controller.syncNow();
  };
  const onKey = (e: KeyboardEvent) => {
    const modal = !!document.querySelector('dialog[open], [aria-modal="true"]');
    if (
      (e.metaKey || e.ctrlKey) &&
      e.key.toLowerCase() === "k" &&
      !modal
    ) {
      e.preventDefault();
      search();
      return;
    }
    if (e.defaultPrevented || host.state.getSnapshot().activeViewId !== MAILBOX_VIEW)
      return;
    const action = shortcutFor(e, e.target as TargetLike | null, modal);
    if (!action) return;
    const handled = runShortcut(action, {
      controller,
      execute: (id) => host.execute(id),
      writable: () => {
        const snapshot = host.state.getSnapshot().snapshot;
        return snapshot.phase === "online" && !compatBlocksWrites(snapshot);
      },
      focusSearch,
      toggleHelp: () => ui.patch({ help: !ui.getSnapshot().help }),
      sync,
    });
    if (handled) e.preventDefault();
  };
  window.addEventListener("keydown", onKey);

  function Workspace(_props: ViewProps) {
    const locale = useUiLocale();
    const state = useStore(controller.store);
    const { help } = useStore(ui);
    useEffect(() => {
      document.title = m.mailbox_document_title({
        scope: scopeTitle(state.scope),
        product: BRAND.productName,
      });
    }, [state.scope, locale]);
    const helpDialog = help && (
      <ShortcutHelp onClose={() => ui.patch({ help: false })} />
    );
    if (state.snapshot.phase === "online" && state.snapshot.accounts.length === 0)
      return (
        <div className="mail-area">
          <Welcome onSetup={(provider) => openSetup({ provider })} />
          {helpDialog}
        </div>
      );
    return (
      <div className="mail-area">
        <SearchBar controller={controller} />
        <Actions
          controller={controller}
          openActivity={() => host.openView(ACTIVITY_VIEW)}
        />
        {state.detailError && !state.selected && (
          <div className="list-banner" role="alert">
            {state.detailError}
          </div>
        )}
        <div
          className={`mail-workspace${state.selected ? " show-reader" : ""}`}
        >
          <MessageList controller={controller} />
          <Reader
            host={host}
            controller={controller}
            documents={documents}
            composer={composer}
          />
        </div>
        {helpDialog}
      </div>
    );
  }

  function Navigation(_props: ViewProps) {
    return (
      <FolderNav
        controller={controller}
        onOpenMailbox={() => host.openView(MAILBOX_VIEW)}
        onRepair={(account) => openSetup({ repair: account })}
      />
    );
  }

  function ActivityView() {
    return (
      <Activity
        controller={activity}
        mailbox={controller}
        openMailbox={() => host.openView(MAILBOX_VIEW)}
      />
    );
  }

  function ReadingPrivacy(_props: ViewProps) {
    // 策略变了：当前打开的邮件重取详情（结论在 getMessage 里），文档随之重新准备。
    return (
      <ReadingPrivacySection
        host={host}
        onChanged={() => controller.reloadSelected()}
      />
    );
  }

  // 贡献标题在注册时成文：注册底座按值复制贡献（host/registry.ts），标题是快照，切换语言后要等宿主支持
  // 按语言重取（见 ADR-0048 决策 8 实现记录）。
  return {
    moduleId: "maild.mailbox",
    apiVersion: 1,
    get title() {
      return m.mailbox_module_title();
    },
    views: [
      {
        id: MAILBOX_VIEW,
        get title() {
          return m.mailbox_view_mail();
        },
        icon: "inbox",
        role: "mailbox",
        order: 0,
        render: Workspace,
        navigation: Navigation,
      },
      {
        id: ACTIVITY_VIEW,
        get title() {
          return m.mailbox_activity_title();
        },
        icon: "activity",
        role: "activity",
        render: ActivityView,
      },
    ],
    // 「阅读与隐私」由邮箱模块贡献，与示例扩展走同一条注册路径（ADR-0033 决策 9）。
    settings: [
      {
        id: "maild.mailbox.reading-privacy",
        get title() {
          return m.mailbox_reading_title();
        },
        order: 15,
        render: ReadingPrivacy,
      },
    ],
    commands: [
      {
        id: "maild.mailbox.search",
        get title() {
          return m.mailbox_shortcut_search_mail();
        },
        icon: "search",
        locations: ["navigation"],
        order: 20,
        execute: search,
      },
      {
        id: "maild.mailbox.open-activity",
        get title() {
          return m.mailbox_activity_title();
        },
        icon: "activity",
        locations: ["navigation"],
        order: 30,
        execute: () => host.openView(ACTIVITY_VIEW),
      },
      {
        id: "maild.mailbox.read",
        get title() {
          return m.mailbox_command_mark_read();
        },
        icon: "mail",
        locations: ["toolbar", "menu"],
        order: 40,
        enabledWhen: "online-message-selected",
        execute: () => {
          void controller.perform({
            action: "mark",
            flag: "seen",
            value: true,
          });
        },
      },
      {
        id: "maild.mailbox.unread",
        get title() {
          return m.mailbox_command_mark_unread();
        },
        icon: "mail",
        locations: ["menu"],
        order: 41,
        enabledWhen: "online-message-selected",
        execute: () => {
          void controller.perform({
            action: "mark",
            flag: "seen",
            value: false,
          });
        },
      },
      {
        id: "maild.mailbox.archive",
        get title() {
          return m.mailbox_command_archive();
        },
        icon: "archive",
        locations: ["toolbar", "menu"],
        order: 50,
        enabledWhen: "online-message-selected",
        execute: () => {
          void controller.perform({ action: "archive" });
        },
      },
      {
        id: "maild.mailbox.move",
        get title() {
          return m.mailbox_command_move();
        },
        icon: "folder",
        locations: ["menu"],
        order: 51,
        enabledWhen: "online-message-selected",
        execute: () => controller.requestAction("move"),
      },
      {
        id: "maild.mailbox.delete",
        get title() {
          return m.mailbox_command_delete();
        },
        icon: "trash",
        locations: ["toolbar", "menu"],
        order: 60,
        enabledWhen: "online-message-selected",
        execute: () => controller.requestAction("delete"),
      },
      {
        id: "maild.mailbox.refresh",
        get title() {
          return m.mailbox_command_refresh();
        },
        icon: "refresh",
        locations: ["toolbar", "menu"],
        order: 100,
        enabledWhen: "online",
        execute: () => {
          void host.bridge
            .refresh()
            .catch(() => host.notify(m.mailbox_command_refresh_offline()));
          // 先敲醒后台立即检查新邮件（不支持时降级），再刷新当前列表。
          void controller.syncNow();
        },
      },
      {
        id: "maild.mailbox.shortcuts",
        get title() {
          return m.mailbox_shortcut_help_title();
        },
        locations: ["menu"],
        order: 200,
        execute: () => {
          host.openView(MAILBOX_VIEW);
          ui.patch({ help: true });
        },
      },
    ],
    dispose: () => {
      stopState();
      stopIntent();
      stopDocuments();
      documents.stop();
      controller.stop();
      activity.hide();
      window.removeEventListener("keydown", onKey);
    },
  };
}
