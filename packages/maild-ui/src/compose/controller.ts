import { Store } from "../host/store";
import { errorCode } from "../bridge/errors";
import type { MessageIdentity } from "../bridge/types";
import {
  attachmentInput,
  type ComposeBridge,
  type Draft,
  type DraftSummary,
  type Patch,
  type PickResult,
  type PickedFile,
  type ReplyMode,
  type Sender,
} from "./types";
export const COMPOSE_VIEW = "maild.compose.workspace";
export const DRAFTS_VIEW = "maild.compose.drafts";
export type Fields = {
  account: string;
  from: string;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
};
export type Upload = PickedFile & {
  status: "uploading" | "ready" | "finishing" | "error";
  offset: number;
  uploadId?: string;
  error?: string;
};
export interface ComposeState {
  draft: Draft | null;
  fields: Fields | null;
  revision: number;
  savedRevision: number;
  saving: boolean;
  busy: boolean;
  closing: boolean;
  uncertain: boolean;
  online: boolean;
  error: string | null;
  conflict: boolean;
  errors: Partial<Record<keyof Fields, string>>;
  confirm: "subject" | "attachment" | "discard" | "resolve" | "reload" | null;
  senders: Sender[];
  drafts: DraftSummary[];
  draftTotal: number;
  uploads: Upload[];
  source: string | null;
  inline: boolean;
  collapsed: boolean;
}
const fieldsOf = (d: Draft): Fields => ({
  account: d.account,
  from: d.from,
  to: d.to.join(", "),
  cc: d.cc.join(", "),
  bcc: d.bcc.join(", "),
  subject: d.subject,
  body: d.body,
});
export const addresses = (value: string) =>
  value
    .split(/[,;\n]/)
    .map((v) => v.trim())
    .filter(Boolean);
const patchOf = (f: Fields): Patch => ({
  ...f,
  to: addresses(f.to),
  cc: addresses(f.cc),
  bcc: addresses(f.bcc),
});
export const sourceKey = (i: MessageIdentity) =>
  JSON.stringify([i.account, i.folder, i.uid]);
const failure = (e: unknown) =>
  e instanceof Error ? e.message : "操作未完成，请重试。";
