import { m } from "@featherpost/i18n/messages";
import type { Host } from "../../maild-ui/src/host/host";
import type { Bundle } from "../../maild-ui/src/host/types";
import { MailboxController } from "../../maild-ui/src/mailbox/controller";
import { DocumentController } from "../../maild-ui/src/mailbox/document";
import { ActivityController } from "../../maild-ui/src/mailbox/activityController";
import { Activity } from "./Activity";
import { ReadingPrivacySection } from "../../maild-ui/src/mailbox/ReadingPrivacy";
import { followMailIntent } from "../../maild-ui/src/mailbox/navigation";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
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
  // 注册表登记时复制描述符，标题在此时按当前界面语言取值、之后不再变化；
  // Studio 自己渲染的分区与模块名另按 ID 查消息（见 settings/module.tsx 的 localTitle），随语言切换。
  const bundle: Bundle = {
    moduleId: "maild.mailbox",
    apiVersion: 1,
    title: m.studio_module_mailbox(),
    views: [
      {
        id: MAIL,
        title: m.studio_view_mail(),
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
        title: m.studio_view_activity(),
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
        title: m.studio_settings_section_reading_privacy(),
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
