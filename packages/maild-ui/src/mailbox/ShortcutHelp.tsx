import { Dialog } from "./Dialog";
import { SHORTCUTS } from "./shortcuts";

export function ShortcutHelp({ onClose }: { onClose: () => void }) {
  return (
    <Dialog title="键盘快捷键" onClose={onClose} onKey={(e) => e.key === "?" && onClose()}>
      <p>焦点不在输入框时可用。</p>
      <dl className="shortcut-list">
        {SHORTCUTS.map((s) => (
          <div key={s.action} className="shortcut-row">
            <dt>
              <kbd>{s.keys}</kbd>
            </dt>
            <dd>{s.label}</dd>
          </div>
        ))}
        <div className="shortcut-row">
          <dt>
            <kbd>⌘K</kbd>
          </dt>
          <dd>搜索邮件</dd>
        </div>
      </dl>
    </Dialog>
  );
}
