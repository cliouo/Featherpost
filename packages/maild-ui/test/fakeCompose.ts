import type {
  ComposeBridge,
  Draft,
  DraftSummary,
  Patch,
  PickResult,
  Progress,
  ReplyMode,
  Sender,
} from "../src/compose/types";
import type { MessageIdentity } from "../src/bridge/types";
import { BridgeError } from "../src/bridge/errors";
export class FakeCompose implements ComposeBridge {
  rows = new Map<string, Draft>();
  saves: Array<{ id: string; version: number; patch: Patch }> = [];
  sends = 0;
  closed = false;
  replyCalls: MessageIdentity[] = [];
  saveHook:
    | ((id: string, version: number, patch: Patch) => Promise<Draft>)
    | null = null;
  sendHook: ((id: string, version: number) => Promise<Draft>) | null = null;
  async senders(): Promise<Sender[]> {
    return [
      {
        account: "demo",
        address: "me@example.test",
        label: "个人",
        available: true,
        simple: false,
      },
    ];
  }
  async drafts(offset = 0): Promise<{ drafts: DraftSummary[]; total: number }> {
    return {
      total: this.rows.size,
      drafts: [...this.rows.values()].slice(offset, offset + 8).map((d) => ({
        ...d,
        snippet: d.body,
        attachment_count: d.attachments.length,
      })),
    };
  }
  async draft(id: string) {
    const d = this.rows.get(id);
    if (!d) throw new BridgeError({ code: "not_found", message: "not found" });
    return structuredClone(d);
  }
  async create(account: string, patch: Patch) {
    const id = `draft-${this.rows.size + 1}`;
    const d: Draft = {
      id,
      account,
      from: "",
      to: [],
      cc: [],
      bcc: [],
      subject: "",
      body: "",
      has_html: false,
      version: 1,
      updated_at: 1,
      send_state: "editing",
      sent_archive: null,
      ...patch,
      attachments: [],
    };
    this.rows.set(id, d);
    return this.draft(id);
  }
  async save(id: string, version: number, patch: Patch) {
    this.saves.push({ id, version, patch: structuredClone(patch) });
    if (this.saveHook) return this.saveHook(id, version, patch);
    const d = await this.draft(id);
    if (d.version !== version)
      throw new BridgeError({ code: "conflict", message: "changed elsewhere" });
    const next = {
      ...d,
      ...patch,
      attachments: d.attachments,
      has_html: patch.body_html === "" ? false : d.has_html,
      version: version + 1,
    };
    this.rows.set(id, next);
    return structuredClone(next);
  }
  async reply(i: MessageIdentity, _mode: ReplyMode) {
    this.replyCalls.push(i);
    return this.create(i.account, {
      to: ["sender@example.test"],
      subject: "Re: " + i.uid,
      body: "quoted " + i.uid,
    });
  }
  async send(id: string, version: number) {
    this.sends++;
    if (this.sendHook) return this.sendHook(id, version);
    const d = await this.draft(id);
    const next = {
      ...d,
      version: version + 2,
      send_state: "sent" as const,
      sent_archive: true,
    };
    this.rows.set(id, next);
    return next;
  }
  async resolve(id: string, version: number) {
    const d = await this.draft(id);
    const next = { ...d, version: version + 1, send_state: "failed" as const };
    this.rows.set(id, next);
    return next;
  }
  async discard(id: string, _version: number) {
    this.rows.delete(id);
  }
  async pick(): Promise<PickResult[]> {
    return [];
  }
  async upload(_draftId: string, _token: string) {
    return "upload-1";
  }
  async release(_token: string) {}
  async finish(_uploadId: string, version: number) {
    const d = await this.draft("draft-1");
    return { ...d, version: version + 1 };
  }
  async cancel(_uploadId: string) {}
  async onProgress(_fn: (p: Progress) => void) {
    return () => {};
  }
  async onPicked(_fn: (p: PickResult[]) => void) {
    return () => {};
  }
  async onClose(_fn: () => void) {
    return () => {};
  }
  async ready() {}
  async close() {
    this.closed = true;
  }
}