export class ComposeController {
  readonly store = new Store<ComposeState>({
    draft: null,
    fields: null,
    revision: 0,
    savedRevision: 0,
    saving: false,
    busy: false,
    closing: false,
    uncertain: false,
    online: false,
    error: null,
    conflict: false,
    errors: {},
    confirm: null,
    senders: [],
    drafts: [],
    draftTotal: 0,
    uploads: [],
    source: null,
    inline: false,
    collapsed: false,
  });
  private timer: ReturnType<typeof setTimeout> | null = null;
  private polling: ReturnType<typeof setInterval> | null = null;
  private serial: Promise<unknown> = Promise.resolve();
  private replies = new Map<string, string>();
  private stops: Array<() => void> = [];
  private sending = false;
  private dropScope = () => true;
  private beforeClose = async () => true;
  setBeforeClose(guard:()=>Promise<boolean>) { this.beforeClose = guard; }
  setDropScope(check: () => boolean) {
    this.dropScope = check;
  }
  constructor(
    readonly bridge: ComposeBridge,
    private navigate: (id: string) => void,
    private notify: (text: string) => void,
  ) {}
  private queue<T>(fn: () => Promise<T>): Promise<T> {
    const p = this.serial.catch(() => {}).then(fn);
    this.serial = p;
    return p;
  }
  async start() {
    this.stops = await Promise.all([
      this.bridge.onProgress((p) =>
        this.uploadPatch(p.token, { offset: p.offset }),
      ),
      this.bridge.onPicked((files) => {
        if (this.dropScope()) void this.attach(files);
        else {
          for (const f of files)
            if ("Ok" in f) void this.bridge.release(f.Ok.token);
          this.notify("请先打开要添加附件的草稿。");
        }
      }),
      this.bridge.onClose(() => {
        void this.closeWindow();
      }),
    ]);
  }
  stop() {
    this.stops.forEach((stop) => stop());
    if (this.timer) clearTimeout(this.timer);
    if (this.polling) clearInterval(this.polling);
  }
  setOnline(online: boolean) {
    const before = this.store.getSnapshot().online;
    this.store.patch({ online });
    if (online && !before) {
      void this.refresh();
      void this.reconcile();
    }
  }
  async refresh() {
    try {
      const [senders, drafts] = await Promise.all([
        this.bridge.senders(),
        this.bridge.drafts(),
      ]);
      this.store.patch({
        senders,
        drafts: drafts.drafts,
        draftTotal: drafts.total,
      });
    } catch (e) {
      this.store.patch({ error: failure(e) });
    }
  }
  async moreDrafts() {
    try {
      const page = await this.bridge.drafts(
        this.store.getSnapshot().drafts.length,
      );
      const old = this.store.getSnapshot().drafts;
      this.store.patch({
        drafts: [
          ...old,
          ...page.drafts.filter((d) => !old.some((o) => o.id === d.id)),
        ],
        draftTotal: page.total,
      });
    } catch (e) {
      this.store.patch({ error: failure(e) });
    }
  }
  private editable() {
    const s = this.store.getSnapshot();
    return (
      !!s.draft &&
      ["editing", "failed"].includes(s.draft.send_state) &&
      !s.uncertain
    );
  }
  change(field: keyof Fields, value: string) {
    const s = this.store.getSnapshot();
    if (
      !s.fields ||
      s.busy ||
      s.closing ||
      !this.editable() ||
      s.draft?.has_html
    )
      return;
    this.store.patch({
      fields: { ...s.fields, [field]: value },
      revision: s.revision + 1,
      errors: { ...s.errors, [field]: undefined },
      confirm: null,
    });
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      void this.flush().catch(() => {});
    }, 500);
  }
  setSender(value: string) {
    const [account, from] = JSON.parse(value) as [string, string];
    this.change("account", account);
    this.change("from", from);
  }
  private async saveLoop() {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    for (;;) {
      const s = this.store.getSnapshot();
      if (!s.draft || !s.fields || s.revision === s.savedRevision) return;
      if (s.conflict) throw new Error(s.error ?? "请先处理草稿冲突。");
      if (!s.online) {
        this.store.patch({ error: "后台连接不可用，修改仍保留在这个窗口。" });
        throw new Error("后台不可用");
      }
      this.store.patch({ saving: true });
      try {
        const result = await this.bridge.save(
          s.draft.id,
          s.draft.version,
          patchOf(s.fields),
        );
        this.store.patch({
          draft: result,
          savedRevision: s.revision,
          error: null,
          conflict: false,
        });
      } catch (e) {
        this.store.patch({
          error: failure(e),
          conflict: errorCode(e) === "conflict",
        });
        throw e;
      } finally {
        this.store.patch({ saving: false });
      }
    }
  }
  flush() {
    return this.queue(() => this.saveLoop());
  }
  private load(draft: Draft, source: string | null = null, inline = false) {
    this.store.patch({
      draft,
      fields: fieldsOf(draft),
      revision: 0,
      savedRevision: 0,
      error: null,
      conflict: false,
      errors: {},
      uncertain: false,
      source,
      inline,
      collapsed: false,
      confirm: null,
    });
    this.watchSend();
  }
  private async prepareSwitch() {
    if (this.store.getSnapshot().uploads.length)
      throw new Error("请先完成或移除正在准备的附件。");
    await this.saveLoop();
  }
  async create(to = "") {
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return;
    this.store.patch({ busy: true });
    return this.queue(async () => {
      try {
        if (this.store.getSnapshot().closing) return;
        await this.prepareSwitch();
        if (!this.store.getSnapshot().senders.length) await this.refresh();
        const s = this.store.getSnapshot();
        const sender = s.senders.find((v) => v.available) ?? s.senders[0];
        if (!sender) throw new Error("尚无发件账户，请先在账户设置中配置。");
        const draft = await this.bridge.create(sender.account, {
          from: sender.address,
          to: to ? [to] : [],
        });
        this.load(draft);
        this.navigate(COMPOSE_VIEW);
        void this.refresh();
      } catch (e) {
        this.store.patch({ error: failure(e) });
        this.notify(failure(e));
      } finally {
        this.store.patch({ busy: false });
      }
    });
  }
  async open(id: string) {
    if (this.store.getSnapshot().draft?.id === id) {
      this.store.patch({ inline: false, collapsed: false });
      this.navigate(COMPOSE_VIEW);
      return;
    }
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return;
    this.store.patch({ busy: true });
    return this.queue(async () => {
      this.store.patch({ busy: true });
      try {
        await this.prepareSwitch();
        this.load(await this.bridge.draft(id));
        this.navigate(COMPOSE_VIEW);
      } catch (e) {
        this.store.patch({ error: failure(e) });
        this.notify(failure(e));
      } finally {
        this.store.patch({ busy: false });
      }
    });
  }
  async reply(identity: MessageIdentity, mode: ReplyMode) {
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return;
    this.store.patch({ busy: true });
    return this.queue(async () => {
      this.store.patch({ busy: true });
      try {
        await this.prepareSwitch();
        const key = sourceKey(identity) + mode;
        const id = this.replies.get(key);
        let draft = id
          ? await this.bridge.draft(id).catch((e) => {
              if (errorCode(e) === "not_found") return null;
              throw e;
            })
          : null;
        if (!draft || draft.send_state === "sent") {
          draft = await this.bridge.reply(identity, mode);
          this.replies.set(key, draft.id);
        }
        this.load(draft, sourceKey(identity), mode !== "forward");
        if (mode === "forward") this.navigate(COMPOSE_VIEW);
      } catch (e) {
        this.store.patch({ error: failure(e) });
        this.notify(failure(e));
      } finally {
        this.store.patch({ busy: false });
      }
    });
  }
  collapse() {
    this.store.patch({ collapsed: !this.store.getSnapshot().collapsed });
  }
  expand() {
    this.store.patch({ inline: false });
    this.navigate(COMPOSE_VIEW);
  }
  async leave() {
    try {
      await this.flush();
      this.navigate("maild.mailbox.workspace");
    } catch {
      this.notify("草稿尚未保存，请保留窗口并重试。");
    }
  }
  confirm(value: ComposeState["confirm"]) {
    this.store.patch({ confirm: value });
  }
  private validate() {
    const s = this.store.getSnapshot(),
      f = s.fields!;
    const errors: ComposeState["errors"] = {};
    const sender = s.senders.find(
      (v) => v.account === f.account && (v.address === f.from || !f.from),
    );
    if (!sender?.available)
      errors.from = "该发件身份目前不可用，请配置 SMTP 或选择其他账户。";
    if (
      !addresses(f.to).length &&
      !addresses(f.cc).length &&
      !addresses(f.bcc).length
    )
      errors.to = "请输入至少一个收件地址。";
    for (const key of ["to", "cc", "bcc"] as const) {
      if (addresses(f[key]).some((v) => !/^[^\s<>@,;]+@[^\s<>@,;]+$/.test(v)))
        errors[key] = "请输入完整邮箱地址，多个地址用逗号分隔。";
    }
    if (
      addresses(f.to).length +
        addresses(f.cc).length +
        addresses(f.bcc).length >
      100
    )
      errors.to = "收件地址合计不能超过 100 个。";
    if (new TextEncoder().encode(f.subject).length > 998)
      errors.subject = "主题不能超过 998 字节。";
    if (new TextEncoder().encode(f.body).length > 256 * 1024)
      errors.body = "正文不能超过 256 KiB。";
    if (
      sender?.simple &&
      (addresses(f.to).length !== 1 ||
        addresses(f.cc).length ||
        addresses(f.bcc).length ||
        s.draft!.attachments.length)
    )
      errors.from = "这个发件服务只支持单收件人纯文本，不支持 Cc/Bcc 或附件。";
    this.store.patch({ errors });
    return !Object.keys(errors).length;
  }
  async send(confirmed = false) {
    if (this.sending) return;
    const before = this.store.getSnapshot();
    if (before.busy || before.conflict || (before.confirm && !confirmed))
      return;
    const targetId = before.draft?.id;
    if (
      !before.draft ||
      !this.editable() ||
      before.draft.has_html ||
      before.closing
    )
      return;
    if (!this.validate()) return;
    if (
      before.uploads.length ||
      before.draft.attachments.some((a) => !a.stored)
    ) {
      this.store.patch({ error: "请先完成、修复或移除未准备好的附件。" });
      return;
    }
    if (!confirmed) {
      if (!before.fields!.subject.trim()) {
        this.confirm("subject");
        return;
      }
      if (
        /附件|attach/i.test(before.fields!.body) &&
        !before.draft.attachments.length
      ) {
        this.confirm("attachment");
        return;
      }
    }
    this.sending = true;
    this.store.patch({ busy: true, confirm: null, error: null });
    await this.queue(async () => {
      try {
        await this.saveLoop();
        const s = this.store.getSnapshot();
        if (s.draft?.id !== targetId)
          throw new Error("编辑上下文已变化，请重新确认要发送的草稿。");
        if (!this.validate()) return;
        if (
          !confirmed &&
          /附件|attach/i.test(s.fields!.body) &&
          !s.draft!.attachments.length
        ) {
          this.confirm("attachment");
          return;
        }
        if (!s.online) throw new Error("后台连接不可用。");
        try {
          const d = await this.bridge.send(s.draft!.id, s.draft!.version);
          this.store.patch({ draft: d });
          this.watchSend();
          void this.refresh();
        } catch (e) {
          if (errorCode(e) === "conflict") {
            this.store.patch({ conflict: true, error: failure(e) });
            return;
          }
          this.store.patch({
            uncertain: true,
            error: "投递结果尚未确认。请核对已发送邮件，切勿直接重发。",
          });
          await this.reconcile();
          if (this.editable()) this.store.patch({ error: failure(e) });
        }
      } catch (e) {
        this.store.patch({ error: failure(e) });
      } finally {
        this.store.patch({ busy: false });
        this.sending = false;
      }
    });
  }
  async reconcile() {
    const s = this.store.getSnapshot();
    if (
      !s.draft ||
      (!s.uncertain && !["sending", "unknown"].includes(s.draft.send_state))
    )
      return;
    try {
      const d = await this.bridge.draft(s.draft.id);
      if (this.store.getSnapshot().draft?.id === d.id) {
        if (JSON.stringify(fieldsOf(d)) !== JSON.stringify(fieldsOf(s.draft))) {
          this.store.patch({
            conflict: true,
            uncertain: false,
            error: "草稿已在别处修改。请先重新载入并核对内容。",
          });
        } else this.store.patch({ draft: d, uncertain: false });
        this.watchSend();
      }
    } catch {
      /* keep uncertainty until an authoritative reply */
    }
  }
  private watchSend() {
    if (this.polling) clearInterval(this.polling);
    this.polling = null;
    if (
      this.store.getSnapshot().draft?.send_state === "sending" ||
      this.store.getSnapshot().uncertain
    )
      this.polling = setInterval(() => {
        void this.reconcile();
      }, 2000);
  }
  async resolve() {
    return this.queue(async () => {
      const s = this.store.getSnapshot();
      if (!s.draft) return;
      try {
        this.load(
          await this.bridge.resolve(s.draft.id, s.draft.version),
          s.source,
          s.inline,
        );
      } catch (e) {
        this.store.patch({ error: failure(e) });
      }
    });
  }
  async reload() {
    if (this.store.getSnapshot().busy) return;
    this.store.patch({ busy: true });
    return this.queue(async () => {
      const s = this.store.getSnapshot();
      try {
        if (s.draft)
          this.load(await this.bridge.draft(s.draft.id), s.source, s.inline);
      } catch (e) {
        this.store.patch({ error: failure(e) });
      } finally {
        this.store.patch({ busy: false });
      }
    });
  }
  async plainText() {
    return this.queue(async () => {
      const s = this.store.getSnapshot();
      if (!s.draft) return;
      try {
        this.load(
          await this.bridge.save(s.draft.id, s.draft.version, {
            body_html: "",
          }),
          s.source,
          s.inline,
        );
      } catch (e) {
        this.store.patch({ error: failure(e) });
      }
    });
  }
  async discard() {
    if (this.store.getSnapshot().busy) return;
    this.store.patch({ busy: true });
    return this.queue(async () => {
      const s = this.store.getSnapshot();
      try {
        if (!s.draft) return;
        for (const u of s.uploads) await this.removeUpload(u.token);
        await this.bridge.discard(s.draft.id, s.draft.version);
        this.store.patch({
          draft: null,
          fields: null,
          confirm: null,
          error: null,
          uploads: [],
        });
        this.navigate(DRAFTS_VIEW);
        await this.refresh();
      } catch (e) {
        this.store.patch({ error: failure(e) });
      } finally {
        this.store.patch({ busy: false });
      }
    });
  }
  async pick() {
    const id = this.store.getSnapshot().draft?.id;
    try {
      const picked = await this.bridge.pick();
      if (this.store.getSnapshot().draft?.id !== id) {
        for (const f of picked)
          if ("Ok" in f) await this.bridge.release(f.Ok.token);
        return;
      }
      await this.attach(picked);
    } catch (e) {
      this.store.patch({ error: failure(e) });
    }
  }
  async attach(results: PickResult[]) {
    const s = this.store.getSnapshot();
    for (const result of results) {
      if ("Err" in result) {
        this.store.patch({ error: result.Err.message });
        continue;
      }
      if (!s.draft || !this.editable() || s.busy || s.closing) {
        await this.bridge.release(result.Ok.token);
        this.notify("请先打开一份可编辑的草稿。");
        continue;
      }
      this.store.patch({
        uploads: [
          ...this.store.getSnapshot().uploads,
          { ...result.Ok, status: "uploading", offset: 0 },
        ],
      });
      void this.upload(result.Ok.token);
    }
  }
  private uploadPatch(token: string, patch: Partial<Upload>) {
    this.store.patch({
      uploads: this.store
        .getSnapshot()
        .uploads.map((u) => (u.token === token ? { ...u, ...patch } : u)),
    });
  }
  async upload(token: string) {
    const s = this.store.getSnapshot();
    const file = s.uploads.find((u) => u.token === token);
    if (!file || !s.draft) return;
    this.uploadPatch(token, { status: "uploading", error: undefined });
    try {
      const id = file.uploadId ?? (await this.bridge.upload(s.draft.id, token));
      this.uploadPatch(token, { uploadId: id, status: "ready" });
      await this.queue(async () => {
        if (!this.store.getSnapshot().uploads.some((u) => u.token === token)) {
          await this.bridge.cancel(id).catch(() => {});
          return;
        }
        await this.saveLoop();
        const current = this.store.getSnapshot();
        if (!current.uploads.some((u) => u.token === token)) {
          await this.bridge.cancel(id).catch(() => {});
          return;
        }
        this.uploadPatch(token, { status: "finishing" });
        const saved = await this.bridge.finish(id, current.draft!.version);
        // Idempotent finish can return a newer draft after a lost receipt. Never
        // adopt an unseen recipient/body version just to attach a file.
        if (
          JSON.stringify(fieldsOf(saved)) !==
          JSON.stringify(fieldsOf(current.draft!))
        ) {
          this.store.patch({
            conflict: true,
            error: "草稿已在别处修改。请先重新载入并核对内容。",
          });
          throw new Error("附件已完成，但草稿内容已变化；请重新载入核对。");
        }
        this.store.patch({
          draft: saved,
          uploads: this.store
            .getSnapshot()
            .uploads.filter((u) => u.token !== token),
        });
        await this.bridge.release(token);
      });
    } catch (e) {
      this.uploadPatch(token, { status: "error", error: failure(e) });
    }
  }
  async removeUpload(token: string) {
    const u = this.store.getSnapshot().uploads.find((u) => u.token === token);
    if (u?.status === "finishing") return;
    this.store.patch({
      uploads: this.store
        .getSnapshot()
        .uploads.filter((u) => u.token !== token),
    });
    await this.bridge.release(token);
    if (u?.uploadId) await this.bridge.cancel(u.uploadId).catch(() => {});
  }
  async detach(index: number) {
    const clicked = this.store.getSnapshot().draft,
      attachment = clicked?.attachments[index];
    if (!clicked || !attachment) return;
    const key = JSON.stringify(attachmentInput(attachment));
    return this.queue(async () => {
      try {
        await this.saveLoop();
        const s = this.store.getSnapshot();
        if (s.draft?.id !== clicked.id) return;
        const index = s.draft.attachments.findIndex(
          (a) => JSON.stringify(attachmentInput(a)) === key,
        );
        if (index < 0) return;
        const attachments = s.draft.attachments
          .filter((_, i) => i !== index)
          .map(attachmentInput);
        this.store.patch({
          draft: await this.bridge.save(s.draft.id, s.draft.version, {
            attachments,
          }),
        });
      } catch (e) {
        this.store.patch({
          error: failure(e),
          conflict: errorCode(e) === "conflict",
        });
      }
    });
  }
  async closeWindow() {
    if (this.store.getSnapshot().closing) return;
    this.store.patch({ closing: true });
    try {
      if (!(await this.beforeClose())) { this.store.patch({closing:false}); return; }
      if (
        this.sending ||
        this.store.getSnapshot().draft?.send_state === "sending"
      )
        throw new Error("邮件正在发送，请等待投递结果。");
      if (this.store.getSnapshot().uploads.length)
        throw new Error("附件仍在准备，请完成或取消后再关闭窗口。");
      await this.flush();
      await this.bridge.close();
    } catch (e) {
      this.store.patch({ closing: false, error: failure(e) });
      this.navigate(COMPOSE_VIEW);
      this.notify(failure(e));
    }
  }
  async copy() {
    const s = this.store.getSnapshot();
    if (!s.fields) return;
    const f = s.fields;
    try {
      await navigator.clipboard.writeText(
        `From: ${f.from}\nTo: ${f.to}\nCc: ${f.cc}\nBcc: ${f.bcc}\nSubject: ${f.subject}\n\n${f.body}`,
      );
      this.notify("已复制草稿内容。");
    } catch {
      this.notify("无法访问剪贴板，请在编辑器中选择并复制。");
    }
  }
}
