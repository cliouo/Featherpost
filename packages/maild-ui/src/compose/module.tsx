import { useEffect } from "react";
import type { Host } from "../host/host";
import { useStore } from "../host/hooks";
import type { Bundle } from "../host/types";
import { ComposeController, COMPOSE_VIEW, DRAFTS_VIEW } from "./controller";
import { Editor } from "./Editor";
export function composeModule(
  host: Host,
  controller: ComposeController,
): Bundle {
  const stop = host.state.subscribe(() =>
    controller.setOnline(host.state.getSnapshot().snapshot.phase === "online"),
  );
  const intent = host.onIntent((i) => {
    if (i.route === "compose") void controller.create();
  });
  controller.setDropScope(() => {
    const ui = host.state.getSnapshot(),
      compose = controller.store.getSnapshot();
    return (
      ui.activeViewId === COMPOSE_VIEW ||
      (ui.activeViewId === "maild.mailbox.workspace" &&
        compose.inline &&
        !compose.collapsed &&
        compose.source ===
          JSON.stringify(
            ui.selection
              ? [
                  ui.selection.identity.account,
                  ui.selection.identity.folder,
                  ui.selection.identity.uid,
                ]
              : null,
          ))
    );
  });
  void controller.start();
  function Workspace() {
    return <Editor controller={controller} />;
  }
  function Drafts() {
    const state = useStore(controller.store);
    useEffect(() => {
      void controller.refresh();
    }, []);
    return (
      <section className="page drafts-page">
        <header className="compose-heading">
          <div className="compose-title">
            <h1>本机草稿</h1>
            <span className="compose-save">{state.draftTotal} 份</span>
          </div>
          <button
            className="text-button"
            onClick={() => void controller.refresh()}
          >
            刷新
          </button>
        </header>
        <div className="page-scroll">
          <div className="page-column">
            <p className="group-note">
              草稿保存在这台电脑的后台，关闭窗口后仍可继续编辑。尚不同步到其他设备。
            </p>
            {state.drafts.length === 0 ? (
              <div className="empty">
                <h2>还没有草稿</h2>
                <p>写到一半的邮件会保留在这里。</p>
                <button
                  className="primary-button"
                  onClick={() => void controller.create()}
                >
                  写邮件
                </button>
              </div>
            ) : (
              <div className="draft-list">
                {state.drafts.map((d) => (
                  <button
                    key={d.id}
                    className="draft-row"
                    onClick={() => void controller.open(d.id)}
                  >
                    <div>
                      <strong>{d.subject || "（无主题）"}</strong>
                      <span>
                        {d.to.join(", ") || "尚无收件人"} · {d.account}
                      </span>
                      <p>{d.snippet || "空白草稿"}</p>
                    </div>
                    <time>
                      {new Date(d.updated_at * 1000).toLocaleDateString()}
                    </time>
                  </button>
                ))}
              </div>
            )}
            {state.drafts.length < state.draftTotal && (
              <button
                className="text-button"
                onClick={() => void controller.moreDrafts()}
              >
                加载更多草稿
              </button>
            )}
          </div>
        </div>
      </section>
    );
  }
  const onKey = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") {
      e.preventDefault();
      void controller.create();
    }
  };
  window.addEventListener("keydown", onKey);
  return {
    moduleId: "maild.compose",
    apiVersion: 1,
    title: "写信与草稿",
    views: [
      { id: COMPOSE_VIEW, title: "写信", role: "compose", render: Workspace },
      { id: DRAFTS_VIEW, title: "草稿", role: "drafts", render: Drafts },
    ],
    commands: [
      {
        id: "maild.compose.new",
        title: "写邮件",
        icon: "drafts",
        locations: ["navigation"],
        order: 0,
        enabledWhen: "online",
        execute: () => {
          void controller.create();
        },
      },
      {
        id: "maild.compose.open-drafts",
        title: "本机草稿",
        icon: "drafts",
        locations: ["navigation"],
        order: 10,
        execute: () => host.openView(DRAFTS_VIEW),
      },
      ...(["reply", "reply_all", "forward"] as const).map((mode, index) => ({
        id: `maild.compose.${mode.replaceAll("_", "-")}`,
        title: ["回复", "全部回复", "转发"][index]!,
        icon: mode === "forward" ? ("forward" as const) : ("reply" as const),
        locations: ["toolbar", "menu"] as const,
        order: 10 + index,
        enabledWhen: "online-message-selected" as const,
        execute: (context: import("../host/types").CommandContext) => {
          if (context.online && context.selection)
            void controller.reply(context.selection, mode);
          else host.notify("后台连接不可用。");
        },
      })),
    ],
    dispose: () => {
      stop();
      intent();
      controller.stop();
      window.removeEventListener("keydown", onKey);
    },
  };
}
