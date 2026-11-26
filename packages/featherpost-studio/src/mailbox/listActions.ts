import type { UiSummary } from "../bridge/types";
import { messageKey, type MailboxController } from "./controller";

export type ListRowSource = "local" | "remote";

export interface ResolvedListRow {
  readonly key: string;
  readonly source: ListRowSource;
  readonly row: UiSummary;
}

export interface StepResult {
  readonly previousKey: string | null;
  readonly nextKey: string | null;
  readonly row: UiSummary | null;
  readonly moved: boolean;
  readonly atBoundary: boolean;
}

export interface CheckRowOptions {
  readonly range?: boolean;
}

export type ListActionController = Pick<
  MailboxController,
  | "state"
  | "selectMessage"
  | "openRemote"
  | "step"
  | "clearSelection"
  | "toggle"
  | "selectLoaded"
>;

export interface MessageListActions {
  /** Resolves a `messageKey` across loaded local rows (first) and remote server-search rows. */
  resolve(key: string): ResolvedListRow | null;
  /** Returns whether `key` is the currently opened message in the reading pane. */
  isSelected(key: string): boolean;
  /** Returns whether `key` is currently bulk-checked for batch operations. */
  isChecked(key: string): boolean;
  /**
   * Opens a message by `messageKey`, routing local rows to `selectMessage`
   * and server-history rows to `openRemote` without touching bulk-checked rows.
   */
  openByKey(key: string): ResolvedListRow | null;
  /** Moves open selection forward (+1) or backward (-1) in the active local or remote list. */
  step(delta: 1 | -1): StepResult;
  /** Clears the reading-pane selection without clearing bulk-checked rows. */
  clearOpenSelection(): void;
  /**
   * Toggles bulk-check state for a local message by `messageKey`.
   * Returns `false` when the key is missing, remote-only, or the controller is busy.
   */
  toggleCheckByKey(key: string, options?: CheckRowOptions): boolean;
  /**
   * Sets bulk-check state for a local message by `messageKey` to a specific boolean.
   * No-ops when already in the target state (unless `range: true` while checking).
   */
  setCheckedByKey(
    key: string,
    checked: boolean,
    options?: CheckRowOptions,
  ): boolean;
  /** Checks or unchecks all currently loaded local rows. */
  setAllLoadedChecked(checked: boolean): void;
}

/**
 * Semantic message-list interaction adapter decoupled from `listScroll.ts`.
 *
 * Keeps reading-pane selection (open) distinct from batch multi-select (check),
 * dispatches both local and remote rows consistently by `messageKey`, and can
 * be shared by UI click handlers, keyboard navigation, and Cordis Host commands.
 */
export function createMessageListActions(
  controller: ListActionController,
): MessageListActions {
  const resolve = (key: string): ResolvedListRow | null => {
    const local = controller.state.rows.find((row) => messageKey(row) === key);
    if (local) return { key, source: "local", row: local };
    const remote = controller.state.remote.rows.find(
      (row) => messageKey(row) === key,
    );
    if (remote) return { key, source: "remote", row: remote };
    return null;
  };

  const isSelected = (key: string): boolean =>
    Boolean(
      controller.state.selected && messageKey(controller.state.selected) === key,
    );

  const isChecked = (key: string): boolean =>
    controller.state.checked.some((item) => messageKey(item) === key);

  const openByKey = (key: string): ResolvedListRow | null => {
    const resolved = resolve(key);
    if (!resolved) return null;
    if (resolved.source === "remote") {
      controller.openRemote(resolved.row);
    } else {
      controller.selectMessage(resolved.row);
    }
    return resolved;
  };

  const step = (delta: 1 | -1): StepResult => {
    const previousKey = controller.state.selected
      ? messageKey(controller.state.selected)
      : null;
    const row = controller.step(delta);
    const nextKey = row ? messageKey(row) : null;
    const moved = nextKey !== null && nextKey !== previousKey;
    const atBoundary = row !== null && previousKey === nextKey;
    return { previousKey, nextKey, row, moved, atBoundary };
  };

  const clearOpenSelection = (): void => {
    controller.clearSelection();
  };

  const toggleCheckByKey = (
    key: string,
    options?: CheckRowOptions,
  ): boolean => {
    if (controller.state.busy) return false;
    const resolved = resolve(key);
    if (!resolved || resolved.source !== "local") return false;
    controller.toggle(resolved.row, Boolean(options?.range));
    return true;
  };

  const setCheckedByKey = (
    key: string,
    checked: boolean,
    options?: CheckRowOptions,
  ): boolean => {
    if (controller.state.busy) return false;
    const resolved = resolve(key);
    if (!resolved || resolved.source !== "local") return false;
    const range = Boolean(options?.range);
    if (!range && isChecked(key) === checked) return true;
    if (!checked) {
      if (!isChecked(key)) return true;
      // `controller.toggle(row, true)` always expands/checks a range, so unchecking
      // must always toggle the individual row without `range`.
      controller.toggle(resolved.row, false);
      return true;
    }
    controller.toggle(resolved.row, range);
    return true;
  };

  const setAllLoadedChecked = (checked: boolean): void => {
    controller.selectLoaded(checked);
  };

  return {
    resolve,
    isSelected,
    isChecked,
    openByKey,
    step,
    clearOpenSelection,
    toggleCheckByKey,
    setCheckedByKey,
    setAllLoadedChecked,
  };
}
