import { createContext, useContext } from "react";

/** Shell-owned actions that menus deep in the view tree need. */
export interface StudioActions {
  /** Runs a mail search through the top bar so its input and scope stay in sync. */
  search(text: string, all?: boolean): void;
  copy(text: string, what: string): void;
}

export const StudioActionsContext = createContext<StudioActions>({
  search() {},
  copy() {},
});
export const useStudioActions = () => useContext(StudioActionsContext);

/** Clipboard write with a toast; WKWebView allows it inside the menu's click. */
export function clipboardCopier(notify: (text: string) => void) {
  return (text: string, what: string) => {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      notify("无法访问剪贴板");
      return;
    }
    clipboard.writeText(text).then(
      () => notify(`已复制${what}`),
      () => notify("无法写入剪贴板"),
    );
  };
}
