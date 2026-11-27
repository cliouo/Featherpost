import { m } from "@featherpost/i18n/messages";

import { Store } from "../host/store";
import { uiLocale } from "../i18n";
import { BridgeError, bridgeErrorText, errorCode } from "../bridge/errors";
import type { MessageIdentity, UiContactSuggestion } from "../bridge/types";
import {
  buildFallbackContactSuggestions,
  canonicalAddrSpec,
  checkRecipientDraftLimits,
  cleanDisplayName,
  createRecipientMap,
  effectiveRecipientValues,
  hasControlChar,
  hasTopLevelDelimiter,
  isValidAddrSpec,
  MAX_CONTACT_QUERY_BYTES,
  mergeRecipientTokens,
  parseRecipientList,
  parseRecipientToken,
  RECIPIENT_FIELDS,
  recipientDedupKey,
  utf8ByteLength,
  type RecipientFieldName,
  type RecipientLimitError,
  type RecipientMap,
} from "./recipients";
import {
  attachmentInput,
  type ComposeBridge,
  type ContactSearchSource,
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
  recipients: RecipientMap | null;
  recipientNames: Record<string, string>;
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
  parseRecipientList(value).map((item) => item.value);
const recipientArraysOf = (
  fields: Fields,
  recipients: RecipientMap | null,
): Record<RecipientFieldName, string[]> =>
  recipients
    ? {
        to: effectiveRecipientValues(recipients.to),
        cc: effectiveRecipientValues(recipients.cc),
        bcc: effectiveRecipientValues(recipients.bcc),
      }
    : {
        to: addresses(fields.to),
        cc: addresses(fields.cc),
        bcc: addresses(fields.bcc),
      };
const limitMessage = (kind: RecipientLimitError): string =>
  kind === "too_many"
    ? m.compose_validate_too_many_recipients()
    : kind === "too_long"
      ? m.compose_validate_address_too_long()
      : m.compose_validate_address_control_char();
const patchOf = (f: Fields, recipients: RecipientMap | null): Patch => {
  const lists = recipientArraysOf(f, recipients);
  return {
    ...f,
    to: lists.to,
    cc: lists.cc,
    bcc: lists.bcc,
  };
};
const isRecipientField = (field: keyof Fields): field is RecipientFieldName =>
  field === "to" || field === "cc" || field === "bcc";
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
    recipients: null,
    recipientNames: {},
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
  private contactSource: ContactSearchSource = {};
  private dropScope = () => true;
  private beforeClose = async () => true;
  private leaveGuard: (
    id: string,
    next: () => void,
    cancel?: () => void,
  ) => boolean = (_id, next) => {
    next();
    return true;
  };
  setContactSource(source: Partial<ContactSearchSource>) {
    this.contactSource = { ...this.contactSource, ...source };
  }
  setBeforeClose(guard:()=>Promise<boolean>) { this.beforeClose = guard; }
  setBeforeNavigate(
    guard: (id: string, next: () => void, cancel?: () => void) => boolean,
  ) {
    this.leaveGuard = guard;
  }
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
  private canEdit() {
    const s = this.store.getSnapshot();
    return (
      !!s.fields &&
      !!s.recipients &&
      !s.busy &&
      !s.closing &&
      this.editable() &&
      !s.draft?.has_html
    );
  }
  private scheduleAutosave() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      void this.flush().catch(() => {});
    }, 500);
  }
  private applyRecipients(
    nextRecipients: RecipientMap,
    extraNames?: Record<string, string>,
    touchedField?: RecipientFieldName,
  ) {
    const s = this.store.getSnapshot();
    if (!s.fields) return;
    const lists = recipientArraysOf(s.fields, nextRecipients);
    const nextFields: Fields = {
      ...s.fields,
      to: lists.to.join(", "),
      cc: lists.cc.join(", "),
      bcc: lists.bcc.join(", "),
    };
    const limits = checkRecipientDraftLimits(lists);
    const nextErrors: ComposeState["errors"] = { ...s.errors };
    if (touchedField) {
      nextErrors[touchedField] = undefined;
    }
    for (const key of RECIPIENT_FIELDS) {
      const limitKind = limits[key];
      if (limitKind) {
        nextErrors[key] = limitMessage(limitKind);
      } else if (
        nextErrors[key] === m.compose_validate_too_many_recipients() ||
        nextErrors[key] === m.compose_validate_address_too_long() ||
        nextErrors[key] === m.compose_validate_address_control_char()
      ) {
        nextErrors[key] = undefined;
      }
    }
    const nextNames =
      extraNames && Object.keys(extraNames).length > 0
        ? { ...s.recipientNames, ...extraNames }
        : s.recipientNames;
    const clearLimitBanner =
      Object.keys(limits).length === 0 &&
      (s.error === m.compose_validate_too_many_recipients() ||
        s.error === m.compose_validate_address_too_long() ||
        s.error === m.compose_validate_address_control_char());
    this.store.patch({
      fields: nextFields,
      recipients: nextRecipients,
      recipientNames: nextNames,
      revision: s.revision + 1,
      errors: nextErrors,
      ...(clearLimitBanner ? { error: null } : {}),
      confirm: null,
    });
    this.scheduleAutosave();
  }
  setRecipientPending(field: RecipientFieldName, value: string) {
    if (!this.canEdit()) return;
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    if (current.pending === value) return;
    const nextRecipients: RecipientMap = {
      ...s.recipients!,
      [field]: { ...current, pending: value },
    };
    this.applyRecipients(nextRecipients, undefined, field);
  }
  commitRecipientInput(field: RecipientFieldName, raw?: string) {
    if (!this.canEdit()) return;
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    const sourceText = raw ?? current.pending;
    if (current.editingIndex === null && !sourceText.trim() && !current.pending) {
      return;
    }
    const parsed = parseRecipientList(sourceText);
    const merged = mergeRecipientTokens(
      current.chips,
      parsed,
      current.editingIndex,
    );
    const nextRecipients: RecipientMap = {
      ...s.recipients!,
      [field]: {
        chips: merged.chips,
        pending: "",
        editingIndex: null,
        editingOriginal: null,
      },
    };
    this.applyRecipients(nextRecipients, merged.names, field);
  }
  pasteRecipientText(
    field: RecipientFieldName,
    pasted: string,
    selectionStart: number,
    selectionEnd: number,
  ) {
    if (!this.canEdit()) return;
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    const safeStart = Math.max(0, Math.min(selectionStart, current.pending.length));
    const safeEnd = Math.max(safeStart, Math.min(selectionEnd, current.pending.length));
    const combined =
      current.pending.slice(0, safeStart) +
      pasted +
      current.pending.slice(safeEnd);
    if (hasTopLevelDelimiter(pasted)) {
      this.commitRecipientInput(field, combined);
    } else {
      this.setRecipientPending(field, combined);
    }
  }
  selectRecipientSuggestion(
    field: RecipientFieldName,
    suggestion: UiContactSuggestion,
  ) {
    if (!this.canEdit()) return;
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    const parsed = parseRecipientToken(suggestion.address);
    const displayName = cleanDisplayName(suggestion.name, parsed.value);
    const item = displayName ? { ...parsed, displayName } : parsed;
    const merged = mergeRecipientTokens(
      current.chips,
      [item],
      current.editingIndex,
    );
    const nextRecipients: RecipientMap = {
      ...s.recipients!,
      [field]: {
        chips: merged.chips,
        pending: "",
        editingIndex: null,
        editingOriginal: null,
      },
    };
    this.applyRecipients(nextRecipients, merged.names, field);
  }
  removeRecipient(field: RecipientFieldName, index: number) {
    if (!this.canEdit()) return;
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    if (index < 0 || index >= current.chips.length) return;
    const chips = current.chips.filter((_, i) => i !== index);
    let editingIndex = current.editingIndex;
    let editingOriginal = current.editingOriginal;
    let pending = current.pending;
    if (editingIndex !== null) {
      if (editingIndex === index) {
        editingIndex = null;
        editingOriginal = null;
        pending = "";
      } else if (editingIndex > index) {
        editingIndex -= 1;
      }
    }
    const nextRecipients: RecipientMap = {
      ...s.recipients!,
      [field]: { chips, pending, editingIndex, editingOriginal },
    };
    this.applyRecipients(nextRecipients, undefined, field);
  }
  startEditRecipient(field: RecipientFieldName, index: number) {
    if (!this.canEdit()) return;
    const before = this.store.getSnapshot().recipients![field];
    if (before.editingIndex === index) return;
    if (before.editingIndex !== null || before.pending.trim().length > 0) {
      this.commitRecipientInput(field);
    }
    const s = this.store.getSnapshot();
    const current = s.recipients![field];
    if (index < 0 || index >= current.chips.length) return;
    const original = current.chips[index]!;
    const nextRecipients: RecipientMap = {
      ...s.recipients!,
      [field]: {
        ...current,
        editingIndex: index,
        editingOriginal: original,
        pending: original,
      },
    };
    this.store.patch({ recipients: nextRecipients });
  }
  cancelEditRecipient(field: RecipientFieldName) {
    const s = this.store.getSnapshot();
    if (!s.fields || !s.recipients) return;
    const current = s.recipients[field];
    if (current.editingIndex === null) return;
    const chips = [...current.chips];
    if (
      current.editingOriginal !== null &&
      current.editingIndex >= 0 &&
      current.editingIndex < chips.length
    ) {
      chips[current.editingIndex] = current.editingOriginal;
    }
    const nextRecipients: RecipientMap = {
      ...s.recipients,
      [field]: {
        chips,
        pending: "",
        editingIndex: null,
        editingOriginal: null,
      },
    };
    const lists = recipientArraysOf(s.fields, nextRecipients);
    const nextFields: Fields = {
      ...s.fields,
      [field]: lists[field].join(", "),
    };
    const changed = nextFields[field] !== s.fields[field];
    this.store.patch({
      recipients: nextRecipients,
      fields: nextFields,
      ...(changed ? { revision: s.revision + 1 } : {}),
    });
    if (changed) this.scheduleAutosave();
  }
  commitAllPendingRecipients() {
    if (!this.canEdit()) return;
    for (const field of RECIPIENT_FIELDS) {
      const state = this.store.getSnapshot().recipients?.[field];
      if (!state) continue;
      if (state.editingIndex !== null || state.pending.trim().length > 0) {
        this.commitRecipientInput(field);
      }
    }
  }
  async searchContacts(
    account: string,
    query: string,
    limit = 8,
  ): Promise<UiContactSuggestion[]> {
    const trimmed = query.trim();
    if (
      !account ||
      utf8ByteLength(trimmed) > MAX_CONTACT_QUERY_BYTES ||
      hasControlChar(trimmed)
    ) {
      return [];
    }
    const fallback = (): UiContactSuggestion[] => {
      const rows = this.contactSource.fallbackRows?.(account) ?? [];
      return buildFallbackContactSuggestions(
        account,
        rows,
        this.store.getSnapshot().senders,
        trimmed,
        limit,
      );
    };
    const s = this.store.getSnapshot();
    if (!s.online || !this.contactSource.searchContacts) {
      return fallback();
    }
    try {
      const raw = await this.contactSource.searchContacts(
        account,
        trimmed || undefined,
        limit,
      );
      const out: UiContactSuggestion[] = [];
      const seen = new Set<string>();
      for (const item of raw) {
        const canonical = canonicalAddrSpec(item.address);
        if (!canonical || seen.has(canonical)) continue;
        seen.add(canonical);
        const name = cleanDisplayName(item.name, canonical);
        out.push(name ? { address: canonical, name } : { address: canonical });
      }
      return out;
    } catch (e) {
      const code = errorCode(e);
      const reason = e instanceof BridgeError ? e.detail.reason : undefined;
      if (code === "denied") {
        return [];
      }
      if (
        code === "offline" ||
        reason === "contacts_unsupported" ||
        code === "failed" ||
        code === "not_found" ||
        code === "incompatible"
      ) {
        return fallback();
      }
      return [];
    }
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
    if (isRecipientField(field) && s.recipients) {
      const parsed = parseRecipientList(value);
      const names: Record<string, string> = {};
      for (const item of parsed) {
        if (item.displayName) {
          names[recipientDedupKey(item.value)] = item.displayName;
        }
      }
      const nextRecipients: RecipientMap = {
        ...s.recipients,
        [field]: {
          chips: parsed.map((p) => p.value),
          pending: "",
          editingIndex: null,
          editingOriginal: null,
        },
      };
      this.applyRecipients(nextRecipients, names, field);
      return;
    }
    this.store.patch({
      fields: { ...s.fields, [field]: value },
      revision: s.revision + 1,
      errors: { ...s.errors, [field]: undefined },
      confirm: null,
    });
    this.scheduleAutosave();
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
      const recipientLists = recipientArraysOf(s.fields, s.recipients);
      const limitErrors = checkRecipientDraftLimits(recipientLists);
      const limitEntries = Object.entries(limitErrors) as Array<
        [RecipientFieldName, RecipientLimitError]
      >;
      if (limitEntries.length > 0) {
        const mappedErrors: ComposeState["errors"] = { ...s.errors };
        for (const [field, kind] of limitEntries) {
          mappedErrors[field] = limitMessage(kind);
        }
        const firstMessage = limitMessage(limitEntries[0]![1]);
        this.store.patch({ errors: mappedErrors, error: firstMessage });
        throw new Error(firstMessage);
      }
      this.store.patch({ saving: true });
      try {
        const result = await this.bridge.save(
          s.draft.id,
          s.draft.version,
          patchOf(s.fields, s.recipients),
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
    const { recipients, names } = createRecipientMap(draft);
    const fields: Fields = {
      ...fieldsOf(draft),
      to: recipients.to.chips.join(", "),
      cc: recipients.cc.chips.join(", "),
      bcc: recipients.bcc.chips.join(", "),
    };
    const prev = this.store.getSnapshot();
    const keepNames =
      prev.draft && prev.draft.account === draft.account
        ? { ...prev.recipientNames, ...names }
        : names;
    this.store.patch({
      draft,
      fields,
      recipients,
      recipientNames: keepNames,
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
  create(to = ""): Promise<void> {
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return Promise.resolve();
    return new Promise<void>((resolve) => {
      this.leaveGuard(
        COMPOSE_VIEW,
        () => {
          void this.createApproved(to).finally(resolve);
        },
        () => resolve(),
      );
    });
  }
  private async createApproved(to = "") {
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
  open(id: string): Promise<void> {
    if (this.store.getSnapshot().draft?.id === id) {
      return new Promise<void>((resolve) => {
        this.leaveGuard(
          COMPOSE_VIEW,
          () => {
            if (this.store.getSnapshot().draft?.id === id) {
              this.store.patch({ inline: false, collapsed: false });
              this.navigate(COMPOSE_VIEW);
            }
            resolve();
          },
          () => resolve(),
        );
      });
    }
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return Promise.resolve();
    return new Promise<void>((resolve) => {
      this.leaveGuard(
        COMPOSE_VIEW,
        () => {
          void this.openApproved(id).finally(resolve);
        },
        () => resolve(),
      );
    });
  }
  private async openApproved(id: string) {
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
  reply(identity: MessageIdentity, mode: ReplyMode): Promise<void> {
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closing)
      return Promise.resolve();
    const targetView =
      mode === "forward" ? COMPOSE_VIEW : "maild.mailbox.workspace";
    return new Promise<void>((resolve) => {
      this.leaveGuard(
        targetView,
        () => {
          void this.replyApproved(identity, mode).finally(resolve);
        },
        () => resolve(),
      );
    });
  }
  private async replyApproved(identity: MessageIdentity, mode: ReplyMode) {
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
          draft = await this.bridge.reply(identity, mode, uiLocale());
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
    const lists = recipientArraysOf(f, s.recipients);
    const errors: ComposeState["errors"] = {};
    const sender = s.senders.find(
      (v) => v.account === f.account && (v.address === f.from || !f.from),
    );
    if (!sender?.available)
      errors.from = m.compose_validate_sender_unavailable();
    if (!lists.to.length && !lists.cc.length && !lists.bcc.length)
      errors.to = m.compose_validate_to_required();
    const limitErrors = checkRecipientDraftLimits(lists);
    for (const key of RECIPIENT_FIELDS) {
      if (limitErrors[key]) {
        errors[key] = limitMessage(limitErrors[key]!);
      } else if (lists[key].some((v) => !isValidAddrSpec(v))) {
        errors[key] = m.compose_validate_address_invalid();
      }
    }
    if (utf8ByteLength(f.subject) > 998)
      errors.subject = m.compose_validate_subject_too_long();
    if (utf8ByteLength(f.body) > 256 * 1024)
      errors.body = m.compose_validate_body_too_long();
    if (
      sender?.simple &&
      (lists.to.length !== 1 ||
        lists.cc.length ||
        lists.bcc.length ||
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
    this.commitAllPendingRecipients();
    const afterCommit = this.store.getSnapshot();
    if (!this.validate()) return;
    if (
      afterCommit.uploads.length ||
      afterCommit.draft!.attachments.some((a) => !a.stored)
    ) {
      this.store.patch({ error: m.compose_error_attachments_not_ready() });
      return;
    }
    if (!confirmed) {
      if (!afterCommit.fields!.subject.trim()) {
        this.confirm("subject");
        return;
      }
      if (
        MENTIONS_ATTACHMENT.test(afterCommit.fields!.body) &&
        !afterCommit.draft!.attachments.length
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
          recipients: null,
          recipientNames: {},
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
