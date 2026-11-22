import { m } from "@featherpost/i18n/messages";

import { useUiLocale } from "../i18n";
import { Dialog } from "./Dialog";
import { SHORTCUTS } from "./shortcuts";

export function ShortcutHelp({ onClose }: { onClose: () => void }) {
  useUiLocale();
  return (
    <Dialog title={m.mailbox_shortcut_help_title()} onClose={onClose} onKey={(e) => e.key === "?" && onClose()}>
      <p>{m.mailbox_shortcut_help_hint()}</p>
      <dl className="shortcut-list">
        {SHORTCUTS.map((s) => (
          <div key={s.action} className="shortcut-row">
            <dt>
              <kbd>{s.keys}</kbd>
            </dt>
            <dd>{s.label()}</dd>
          </div>
        ))}
        <div className="shortcut-row">
          <dt>
            <kbd>⌘K</kbd>
          </dt>
          <dd>{m.mailbox_shortcut_search_mail()}</dd>
        </div>
      </dl>
    </Dialog>
  );
}
