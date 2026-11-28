import { m } from "@featherpost/i18n/messages";
import { compatBlocksWrites, type UiSummary } from "./bridge/types";
import type { Host } from "./host/host";
import type { Bundle, HostScopeSummary } from "./host/types";
import { bilingualKeywords } from "./i18n";
import { MailboxController, messageKey } from "./mailbox/controller";
import type { Scope } from "./mailbox/types";
import { DocumentController } from "./mailbox/document";
import { TranslationController } from "./mailbox/translationController";
import { ActivityController } from "./mailbox/activityController";
import { Activity } from "./Activity";
import { ReadingPrivacySection } from "./mailbox/ReadingPrivacy";
import { followMailIntent } from "./mailbox/navigation";
import type { ComposeController } from "./compose/controller";
import { Workspace } from "./Workspace";
import { MAIL, ACTIVITY } from "./views";
import { groupedBridge } from "./groupQuery";
import { STUDIO_SHORTCUT_KEYS } from "./shortcuts";
export { MAIL, ACTIVITY } from "./views";

function summarizeScope(scope: Scope): HostScopeSummary {
  if (scope.kind === "unread") {
    const base = summarizeScope(scope.base);
    return {
      ...base,
      kind: "unread",
      unreadOnly: true,
    };
  }
  if (scope.kind === "folder") {
    return {
      kind: "folder",
      account: scope.account,
      folder: scope.folder,
      unreadOnly: false,
    };
  }
  if (scope.kind === "search") {
    return {
      kind: "search",
      ...(scope.query.account ? { account: scope.query.account } : {}),
      ...(scope.query.accounts ? { accounts: [...scope.query.accounts] } : {}),
      ...(scope.query.folder ? { folder: scope.query.folder } : {}),
      ...(scope.query.query?.trim()
        ? { searchQuery: scope.query.query.trim() }
        : {}),
    };
  }
  return {
    kind: "smart",
    smartFolder: scope.name,
    ...(scope.group
      ? {
          groupId: scope.group.id,
          groupName: scope.group.name,
          accounts: [...scope.group.accounts],
        }
      : {}),
    unreadOnly: scope.name === "unread",
  };
}

