import { m } from "@featherpost/i18n/messages";
import { useEffect } from "react";
import { compatBlocksWrites } from "../bridge/types";
import type { Host } from "../host/host";
import { useStore } from "../host/hooks";
import type { Bundle } from "../host/types";
import { ComposeController, COMPOSE_VIEW, DRAFTS_VIEW } from "./controller";
import { Editor } from "./Editor";
import { fullDate, isoDateTime, shortDate } from "../mailbox/format";
import { bilingualKeywords, uiLocale, useUiLocale } from "../i18n";
import { MAIL } from "../views";

const REPLY_TITLE = {
  reply: m.compose_command_reply,
  reply_all: m.compose_command_reply_all,
  forward: m.compose_command_forward,
} as const;

const REPLY_DESCRIPTION = {
  reply: m.compose_command_reply_description,
  reply_all: m.compose_command_reply_all_description,
  forward: m.compose_command_forward_description,
} as const;

const REPLY_KEYWORDS = {
  reply: m.compose_command_reply_keywords,
  reply_all: m.compose_command_reply_all_keywords,
  forward: m.compose_command_forward_keywords,
} as const;

function draftCount(count: number): string {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? m.compose_drafts_count_one({ count })
    : m.compose_drafts_count_other({ count });
}

export function composeModule(
  host: Host,
  controller: ComposeController,
): Bundle {
  const stop = host.state.subscribe(() =>
    controller.setOnline(host.state.getSnapshot().snapshot.phase === "online"),
  );
  const stopContext = host.registerContextProvider("compose", () => {
    const s = controller.store.getSnapshot();
    return {
      draftId: s.draft?.id ?? null,
      sendState: s.draft?.send_state ?? null,
      inline: s.inline,
      collapsed: s.collapsed,
      dirty: s.revision !== s.savedRevision,
    };
  });
  const intent = host.onIntent((i) => {
    if (i.route === "compose") void controller.create();
  });
  controller.setBeforeNavigate((id, next, cancel) =>
    host.runAfterLeave(id, next, cancel),
  );
  controller.setDropScope(() => {
    const ui = host.state.getSnapshot(),
      compose = controller.store.getSnapshot();
    return (
      ui.activeViewId === COMPOSE_VIEW ||
      (ui.activeViewId === MAIL &&
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
  const resolveReadingTarget = () => {
    const ui = host.state.getSnapshot();
    if (ui.activeViewId !== MAIL) return null;
    if (ui.snapshot.phase !== "online" || compatBlocksWrites(ui.snapshot))
      return null;
    const snap = host.getContextSnapshot();
    if (snap.mailbox?.busy) return null;
    return snap.mailbox ? snap.mailbox.selectedMessage : (ui.selection?.identity ?? null);
  };
  void controller.start();
  function Workspace() {
    return <Editor controller={controller} />;
  }
  function Drafts() {
    useUiLocale();
    const state = useStore(controller.store);
    useEffect(() => {
      void controller.refresh();
    }, []);
    return (
      <section className="page drafts-page">
        <header className="compose-heading" data-tauri-drag-region="deep">
          <div className="compose-title">
            <h1>{m.compose_drafts_title()}</h1>
            <span className="compose-save">{draftCount(state.draftTotal)}</span>
          </div>
          <button
            className="text-button"
            onClick={() => void controller.refresh()}
          >
            {m.compose_drafts_refresh()}
          </button>
        </header>
        <div className="page-scroll">
          <div className="page-column">
            <p className="group-note">
              {m.compose_drafts_note()}
            </p>
            {state.drafts.length === 0 ? (
              <div className="empty">
                <h2>{m.compose_drafts_empty_title()}</h2>
                <p>{m.compose_drafts_empty_body()}</p>
                <button
                  className="primary-button"
                  onClick={() => void controller.create()}
                >
                  {m.compose_command_new()}
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
                      <strong>{d.subject || m.compose_draft_no_subject()}</strong>
                      <span>
                        {d.to.join(", ") || m.compose_draft_no_recipients()} ·{" "}
                        {d.account}
                      </span>
                      <p>{d.snippet || m.compose_draft_empty()}</p>
                    </div>
                    <time
                      dateTime={isoDateTime(d.updated_at)}
                      title={fullDate(d.updated_at)}
                    >
                      {shortDate(d.updated_at)}
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
                {m.compose_drafts_more()}
              </button>
            )}
          </div>
        </div>
      </section>
    );
  }
  const onKey = (e: KeyboardEvent) => {
    if (
      typeof document !== "undefined" &&
      document.querySelector('dialog[open], [aria-modal="true"]')
    )
      return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") {
      e.preventDefault();
      void controller.create();
    }
  };
  window.addEventListener("keydown", onKey);
  return {
    moduleId: "maild.compose",
    apiVersion: 1,
    // 标题写成 getter：注册表保留访问器，切换语言后按当前语言重新成文。
    get title() {
      return m.compose_module_title();
    },
    views: [
      {
        id: COMPOSE_VIEW,
        get title() {
          return m.compose_view_compose();
        },
        role: "compose",
        render: Workspace,
      },
      {
        id: DRAFTS_VIEW,
        get title() {
          return m.compose_view_drafts();
        },
        role: "drafts",
        render: Drafts,
      },
    ],
    commands: [
      {
        id: "maild.compose.new",
        get title() {
          return m.compose_command_new();
        },
        get description() {
          return m.compose_command_new_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.compose_command_new,
            m.compose_command_new_keywords,
          );
        },
        shortcut: "mod+n",
        risk: "stage",
        icon: "drafts",
        locations: ["navigation", "palette"],
        order: 0,
        enabledWhen: "online",
        execute: () => {
          void controller.create();
        },
      },
      {
        id: "maild.compose.open-drafts",
        get title() {
          return m.compose_drafts_title();
        },
        get description() {
          return m.compose_command_open_drafts_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.compose_drafts_title,
            m.compose_command_open_drafts_keywords,
          );
        },
        risk: "read",
        icon: "drafts",
        locations: ["navigation", "palette"],
        order: 10,
        execute: () => host.openView(DRAFTS_VIEW),
      },
      ...(["reply", "reply_all", "forward"] as const).map((mode, index) => ({
        id: `maild.compose.${mode.replaceAll("_", "-")}`,
        get title() {
          return REPLY_TITLE[mode]();
        },
        get description() {
          return REPLY_DESCRIPTION[mode]();
        },
        get keywords() {
          return bilingualKeywords(REPLY_TITLE[mode], REPLY_KEYWORDS[mode]);
        },
        risk: "stage" as const,
        icon: mode === "forward" ? ("forward" as const) : ("reply" as const),
        locations: ["toolbar", "menu", "palette"] as const,
        order: 10 + index,
        enabledWhen: "online-message-selected" as const,
        canExecute: () => resolveReadingTarget() !== null,
        execute: (context: import("../host/types").CommandContext) => {
          const ui = host.state.getSnapshot();
          if (ui.activeViewId !== MAIL) return;
          if (ui.snapshot.phase !== "online" || compatBlocksWrites(ui.snapshot)) {
            host.notify(m.compose_error_offline());
            return;
          }
          const target = resolveReadingTarget();
          if (!target) return;
          if (
            context.selection &&
            (context.selection.account !== target.account ||
              context.selection.folder !== target.folder ||
              context.selection.uid !== target.uid)
          ) {
            return;
          }
          void controller.reply(target, mode);
        },
      })),
    ],
    dispose: () => {
      controller.setBeforeNavigate((_id, next) => {
        next();
        return true;
      });
      stopContext();
      stop();
      intent();
      controller.stop();
      window.removeEventListener("keydown", onKey);
    },
  };
}
