import { m } from "@featherpost/i18n/messages";
import type { Host } from "./host/host";
import type { Bundle } from "./host/types";
import { MailboxController } from "./mailbox/controller";
import { DocumentController } from "./mailbox/document";
import { ActivityController } from "./mailbox/activityController";
import { Activity } from "./Activity";
import { ReadingPrivacySection } from "./mailbox/ReadingPrivacy";
import { followMailIntent } from "./mailbox/navigation";
import type { ComposeController } from "./compose/controller";
import { Workspace } from "./Workspace";
import { MAIL, ACTIVITY } from "./views";
import { groupedBridge } from "./groupQuery";
export { MAIL, ACTIVITY } from "./views";

export function studioMailbox(
  host: Host,
  composer: ComposeController,
  setup: () => void,
) {
  const controller = new MailboxController(
    groupedBridge(host.bridge),
    (selection) => host.setSelection(selection),
  );
  const documents = new DocumentController(host.bridge);
  const activity = new ActivityController(host.bridge.mailbox);
  const stopDocuments = documents.bindTo(controller.store);
  void documents.start();
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
      stopState();
      stopIntent();
      stopDocuments();
      documents.stop();
      controller.stop();
      activity.hide();
    },
  };
  return { controller, bundle, setup };
}
