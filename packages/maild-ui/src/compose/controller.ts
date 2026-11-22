import { m } from "@featherpost/i18n/messages";

import { Store } from "../host/store";
import { bridgeErrorText, errorCode } from "../bridge/errors";
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
  e instanceof Error ? e.message : m.compose_error_generic();
/** 正文提到附件却没有附件时提醒；按正文内容识别，中英文关键词都认，与界面语言无关。 */
// i18n-lint-allow-cjk: 附件提醒按正文内容识别的关键词，不是界面文案
const MENTIONS_ATTACHMENT = /附件|attach/i;
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
          this.notify(m.compose_attach_no_draft());
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
      if (s.conflict) throw new Error(s.error ?? m.compose_error_conflict());
      if (!s.online) {
        this.store.patch({ error: m.compose_error_offline_kept() });
        throw new Error(m.compose_error_offline_short());
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
      throw new Error(m.compose_error_uploads_pending());
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
        if (!sender) throw new Error(m.compose_error_no_sender());
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
      this.notify(m.compose_error_not_saved());
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
      errors.from = m.compose_validate_sender_unavailable();
    if (
      !addresses(f.to).length &&
      !addresses(f.cc).length &&
      !addresses(f.bcc).length
    )
      errors.to = m.compose_validate_to_required();
    for (const key of ["to", "cc", "bcc"] as const) {
      if (addresses(f[key]).some((v) => !/^[^\s<>@,;]+@[^\s<>@,;]+$/.test(v)))
        errors[key] = m.compose_validate_address_invalid();
    }
    if (
      addresses(f.to).length +
        addresses(f.cc).length +
        addresses(f.bcc).length >
      100
    )
      errors.to = m.compose_validate_too_many_recipients();
    if (new TextEncoder().encode(f.subject).length > 998)
      errors.subject = m.compose_validate_subject_too_long();
    if (new TextEncoder().encode(f.body).length > 256 * 1024)
      errors.body = m.compose_validate_body_too_long();
    if (
      sender?.simple &&
      (addresses(f.to).length !== 1 ||
        addresses(f.cc).length ||
        addresses(f.bcc).length ||
        s.draft!.attachments.length)
    )
      errors.from = m.compose_validate_simple_sender();
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
      this.store.patch({ error: m.compose_error_attachments_not_ready() });
      return;
    }
    if (!confirmed) {
      if (!before.fields!.subject.trim()) {
        this.confirm("subject");
        return;
      }
      if (
        MENTIONS_ATTACHMENT.test(before.fields!.body) &&
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
          throw new Error(m.compose_error_context_changed());
        if (!this.validate()) return;
        if (
          !confirmed &&
          MENTIONS_ATTACHMENT.test(s.fields!.body) &&
          !s.draft!.attachments.length
        ) {
          this.confirm("attachment");
          return;
        }
        if (!s.online) throw new Error(m.compose_error_offline());
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
            error: m.compose_error_send_uncertain(),
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
            error: m.compose_error_conflict_elsewhere(),
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
        this.store.patch({ error: bridgeErrorText(result.Err) });
        continue;
      }
      if (!s.draft || !this.editable() || s.busy || s.closing) {
        await this.bridge.release(result.Ok.token);
        this.notify(m.compose_attach_no_editable_draft());
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
            error: m.compose_error_conflict_elsewhere(),
          });
          throw new Error(m.compose_error_attach_draft_changed());
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
        throw new Error(m.compose_error_close_sending());
      if (this.store.getSnapshot().uploads.length)
        throw new Error(m.compose_error_close_uploads());
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
      this.notify(m.compose_copy_done());
    } catch {
      this.notify(m.compose_copy_failed());
    }
  }
}
