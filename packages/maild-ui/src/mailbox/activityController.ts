import { m } from "@featherpost/i18n/messages";

import type { MailboxBridge, Operation } from "./types";
import { Store } from "../host/store";
import { RequestGeneration } from "./controller";
export interface ActivityState {
  ops: Operation[];
  account: string;
  more: boolean;
  loading: boolean;
  error: string | null;
  busy: number | null;
  online: boolean;
}
export class ActivityController {
  readonly store = new Store<ActivityState>({
    ops: [],
    account: "",
    more: false,
    loading: false,
    error: null,
    busy: null,
    online: false,
  });
  private requests = new RequestGeneration();
  private timer: ReturnType<typeof setInterval> | null = null;
  private visible = false;
  private epoch = 0;
  constructor(private bridge: MailboxBridge) {}
  setConnection(online: boolean, epoch: number) {
    const changed = epoch !== this.epoch;
    this.epoch = epoch;
    if (changed || !online) {
      this.requests.invalidate();
      this.store.patch({ loading: false });
    }
    const before = this.store.getSnapshot().online;
    this.store.patch({ online });
    if (online && (!before || changed) && this.visible) void this.refresh();
  }
  show() {
    this.visible = true;
    void this.refresh();
    if (!this.timer)
      this.timer = setInterval(() => {
        const s = this.store.getSnapshot();
        if (!s.loading && s.busy === null && s.ops.length <= 40)
          void this.refresh();
      }, 5000);
  }
  hide() {
    this.visible = false;
    this.requests.invalidate();
    this.store.patch({ loading: false });
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }
  filter(account: string) {
    this.requests.invalidate();
    this.store.patch({
      account,
      ops: [],
      more: false,
      loading: false,
      error: null,
    });
    void this.refresh();
  }
  async refresh(append = false) {
    const s = this.store.getSnapshot();
    if (!s.online || s.loading || s.busy !== null) return;
    const current = this.requests.begin();
    this.store.patch({ loading: true, error: null });
    try {
      const before = append ? s.ops.at(-1)?.id : undefined;
      const page = await this.bridge.activity(s.account || undefined, before);
      if (!current()) return;
      if (
        page.ops.some(
          (o) =>
            (s.account && o.account !== s.account) ||
            (before !== undefined && o.id >= before),
        )
      )
        throw new Error("scope mismatch");
      const ops = append
        ? [...new Map([...s.ops, ...page.ops].map((o) => [o.id, o])).values()]
        : page.ops;
      this.store.patch({ ops, more: page.has_more, loading: false });
    } catch {
      if (current())
        this.store.patch({
          loading: false,
          error: m.mailbox_activity_error_load(),
        });
    }
  }
  async resolve(op: Operation, resolution: "retry" | "discard") {
    const s = this.store.getSnapshot(),
      epoch = this.epoch;
    if (
      !s.online ||
      s.busy !== null ||
      !["flag", "move", "append"].includes(op.kind) ||
      !["failed", "unknown"].includes(op.state)
    )
      return;
    this.requests.invalidate();
    this.store.patch({ busy: op.id, loading: false, error: null });
    try {
      const updated = await this.bridge.resolve(op.id, resolution);
      if (updated.id !== op.id) throw new Error("id mismatch");
      if (this.epoch === epoch)
        this.store.patch({
          ops: this.store
            .getSnapshot()
            .ops.map((o) => (o.id === updated.id ? updated : o)),
        });
    } catch {
      this.store.patch({
        error: m.mailbox_activity_error_resolve(),
      });
    } finally {
      this.store.patch({ busy: null });
    }
  }
}
const STATE_LABELS: Record<string, () => string> = {
  pending: m.mailbox_activity_state_pending,
  in_flight: m.mailbox_activity_state_in_flight,
  done: m.mailbox_activity_state_done,
  failed: m.mailbox_activity_state_failed,
  unknown: m.mailbox_activity_state_unknown,
};
export function stateLabel(state: string) {
  const label = Object.prototype.hasOwnProperty.call(STATE_LABELS, state)
    ? STATE_LABELS[state]
    : undefined;
  return label ? label() : m.mailbox_activity_state_unsupported();
}
/** 标记操作按「设置 / 取消」× 标记名各给一条消息，英文才能各自成句。 */
const FLAG_LABELS: Record<string, { set: () => string; clear: () => string }> = {
  seen: { set: m.mailbox_activity_flag_seen_set, clear: m.mailbox_activity_flag_seen_clear },
  flagged: { set: m.mailbox_activity_flag_flagged_set, clear: m.mailbox_activity_flag_flagged_clear },
  answered: { set: m.mailbox_activity_flag_answered_set, clear: m.mailbox_activity_flag_answered_clear },
  deleted: { set: m.mailbox_activity_flag_deleted_set, clear: m.mailbox_activity_flag_deleted_clear },
};
const OTHER_FLAG = { set: m.mailbox_activity_flag_other_set, clear: m.mailbox_activity_flag_other_clear };
export function operationLabel(op: Operation) {
  if (op.kind === "move") return m.mailbox_activity_operation_move();
  if (op.kind === "append") return m.mailbox_activity_operation_append();
  if (op.kind !== "flag") return m.mailbox_activity_operation_unsupported();
  const flag = op.flag ?? "";
  const labels = Object.prototype.hasOwnProperty.call(FLAG_LABELS, flag)
    ? FLAG_LABELS[flag]
    : OTHER_FLAG;
  return op.value ? labels.set() : labels.clear();
}
