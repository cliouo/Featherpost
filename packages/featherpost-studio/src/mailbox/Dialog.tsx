import type React from "react";
import type { ReactNode, RefObject } from "react";

import { StudioDialog, type StudioDialogRole } from "../app/Dialog";

export function Dialog({
  title,
  children,
  onClose,
  busy = false,
  onKey,
  role = "dialog",
  dismissOnOutsideClick,
  initialFocusRef,
}: {
  title: string;
  children?: ReactNode;
  onClose: () => void;
  busy?: boolean;
  /** 对话框内的按键（如「?」再次按下关闭速查）；在阻止冒泡之前调用。 */
  onKey?: (e: React.KeyboardEvent<HTMLElement>) => void;
  role?: StudioDialogRole;
  dismissOnOutsideClick?: boolean;
  initialFocusRef?: RefObject<HTMLElement | null>;
}) {
  return (
    <StudioDialog
      title={title}
      onClose={onClose}
      busy={busy}
      onKeyDown={onKey}
      role={role}
      variant="dialog"
      showCloseButton
      dismissOnOutsideClick={dismissOnOutsideClick}
      initialFocusRef={initialFocusRef}
    >
      {children}
    </StudioDialog>
  );
}
