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
          error: "同步活动暂时无法读取，请重试。",
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
        error: "裁决结果未确认，请先刷新核对，不要连续重试。",
      });
    } finally {
      this.store.patch({ busy: null });
    }
  }
}
export function stateLabel(state: string) {
  return (
    (
      {
        pending: "等待同步",
        in_flight: "同步中",
        done: "已同步",
        failed: "同步失败",
        unknown: "结果未知",
      } as Record<string, string>
    )[state] ?? "此版本无法处理的状态"
  );
}
export function operationLabel(op: Operation) {
  return op.kind === "move"
    ? "移动邮件"
    : op.kind === "append"
      ? "保存已发送副本"
      : op.kind === "flag"
        ? `${op.value ? "设置" : "取消"}${({ seen: "已读", flagged: "星标", answered: "已回复", deleted: "已删除" } as Record<string, string>)[op.flag ?? ""] ?? "邮件标记"}`
        : "此版本无法处理的操作";
}
