import { useEffect, useRef, type ReactNode } from "react";
import type React from "react";
export function Dialog({
  title,
  children,
  onClose,
  busy = false,
  onKey,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  busy?: boolean;
  /** 对话框内的按键（如「?」再次按下关闭速查）；在阻止冒泡之前调用。 */
  onKey?: (e: React.KeyboardEvent<HTMLDialogElement>) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null),
    close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const previous = document.activeElement;
    ref.current?.showModal();
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  }, []);
  return (
    <dialog
      ref={ref}
      className="mail-dialog"
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) close.current();
      }}
      onKeyDown={(e) => {
        onKey?.(e);
        e.stopPropagation();
      }}
    >
      <header>
        <h2>{title}</h2>
        <button
          type="button"
          className="text-button"
          disabled={busy}
          onClick={onClose}
          aria-label="关闭对话框"
        >
          关闭
        </button>
      </header>
      {children}
    </dialog>
  );
}
