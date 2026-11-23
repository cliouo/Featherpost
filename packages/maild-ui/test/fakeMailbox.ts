import type {
  MailboxBridge,
  MailboxQuery,
  Operation,
  Receipt,
  ThreadPage,
  WriteRequest,
  ActivityPage,
  Profile,
} from "../src/mailbox/types";
import type { MessageIdentity, UiMessage, UiPage } from "../src/bridge/types";
import { deferred } from "./fakeBridge";
export class FakeMailbox implements MailboxBridge {
  queries: Array<{
    query: MailboxQuery;
    offset: number;
    d: ReturnType<typeof deferred<UiPage>>;
  }> = [];
  threads: Array<{
    identity: MessageIdentity;
    offset: number;
    d: ReturnType<typeof deferred<ThreadPage>>;
  }> = [];
  writes: WriteRequest[] = [];
  writeHook: ((request: WriteRequest) => Promise<Receipt>) | null = null;
  activities: Array<{
    account?: string;
    before?: number;
    d: ReturnType<typeof deferred<ActivityPage>>;
  }> = [];
  resolutions: Array<{ id: number; resolution: string }> = [];
  resolveHook:
    | ((id: number, resolution: "retry" | "discard") => Promise<Operation>)
    | null = null;
  profileRows: Profile[] = [];
  // 可选能力默认缺席（= 旧后台）；测试按需挂上。
  syncNow?: MailboxBridge["syncNow"];
  syncScope?: MailboxBridge["syncScope"];
  serverSearch?: MailboxBridge["serverSearch"];
  fetchServerMessage?: MailboxBridge["fetchServerMessage"];
  indexStatus = { pending: 0, done: true };
  async profiles() {
    return this.profileRows;
  }
  query(query: MailboxQuery, offset: number) {
    const d = deferred<UiPage>();
    this.queries.push({ query, offset, d });
    return d.promise;
  }
  thread(identity: MessageIdentity, offset: number) {
    const d = deferred<ThreadPage>();
    this.threads.push({ identity, offset, d });
    return d.promise;
  }
  async locate(_identity: MessageIdentity): Promise<UiMessage> {
    throw new Error("not found");
  }
  async index() {
    return this.indexStatus;
  }
  async write(request: WriteRequest) {
    this.writes.push(structuredClone(request));
    return this.writeHook
      ? this.writeHook(request)
      : {
          queued: request.uids.length,
          mode: request.action === "mark" ? null : "move",
        };
  }
  activity(account?: string, before?: number) {
    const d = deferred<ActivityPage>();
    this.activities.push({ account, before, d });
    return d.promise;
  }
  async resolve(id: number, resolution: "retry" | "discard") {
    this.resolutions.push({ id, resolution });
    if (this.resolveHook) return this.resolveHook(id, resolution);
    throw new Error("offline");
  }
}