export function studioMailbox(
  host: Host,
  composer: ComposeController,
  setup: () => void,
  openSettingsSection?: (sectionId: string) => void,
) {
  const controller = new MailboxController(
    groupedBridge(host.bridge),
    (selection) => host.setSelection(selection),
  );
  composer.setContactSource({
    fallbackRows: (account) => {
      const mState = controller.store.getSnapshot();
      const rows = [
        ...mState.rows,
        ...mState.threadRows,
        ...mState.remote.rows,
        ...(mState.selectedRow ? [mState.selectedRow] : []),
        ...(mState.detail ? [mState.detail] : []),
      ];
      return rows.filter((row) => row.account === account);
    },
  });
  const isUnfetchedRemote = (row: UiSummary) =>
    controller.isRemote(row) &&
    !controller.state.remote.fetched.includes(messageKey(row));
  const resolveFreshTargets = (): UiSummary[] => {
    const s = controller.state;
    const rows = controller.targets();
    if (!rows.length) return [];
    const currentRows = new Map(
      [
        ...s.rows,
        ...s.threadRows,
        ...(s.selectedRow ? [s.selectedRow] : []),
      ].map((row) => [messageKey(row), row]),
    );
    return rows
      .map((row) => currentRows.get(messageKey(row)))
      .filter((row): row is UiSummary => Boolean(row && !isUnfetchedRemote(row)));
  };
  const isMailboxActive = () => host.state.getSnapshot().activeViewId === MAIL;
  const isWritable = () => {
    const s = controller.state;
    return (
      s.snapshot.phase === "online" &&
      !compatBlocksWrites(s.snapshot) &&
      !s.busy
    );
  };
  const resolveActiveTargets = (): UiSummary[] =>
    isMailboxActive() ? resolveFreshTargets() : [];
  const canMoveRows = (rows: readonly UiSummary[]) =>
    rows.length > 0 && new Set(rows.map((row) => row.account)).size === 1;
  const canToggleThread = () => {
    if (!isMailboxActive()) return false;
    const s = controller.state;
    if (s.threadOpen) return true;
    return Boolean(
      s.selectedRow &&
        s.selected &&
        messageKey(s.selectedRow) === messageKey(s.selected) &&
        s.snapshot.phase === "online" &&
        !s.busy,
    );
  };
  const openMailboxAndRun = (action: () => void) => {
    const current = controller.navigationGuard();
    host.runAfterLeave(MAIL, () => {
      if (!current()) return;
      if (host.openView(MAIL)) action();
    });
  };
  const stopContext = host.registerContextProvider("mailbox", () => {
    const s = controller.store.getSnapshot();
    const threadId =
      s.detail?.thread_id ??
      s.selectedRow?.thread_id ??
      (s.threadOpen && s.threadId > 0 ? s.threadId : null);
    return {
      scope: summarizeScope(s.scope),
      selectedMessage: s.selected
        ? {
            account: s.selected.account,
            folder: s.selected.folder,
            uid: s.selected.uid,
          }
        : null,
      selectedThreadId: threadId && threadId > 0 ? threadId : null,
      checkedMessages: s.checked.map(({ account, folder, uid }) => ({
        account,
        folder,
        uid,
      })),
      threadOpen: s.threadOpen,
      totalMessages: s.total,
      busy: s.busy,
      targetMessages: resolveFreshTargets().map(({ account, folder, uid }) => ({
        account,
        folder,
        uid,
      })),
    };
  });
  const documents = new DocumentController(host.bridge);
  const translation = new TranslationController(host.bridge);
  const activity = new ActivityController(host.bridge.mailbox);
  const stopDocuments = documents.bindTo(controller.store);
  const stopTranslation = translation.bindTo(
    controller.store,
    documents.store,
    host.state,
  );
  void documents.start();
  void translation.start();
  const openTranslationSettings = () => {
    if (openSettingsSection) {
      openSettingsSection("maild.settings.translation");
      return;
    }
    host.openRole("settings");
  };
  const canTranslateSelected = () => {
    if (!isMailboxActive() || !translation.available) return false;
    const s = controller.state;
    return Boolean(
      s.selectedRow &&
        s.selected &&
        s.detail &&
        messageKey(s.selectedRow) === messageKey(s.selected),
    );
  };
  let seen = { revision: -2, epoch: -1 };
  let lastView = host.state.getSnapshot().activeViewId;
  const sync = () => {
    const s = host.state.getSnapshot();
    if (s.activeViewId !== lastView) {
      lastView = s.activeViewId;
      controller.invalidateNavigation();
    }
    if (
      s.snapshot.revision === seen.revision &&
      s.snapshot.connection_epoch === seen.epoch
    )
      return;
    seen = {
      revision: s.snapshot.revision,
      epoch: s.snapshot.connection_epoch,
    };
    controller.acceptSnapshot(s.snapshot);
    activity.setConnection(
      s.snapshot.phase === "online",
      s.snapshot.connection_epoch,
    );
  };
  const stopState = host.state.subscribe(sync);
  const stopIntent = host.onIntent((intent) => {
    if (intent.route === "background") host.openView(ACTIVITY);
    if (intent.route === "inbox" || intent.route === "message")
      void followMailIntent(
        controller,
        intent,
        () => composer.flush(),
        () => host.openView(MAIL),
        (text) => host.notify(text),
        () => host.approveNavigation(MAIL),
      );
  });
  // 标题写成 getter：注册表保留访问器（copyContribution），切换界面语言后按当前语言重新成文。
  const bundle: Bundle = {
    moduleId: "maild.mailbox",
    apiVersion: 1,
    get title() {
      return m.studio_module_mailbox();
    },
    commands: [
      {
        id: "maild.mailbox.sync",
        get title() {
          return m.mailbox_command_sync();
        },
        get description() {
          return m.mailbox_command_sync_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_sync,
            m.mailbox_command_sync_keywords,
          );
        },
        risk: "read",
        icon: "refresh",
        locations: ["palette"],
        order: 10,
        enabledWhen: "online",
        canExecute: () =>
          controller.state.snapshot.phase === "online" &&
          !compatBlocksWrites(controller.state.snapshot) &&
          !controller.isSyncPending(),
        execute: () => {
          if (
            controller.state.snapshot.phase !== "online" ||
            compatBlocksWrites(controller.state.snapshot) ||
            controller.isSyncPending()
          )
            return;
          void controller.syncNow();
        },
      },
      {
        id: "maild.mailbox.open-inbox",
        get title() {
          return m.mailbox_command_open_inbox();
        },
        get description() {
          return m.mailbox_command_open_inbox_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_open_inbox,
            m.mailbox_command_open_inbox_keywords,
          );
        },
        risk: "read",
        icon: "inbox",
        locations: ["navigation", "palette"],
        order: 20,
        execute: () => openMailboxAndRun(() => controller.selectSmart("inbox")),
      },
      {
        id: "maild.mailbox.open-starred",
        get title() {
          return m.mailbox_command_open_starred();
        },
        get description() {
          return m.mailbox_command_open_starred_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_open_starred,
            m.mailbox_command_open_starred_keywords,
          );
        },
        risk: "read",
        icon: "star",
        locations: ["navigation", "palette"],
        order: 30,
        execute: () => openMailboxAndRun(() => controller.selectSmart("starred")),
      },
      {
        id: "maild.mailbox.open-activity",
        get title() {
          return m.mailbox_command_open_activity();
        },
        get description() {
          return m.mailbox_command_open_activity_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_open_activity,
            m.mailbox_command_open_activity_keywords,
          );
        },
        risk: "read",
        icon: "activity",
        locations: ["navigation", "palette"],
        order: 40,
        execute: () => {
          host.openView(ACTIVITY);
        },
      },
      {
        id: "maild.mailbox.filter-all",
        get title() {
          return m.mailbox_command_filter_all();
        },
        get description() {
          return m.mailbox_command_filter_all_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_filter_all,
            m.mailbox_command_filter_all_keywords,
          );
        },
        risk: "read",
        icon: "mail",
        locations: ["palette"],
        order: 50,
        execute: () =>
          openMailboxAndRun(() => {
            if (controller.state.scope.kind === "search")
              controller.exitSearch();
            controller.setUnreadFilter(false);
          }),
      },
      {
        id: "maild.mailbox.filter-unread",
        get title() {
          return m.mailbox_command_filter_unread();
        },
        get description() {
          return m.mailbox_command_filter_unread_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_filter_unread,
            m.mailbox_command_filter_unread_keywords,
          );
        },
        risk: "read",
        icon: "mail",
        locations: ["palette"],
        order: 60,
        execute: () =>
          openMailboxAndRun(() => {
            if (controller.state.scope.kind === "search")
              controller.exitSearch();
            controller.setUnreadFilter(true);
          }),
      },
      {
        id: "maild.mailbox.toggle-read",
        get title() {
          const rows = resolveActiveTargets();
          if (!rows.length) return m.mailbox_command_toggle_read();
          return rows.some((row) => !row.seen)
            ? m.mailbox_command_mark_read()
            : m.mailbox_command_mark_unread();
        },
        get description() {
          return m.mailbox_command_toggle_read_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_toggle_read,
            m.mailbox_command_toggle_read_keywords,
          );
        },
        risk: "write",
        icon: "mail",
        locations: ["palette"],
        order: 110,
        enabledWhen: "online-targets",
        canExecute: () =>
          isMailboxActive() &&
          isWritable() &&
          resolveActiveTargets().length > 0,
        execute: () => {
          if (!isMailboxActive() || !isWritable()) return;
          const rows = resolveActiveTargets();
          if (!rows.length) return;
          const markRead = rows.some((row) => !row.seen);
          void controller.perform(
            { action: "mark", flag: "seen", value: markRead },
            rows,
          );
        },
      },
      {
        id: "maild.mailbox.toggle-star",
        get title() {
          const rows = resolveActiveTargets();
          if (!rows.length) return m.mailbox_command_toggle_star();
          return rows.some((row) => !row.flagged)
            ? m.mailbox_command_star()
            : m.mailbox_command_unstar();
        },
        get description() {
          return m.mailbox_command_toggle_star_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_toggle_star,
            m.mailbox_command_toggle_star_keywords,
          );
        },
        risk: "write",
        icon: "star",
        locations: ["palette"],
        order: 120,
        enabledWhen: "online-targets",
        canExecute: () =>
          isMailboxActive() &&
          isWritable() &&
          resolveActiveTargets().length > 0,
        execute: () => {
          if (!isMailboxActive() || !isWritable()) return;
          const rows = resolveActiveTargets();
          if (!rows.length) return;
          const star = rows.some((row) => !row.flagged);
          void controller.perform(
            { action: "mark", flag: "flagged", value: star },
            rows,
          );
        },
      },
      {
        id: "maild.mailbox.archive",
        get title() {
          return m.mailbox_command_archive();
        },
        get description() {
          return m.mailbox_command_archive_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_archive,
            m.mailbox_command_archive_keywords,
          );
        },
        risk: "write",
        icon: "archive",
        locations: ["palette"],
        order: 130,
        enabledWhen: "online-targets",
        canExecute: () =>
          isMailboxActive() &&
          isWritable() &&
          resolveActiveTargets().length > 0,
        execute: () => {
          if (!isMailboxActive() || !isWritable()) return;
          const rows = resolveActiveTargets();
          if (!rows.length) return;
          void controller.perform({ action: "archive" }, rows);
        },
      },
      {
        id: "maild.mailbox.move",
        get title() {
          return m.mailbox_command_move();
        },
        get description() {
          return m.mailbox_command_move_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_move,
            m.mailbox_command_move_keywords,
          );
        },
        risk: "write",
        icon: "folder",
        locations: ["palette"],
        order: 140,
        enabledWhen: "online-targets",
        canExecute: () =>
          isMailboxActive() &&
          isWritable() &&
          canMoveRows(resolveActiveTargets()),
        execute: () => {
          if (!isMailboxActive() || !isWritable()) return;
          const rows = resolveActiveTargets();
          if (!canMoveRows(rows)) return;
          controller.requestAction("move", rows);
        },
      },
      {
        id: "maild.mailbox.delete",
        get title() {
          return m.mailbox_command_delete();
        },
        get description() {
          return m.mailbox_command_delete_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_delete,
            m.mailbox_command_delete_keywords,
          );
        },
        risk: "destructive",
        icon: "trash",
        locations: ["palette"],
        order: 150,
        enabledWhen: "online-targets",
        canExecute: () =>
          isMailboxActive() &&
          isWritable() &&
          resolveActiveTargets().length > 0,
        execute: () => {
          if (!isMailboxActive() || !isWritable()) return;
          const rows = resolveActiveTargets();
          if (!rows.length) return;
          controller.requestAction("delete", rows);
        },
      },
      {
        id: "maild.mailbox.translate",
        get title() {
          const phase = translation.store.getSnapshot().phase;
          if (phase === "translating") return m.mailbox_command_translate_cancel();
          if (phase === "translated") return m.mailbox_command_translate_original();
          return m.mailbox_command_translate();
        },
        get description() {
          return m.mailbox_command_translate_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_translate,
            m.mailbox_command_translate_keywords,
          );
        },
        shortcut: STUDIO_SHORTCUT_KEYS.translateMessage,
        risk: "read",
        icon: "translate",
        locations: ["palette"],
        order: 155,
        enabledWhen: "message-selected",
        canExecute: canTranslateSelected,
        execute: () => {
          if (!canTranslateSelected()) return;
          void translation.toggleTranslate(openTranslationSettings);
        },
      },
      {
        id: "maild.mailbox.retranslate",
        get title() {
          return m.mailbox_command_retranslate();
        },
        get description() {
          return m.mailbox_command_retranslate_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_retranslate,
            m.mailbox_command_retranslate_keywords,
          );
        },
        risk: "read",
        icon: "translate",
        locations: ["palette"],
        order: 156,
        enabledWhen: "message-selected",
        canExecute: canTranslateSelected,
        execute: () => {
          if (!canTranslateSelected()) return;
          void translation.retranslate(openTranslationSettings);
        },
      },
      {
        id: "maild.mailbox.toggle-thread",
        get title() {
          return controller.state.threadOpen
            ? m.mailbox_thread_collapse()
            : m.mailbox_thread_expand();
        },
        get description() {
          return m.mailbox_command_toggle_thread_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_toggle_thread,
            m.mailbox_command_toggle_thread_keywords,
          );
        },
        risk: "read",
        icon: "mail",
        locations: ["palette"],
        order: 160,
        enabledWhen: "message-selected",
        canExecute: canToggleThread,
        execute: () => {
          if (!canToggleThread()) return;
          void controller.openThread();
        },
      },
      {
        id: "maild.mailbox.clear-checked",
        get title() {
          return m.mailbox_command_clear_checked();
        },
        get description() {
          return m.mailbox_command_clear_checked_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.mailbox_command_clear_checked,
            m.mailbox_command_clear_checked_keywords,
          );
        },
        risk: "read",
        icon: "mail",
        locations: ["palette"],
        order: 170,
        enabledWhen: "checked-messages",
        canExecute: () =>
          isMailboxActive() &&
          !controller.state.busy &&
          controller.state.checked.length > 0,
        execute: () => {
          if (
            !isMailboxActive() ||
            controller.state.busy ||
            controller.state.checked.length === 0
          )
            return;
          controller.selectLoaded(false);
        },
      },
    ],
    views: [
      {
        id: MAIL,
        get title() {
          return m.studio_view_mail();
        },
        role: "mailbox",
        render: () => (
          <Workspace
            host={host}
            controller={controller}
            documents={documents}
            composer={composer}
            setup={setup}
            translation={translation}
          />
        ),
      },
      {
        id: ACTIVITY,
        get title() {
          return m.studio_view_activity();
        },
        role: "activity",
        render: () => (
          <Activity
            controller={activity}
            mailbox={controller}
            openMailbox={() => host.openView(MAIL)}
          />
        ),
      },
    ],
    settings: [
      {
        id: "maild.mailbox.reading-privacy",
        get title() {
          return m.studio_settings_section_reading_privacy();
        },
        order: 15,
        render: () => (
          <ReadingPrivacySection
            host={host}
            onChanged={() => controller.reloadSelected()}
          />
        ),
      },
    ],
    dispose: () => {
      stopContext();
      stopState();
      stopIntent();
      stopTranslation();
      stopDocuments();
      translation.stop();
      documents.stop();
      controller.stop();
      activity.hide();
    },
  };
  return { controller, translation, bundle, setup };
}
