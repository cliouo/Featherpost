import { m } from "@featherpost/i18n/messages";

import { folderLabel } from "../i18n";
import type {
  FolderIdentity,
  MailBridge,
  MessageIdentity,
  UiFolder,
  UiMessage,
  UiSnapshot,
  UiSummary,
} from "../bridge/types";
import { compatBlocksWrites } from "../bridge/types";
import { errorCode } from "../bridge/errors";
import { Store } from "../host/store";
import {
  canonicalScopeKey,
  canonicalSearchQuery,
  hasStructuredFilters,
  type SearchFilterMeta,
} from "../searchFilters";
import type {
  BatchReport,
  IndexStatus,
  MailAction,
  MailboxQuery,
  Profile,
  Receipt,
  Scope,
  SmartFolder,
  WriteRequest,
} from "./types";
const PAGE_SIZE = 50,
  MAX_ROWS = 1000;
const MARK_READ_DWELL_MS = 1500;
const BACKGROUND_SYNC_POLL_MS = 1000;
const BACKGROUND_SYNC_WAIT_MS = 5 * 60 * 1000;
export const folderKey = ({ account, folder }: FolderIdentity) =>
  JSON.stringify([account, folder]);
export const messageKey = ({ account, folder, uid }: MessageIdentity) =>
  JSON.stringify([account, folder, uid]);
const scopeKey = canonicalScopeKey;
export class RequestGeneration {
  private generation = 0;
  begin() {
    const generation = ++this.generation;
    return () => generation === this.generation;
  }
  invalidate() {
    this.generation++;
  }
}
/** 服务器历史搜索（本地缓存有界，更早的邮件按需从服务器取）。 */
export interface RemoteSearch {
  status: "idle" | "loading" | "ready" | "error";
  query: string;
  accounts: string[];
  rows: UiSummary[];
  /** 已拉回本地库的远端结果（messageKey）。 */
  fetched: string[];
  error: string | null;
}
export const idleRemote = (): RemoteSearch => ({
  status: "idle",
  query: "",
  accounts: [],
  rows: [],
  fetched: [],
  error: null,
});
export interface MailboxState {
  snapshot: UiSnapshot;
  scope: Scope;
  account: string;
  folders: UiFolder[];
  folder: UiFolder | null;
  profiles: Profile[];
  rows: UiSummary[];
  total: number;
  nextOffset: number;
  selected: MessageIdentity | null;
  selectedRow: UiSummary | null;
  detail: UiMessage | null;
  checked: MessageIdentity[];
  busy: boolean;
  report: BatchReport | null;
  prompt: { kind: "move" | "delete"; rows: UiSummary[] } | null;
  loadingFolders: boolean;
  loadingRows: boolean;
  loadingDetail: boolean;
  folderError: string | null;
  rowsError: string | null;
  detailError: string | null;
  index: IndexStatus | null;
  indexError: string | null;
  threadOpen: boolean;
  threadRoot: MessageIdentity | null;
  threadId: number;
  threadRows: UiSummary[];
  threadTotal: number;
  threadOffset: number;
  threadLoading: boolean;
  threadError: string | null;
  syncingScopes: ReadonlyMap<string | undefined, { folder?: string }>;
  syncError: string | null;
  syncNotice: string | null;
  remote: RemoteSearch;
}
interface Cached {
  rows: UiSummary[];
  total: number;
  nextOffset: number;
}
const describe = (error: unknown, fallback: string) =>
  errorCode(error) === "offline"
    ? m.mailbox_error_offline()
    : errorCode(error) === "denied"
      ? m.mailbox_error_denied()
      : errorCode(error) === "not_found"
        ? m.mailbox_error_not_found()
        : fallback;
const unconfirmedWrite = (error: unknown) =>
  errorCode(error)
    ? m.mailbox_error_submit_unconfirmed()
    : error instanceof Error
      ? error.message
      : m.mailbox_error_unconfirmed();
function validateReceipt(receipt: Receipt, uids: string[]) {
  const { queued, queued_uids, unchanged_uids, missing_uids } = receipt;
  if (!Number.isInteger(queued) || queued < 0 || queued > uids.length)
    throw new Error(m.mailbox_error_receipt_invalid());
  if (queued_uids && unchanged_uids && missing_uids) {
    const ids = [...queued_uids, ...unchanged_uids, ...missing_uids];
    if (
      ids.length !== uids.length ||
      new Set(ids).size !== ids.length ||
      ids.some((uid) => !uids.includes(uid)) ||
      queued_uids.length !== queued
    )
      throw new Error(m.mailbox_error_receipt_identity());
    return {
      queued,
      unchanged: unchanged_uids.length,
      accepted: [...queued_uids, ...unchanged_uids],
      error: missing_uids.length ? m.mailbox_error_receipt_missing() : null,
    };
  }
  return {
    queued,
    unchanged: 0,
    accepted: queued === uids.length ? uids : [],
    error: queued === uids.length ? null : m.mailbox_error_receipt_partial(),
  };
}
function boundedSet<K, V>(cache: Map<K, V>, key: K, value: V, limit: number) {
  cache.delete(key);
  cache.set(key, value);
  while (cache.size > limit) cache.delete(cache.keys().next().value as K);
}
export function queryOf(scope: Scope): MailboxQuery {
  if (scope.kind === "unread") return { ...queryOf(scope.base), seen: false };
  if (scope.kind === "search") return canonicalSearchQuery(scope.query);
  if (scope.kind === "folder")
    return { account: scope.account, folder: scope.folder };
  const filters = scope.name === "inbox"
    ? { role: "inbox" }
    : scope.name === "unread"
      ? { seen: false }
      : scope.name === "starred"
        ? { flagged: true }
        : {};
  return { ...filters, ...(scope.group ? { accounts: scope.group.accounts } : {}) };
}
function matchesMailbox(row: UiSummary, query: MailboxQuery): boolean {
  return (
    (!query.account || row.account === query.account) &&
    (!query.accounts || query.accounts.includes(row.account)) &&
    (!query.folder || row.folder === query.folder)
  );
}

function unreadPinIndex(
  row: UiSummary,
  rows: readonly UiSummary[],
  previous: readonly UiSummary[],
  grouped: boolean,
): number {
  const key = messageKey(row);
  const previousIndex = previous.findIndex((r) => messageKey(r) === key);
  if (previousIndex < 0) return -1;
  const compare = (other: UiSummary) =>
    row.sort_at - other.sort_at ||
    (grouped && row.account !== other.account ? (row.account < other.account ? 1 : -1) : 0);
  const knownOrder = new Map(previous.map((r, i) => [messageKey(r), i]));
  let afterKnownPredecessor: number | null = null;
  let firstTie: number | null = null;
  for (let i = 0; i < rows.length; i++) {
    const order = compare(rows[i]);
    if (order < 0) continue;
    if (order > 0) return afterKnownPredecessor ?? firstTie ?? i;
    // The daemon's same-timestamp row IDs are not on the bridge; keep known ties in their list order.
    firstTie ??= i;
    const prior = knownOrder.get(messageKey(rows[i]));
    if (prior !== undefined && prior > previousIndex) return i;
    if (prior !== undefined && prior < previousIndex) afterKnownPredecessor = i + 1;
  }
  return afterKnownPredecessor ?? firstTie ?? rows.length;
}

export function scopeTitle(scope: Scope) {
  if (scope.kind === "unread") return scopeTitle(scope.base);
  if (scope.kind === "smart" && scope.group) return m.mailbox_scope_group_inbox({ group: scope.group.name });
  return scope.kind === "folder"
    ? scope.title
    : scope.kind === "search"
      ? m.mailbox_scope_search()
      : {
          inbox: m.mailbox_smart_inbox,
          all: m.mailbox_smart_all,
          unread: m.mailbox_smart_unread,
          starred: m.mailbox_smart_starred,
        }[scope.name]();
}
export class MailboxController {
  readonly store: Store<MailboxState>;
  private rowRequests = new RequestGeneration();
  private navigationRequests = new RequestGeneration();
  private detailRequests = new RequestGeneration();
  private readDwellRequests = new RequestGeneration();
  private threadRequests = new RequestGeneration();
  private metadataRequests = new RequestGeneration();
  private remoteRequests = new RequestGeneration();
  private folders = new Map<string, UiFolder[]>();
  private rows = new Map<string, Cached>();
  private unreadPin: string | null = null;
  private details = new Map<string, UiMessage>();
  private openingReads = new Map<string, Promise<void>>();
  private manualSeen = new Set<string>();
  private folderLoads = new Map<string, Promise<void>>();
  private backgroundSyncs = new Map<string, { timer: ReturnType<typeof setTimeout> }>();
  private context = 0;
  private stopped = false;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private readDwellTimer: ReturnType<typeof setTimeout> | null = null;
  private previousSearch: { scope: Scope; selection: UiSummary | null } | null =
    null;
  constructor(
    private readonly bridge: MailBridge,
    private readonly onSelection?: (
      selection: { identity: MessageIdentity; subject: string } | null,
    ) => void,
  ) {
    this.store = new Store<MailboxState>({
      snapshot: {
        frontend_id: "",
        phase: "disconnected",
        attempt: null,
        retry_in_ms: null,
        revision: -1,
        connection_epoch: 0,
        version: null,
        accounts: [],
        principal: null,
        identity_failure: null,
        partial: false,
        compat: "unknown",
      },
      scope: { kind: "smart", name: "inbox" },
      account: "",
      folders: [],
      folder: null,
      profiles: [],
      rows: [],
      total: 0,
      nextOffset: 0,
      selected: null,
      selectedRow: null,
      detail: null,
      checked: [],
      busy: false,
      report: null,
      prompt: null,
      loadingFolders: false,
      loadingRows: false,
      loadingDetail: false,
      folderError: null,
      rowsError: null,
      detailError: null,
      index: null,
      indexError: null,
      threadOpen: false,
      threadRoot: null,
      threadId: 0,
      threadRows: [],
      threadTotal: 0,
      threadOffset: 0,
      threadLoading: false,
      threadError: null,
      syncingScopes: new Map(),
      syncError: null,
      syncNotice: null,
      remote: idleRemote(),
    });
  }
  navigationGuard(){return this.navigationRequests.begin();}
  invalidateNavigation() {
    this.navigationRequests.invalidate();
    this.cancelReadDwell();
  }
  get state() {
    return this.store.getSnapshot();
  }
  private get unreadPinCount() {
    return this.unreadPin === null ? 0 : 1;
  }
  get canLoadMore() {
    return (
      this.state.nextOffset < this.state.total - this.unreadPinCount &&
      this.state.rows.length - this.unreadPinCount < MAX_ROWS &&
      !this.state.loadingRows &&
      this.state.snapshot.phase === "online"
    );
  }
  get capped() {
    return (
      this.state.rows.length - this.unreadPinCount >= MAX_ROWS &&
      this.state.nextOffset < this.state.total - this.unreadPinCount
    );
  }
  label(account: string) {
    const p = this.state.profiles.find((p) => p.id === account);
    return p?.display_name.trim() || p?.address.trim() || account;
  }
  private patch(p: Partial<MailboxState>) {
    this.store.patch(p);
  }
  stop() {
    this.stopped = true;
    this.cancelBackgroundSyncs();
    this.invalidate();
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
  }
  private invalidate() {
    this.cancelReadDwell();
    this.folderLoads.clear();
    this.navigationRequests.invalidate();
    this.rowRequests.invalidate();
    this.detailRequests.invalidate();
    this.threadRequests.invalidate();
    this.metadataRequests.invalidate();
    this.remoteRequests.invalidate();
    this.context++;
    this.patch({
      loadingRows: false,
      loadingDetail: false,
      loadingFolders: false,
      threadLoading: false,
      ...(this.state.remote.status === "loading"
        ? { remote: { ...this.state.remote, status: "idle" as const } }
        : {}),
    });
  }
  acceptSnapshot(snapshot: UiSnapshot) {
    const before = this.state.snapshot;
    if (
      snapshot.connection_epoch === before.connection_epoch &&
      snapshot.revision <= before.revision
    )
      return;
    const reconnect =
      snapshot.connection_epoch !== before.connection_epoch ||
      before.phase !== "online";
    if (snapshot.connection_epoch !== before.connection_epoch)
      this.cancelBackgroundSyncs();
    if (snapshot.phase !== "online" || reconnect) this.invalidate();
    this.patch({ snapshot });
    if (snapshot.phase === "online") {
      if (reconnect) void this.refresh();
      else {
        if (this.refreshTimer) clearTimeout(this.refreshTimer);
        this.refreshTimer = setTimeout(() => {
          this.refreshTimer = null;
          void this.refresh(false);
        }, 250);
      }
    }
  }
  foldersOf(account: string) {
    return this.folders.get(account) ?? [];
  }
  preferredInbox(account: string) {
    const present = this.foldersOf(account).filter((f) => f.present);
    const unclassified = present.filter((f) => f.is_primary === undefined);
    return present.find((f) => f.is_primary === true) ??
      unclassified.find((f) => f.name.toUpperCase() === "INBOX") ??
      unclassified.find((f) => f.role === "inbox");
  }
  async ensureFolders(account: string, force = false): Promise<void> {
    if (this.state.snapshot.phase !== "online" || this.stopped) return;
    const existing = this.folderLoads.get(account);
    if (existing) return existing;
    if (this.folders.has(account) && !force) return;
    const epoch = this.state.snapshot.connection_epoch;
    const task = (async () => {
      try {
        const folders = await this.bridge.folders(account);
        if (
          this.stopped ||
          epoch !== this.state.snapshot.connection_epoch ||
          this.state.snapshot.phase !== "online"
        )
          return;
        if (folders.some((f) => f.account !== account))
          throw new Error("scope mismatch");
        this.folders.set(account, folders);
        const current = this.state.scope.kind === "unread"
          ? this.state.scope.base : this.state.scope;
        this.patch(
          current.kind === "folder" && current.account === account
            ? {
                folders,
                folder:
                  folders.find((f) => f.name === current.folder) ??
                  this.state.folder,
              }
            : {},
        );
      } catch (e) {
        if (!this.stopped && epoch === this.state.snapshot.connection_epoch)
          this.patch({
            folderError: describe(e, m.mailbox_error_folders()),
          });
      }
    })();
    this.folderLoads.set(account, task);
    return task.finally(() => {
      if (this.folderLoads.get(account) === task)
        this.folderLoads.delete(account);
    });
  }
  private async metadata() {
    const current = this.metadataRequests.begin();
    this.patch({ loadingFolders: true, folderError: null });
    const accounts = this.state.snapshot.accounts.map((a) => a.account);
    const profiles = this.bridge.mailbox
      .profiles()
      .then((profiles) => {
        if (current() && !this.stopped) this.patch({ profiles });
      })
      .catch(() => {});
    await Promise.all([
      ...accounts.map((a) => this.ensureFolders(a, true)),
      profiles,
    ]);
    if (current() && !this.stopped) this.patch({ loadingFolders: false });
  }
  selectAccount(account: string) {
    if (!this.state.snapshot.accounts.some((a) => a.account === account))
      return;
    const choose = () => {
      const fs = this.foldersOf(account);
      const f =
        this.preferredInbox(account) ??
        fs.find((f) => f.present) ??
        fs[0];
      if (f) this.selectFolder(f);
    };
    if (this.folders.has(account)) choose();
    else {
      const current = this.navigationRequests.begin();
      void this.ensureFolders(account).then(() => {
        if (current()) choose();
      });
    }
  }
  selectSmart(name: SmartFolder) {
    this.previousSearch = null;
    this.changeScope({ kind: "smart", name });
  }
  selectGroup(id: string, name: string, accounts: string[], unread = false) {
    this.previousSearch = null;
    const base = { kind: "smart" as const, name: "inbox" as const, group: { id, name, accounts } };
    this.changeScope(unread ? { kind: "unread", base } : base);
  }
  /** List filters use listMessageSummaries, never the full-text search protocol. */
  setUnreadFilter(unread: boolean) {
    const current = this.state.scope;
    if (current.kind === "search") return;
    const base = current.kind === "unread" ? current.base : current;
    this.previousSearch = null;
    this.changeScope(unread ? { kind: "unread", base }
      : base.kind === "smart" && base.name === "unread" ? { kind: "smart", name: "all" } : base);
  }
  selectFolder(folder: UiFolder) {
    this.previousSearch = null;
    this.changeScope({
      kind: "folder",
      account: folder.account,
      folder: folder.name,
      title: folderLabel(folder),
    });
  }
  private changeScope(scope: Scope) {
    this.context++;
    this.rowRequests.invalidate();
    this.remoteRequests.invalidate();
    this.clearSelection();
    const cached = this.rows.get(scopeKey(scope));
    const source = scope.kind === "unread" ? scope.base : scope;
    const account = source.kind === "folder" ? source.account : "";
    const folders = this.foldersOf(account);
    this.patch({
      scope,
      account,
      folders,
      folder:
        source.kind === "folder"
          ? (folders.find((f) => f.name === source.folder) ?? null)
          : null,
      rows: cached?.rows ?? [],
      total: cached?.total ?? 0,
      nextOffset: cached?.nextOffset ?? 0,
      checked: [],
      report: null,
      prompt: null,
      loadingRows: false,
      rowsError: null,
      index: null,
      indexError: null,
      remote: idleRemote(),
    });
    if (this.state.snapshot.phase === "online") void this.refreshRows(false);
  }
  search(query: MailboxQuery, meta?: SearchFilterMeta) {
    if (this.state.scope.kind !== "search")
      this.previousSearch = {
        scope: this.state.scope,
        selection: this.state.selectedRow,
      };
    const nextQuery = canonicalSearchQuery(query);
    this.changeScope(
      meta
        ? { kind: "search", query: nextQuery, meta }
        : { kind: "search", query: nextQuery },
    );
  }
  exitSearch() {
    const previous = this.previousSearch;
    this.previousSearch = null;
    this.changeScope(previous?.scope ?? { kind: "smart", name: "inbox" });
    if (previous?.selection) this.openRow(previous.selection);
  }
  selectMessage(identity: MessageIdentity, preserveThread = false) {
    const row = [...this.state.rows, ...this.state.threadRows].find(
      (r) => messageKey(r) === messageKey(identity),
    );
    if (!row) return;
    this.openRow(row, { preserveThread, markRead: true });
  }
  private openRow(
    row: UiSummary,
    { preserveThread = false, markRead = false } = {},
  ) {
    const key = messageKey(row);
    if (markRead || !this.state.selected || key !== messageKey(this.state.selected))
      this.cancelReadDwell();
    const readCurrent = markRead ? this.readDwellRequests.begin() : null;
    this.navigationRequests.invalidate();
    if (!preserveThread) {
      this.threadRequests.invalidate();
      this.patch({
        threadOpen: false,
        threadRoot: null,
        threadRows: [],
        threadTotal: 0,
        threadOffset: 0,
        threadError: null,
        threadLoading: false,
      });
    }
    const current = this.detailRequests.begin();
    const cached = this.details.get(key);
    this.setSelected(this.withOpeningRead(row));
    this.patch({
      detail: cached ?? null,
      detailError: null,
      loadingDetail: !cached,
    });
    if (this.state.snapshot.phase !== "online") {
      this.patch({
        loadingDetail: false,
        detailError: cached
          ? null
          : m.mailbox_error_detail_offline(),
      });
      return;
    }
    const pendingRead = this.openingReads.get(key);
    const load = () => this.bridge.message(row);
    void (pendingRead ? pendingRead.then(load) : load())
      .then((detail) => {
        if (!current() || this.stopped) return;
        if (messageKey(detail) !== key) throw new Error("identity mismatch");
        detail = this.withOpeningRead(detail);
        boundedSet(this.details, key, detail, 24);
        this.patch({ detail, selectedRow: detail, loadingDetail: false });
        if (readCurrent?.()) this.scheduleReadDwell(detail);
      })
      .catch((e) => {
        if (current() && !this.stopped)
          this.patch({
            loadingDetail: false,
            detailError: describe(e, m.mailbox_error_detail()),
          });
      });
  }
  private cancelReadDwell() {
    this.readDwellRequests.invalidate();
    if (this.readDwellTimer !== null) clearTimeout(this.readDwellTimer);
    this.readDwellTimer = null;
  }
  private scheduleReadDwell(detail: UiMessage) {
    if (detail.seen) return;
    const key = messageKey(detail);
    this.readDwellTimer = setTimeout(() => {
      this.readDwellTimer = null;
      const { selected, selectedRow, detail: latest } = this.state;
      if (
        !this.stopped &&
        selected &&
        messageKey(selected) === key &&
        !selectedRow?.seen
      )
        this.markOpenedRead(latest && messageKey(latest) === key ? latest : detail);
    }, MARK_READ_DWELL_MS);
  }
  private withOpeningRead<T extends UiSummary>(row: T): T {
    return !row.seen && this.openingReads.has(messageKey(row))
      ? { ...row, seen: true }
      : row;
  }
  private updateSeen(key: string, seen: boolean) {
    const update = <T extends UiSummary>(row: T): T =>
      messageKey(row) === key && row.seen !== seen ? { ...row, seen } : row;
    const { rows, threadRows, remote, detail, selectedRow } = this.state;
    for (const cached of this.rows.values()) cached.rows = cached.rows.map(update);
    const cached = this.details.get(key);
    if (cached) this.details.set(key, update(cached));
    this.patch({
      rows: rows.map(update),
      threadRows: threadRows.map(update),
      remote: { ...remote, rows: remote.rows.map(update) },
      selectedRow: selectedRow && update(selectedRow),
      detail: detail && update(detail),
    });
  }
  private markOpenedRead({ account, folder, uid, seen }: UiMessage) {
    const { snapshot, remote } = this.state;
    const key = messageKey({ account, folder, uid });
    if (
      seen ||
      snapshot.phase !== "online" ||
      compatBlocksWrites(snapshot) ||
      this.openingReads.has(key) ||
      this.manualSeen.has(key) ||
      (this.isRemote({ account, folder, uid }) && !remote.fetched.includes(key))
    )
      return;
    const context = this.context;
    const write = Promise.resolve().then(async () => {
      let error: string | null = null;
      try {
        if (
          this.stopped ||
          this.state.snapshot.phase !== "online" ||
          this.state.snapshot.connection_epoch !== snapshot.connection_epoch
        )
          throw new Error(m.mailbox_error_connection_changed());
        const receipt = await this.bridge.mailbox.write({
          action: "mark",
          flag: "seen",
          value: true,
          account,
          folder,
          uids: [uid],
        });
        error = validateReceipt(receipt, [uid]).error;
      } catch (e) {
        error = unconfirmedWrite(e);
      } finally {
        this.openingReads.delete(key);
      }
      if (this.stopped) return;
      if (error) {
        this.updateSeen(key, false);
        if (context === this.context)
          this.patch({ report: { requested: 1, queued: 0, unchanged: 0, unconfirmed: 1, error } });
      }
      void this.refresh(false);
    });
    this.openingReads.set(key, write);
    this.updateSeen(key, true);
  }
  async locate(account: string, uid: string, folder?: string) {
    if (!folder) {
      this.selectAccount(account);
      this.patch({
        rowsError: m.mailbox_error_intent_no_folder(),
      });
      return;
    }
    this.context++;
    const epoch = this.state.snapshot.connection_epoch;
    this.clearSelection();
    const current = this.navigationRequests.begin();
    try {
      const message = await this.bridge.mailbox.locate({
        account,
        folder,
        uid,
      });
      if (
        !current() ||
        epoch !== this.state.snapshot.connection_epoch ||
        this.state.snapshot.phase !== "online"
      )
        return;
      if (messageKey(message) !== messageKey({ account, folder, uid }))
        throw new Error("identity mismatch");
      const source = this.foldersOf(account).find(
        (f) => f.name === message.folder,
      );
      this.changeScope({
        kind: "folder",
        account,
        folder: message.folder,
        title: folderLabel(source, message.folder),
      });
      boundedSet(this.details, messageKey(message), message, 24);
      this.openRow(message, { markRead: true });
    } catch (e) {
      if (current())
        this.patch({
          detailError: describe(e, m.mailbox_error_locate()),
        });
    }
  }
  private setSelected(row: UiSummary | null) {
    const identity = row
      ? { account: row.account, folder: row.folder, uid: row.uid }
      : null;
    const pin = this.unreadPin;
    if (pin !== null && (!identity || messageKey(identity) !== pin)) {
      this.unreadPin = null;
      this.patch({
        selected: identity,
        selectedRow: row,
        rows: this.state.rows.filter((r) => messageKey(r) !== pin),
        checked: this.state.checked.filter((r) => messageKey(r) !== pin),
        total: this.state.total - 1,
      });
    } else this.patch({ selected: identity, selectedRow: row });
    this.onSelection?.(
      row ? { identity: identity!, subject: row.subject } : null,
    );
  }
  clearSelection() {
    this.cancelReadDwell();
    this.navigationRequests.invalidate();
    this.detailRequests.invalidate();
    this.threadRequests.invalidate();
    this.setSelected(null);
    this.patch({
      detail: null,
      detailError: null,
      loadingDetail: false,
      threadOpen: false,
      threadRoot: null,
      threadRows: [],
      threadTotal: 0,
      threadOffset: 0,
      threadLoading: false,
      threadError: null,
    });
  }
  reloadSelected() {
    const row = this.state.selectedRow;
    if (row) {
      this.details.delete(messageKey(row));
      this.openRow(row, { preserveThread: true });
    }
  }
  refresh = async (refreshDetail=true) => {
    if (this.state.snapshot.phase !== "online" || this.stopped) return;
    const selected = this.state.selected;
    const epoch = this.state.snapshot.connection_epoch;
    await Promise.all([this.metadata(), this.refreshRows(false)]);
    if (
      refreshDetail &&
      selected &&
      this.state.selected &&
      messageKey(selected) === messageKey(this.state.selected) &&
      epoch === this.state.snapshot.connection_epoch &&
      this.state.snapshot.phase === "online" &&
      !this.stopped
    )
      this.reloadSelected();
  };
  loadMore = async () => {
    if (this.canLoadMore) await this.refreshRows(true);
  };
  private async refreshRows(append: boolean) {
    if (this.state.snapshot.phase !== "online" || this.stopped) return;
    const scope = this.state.scope;
    const key = scopeKey(scope);
    const previous = append
      ? this.state.rows.filter((r) => messageKey(r) !== this.unreadPin)
      : [];
    const offset = append ? this.state.nextOffset : 0;
    const current = this.rowRequests.begin();
    this.patch({ loadingRows: true, rowsError: null });
    if (scope.kind === "search")
      void this.bridge.mailbox
        .index()
        .then((index) => {
          if (current()) this.patch({ index, indexError: null });
        })
        .catch(() => {
          if (current())
            this.patch({ indexError: m.mailbox_error_index() });
        });
    try {
      const q = queryOf(scope);
      const read = (at: number) =>
        scope.kind === "folder"
          ? this.bridge.messages(
              { account: scope.account, folder: scope.folder },
              at,
              PAGE_SIZE,
            )
          : this.bridge.mailbox.query(q, at);
      const page = await read(offset);
      if (!append) {
        const goal = Math.max(PAGE_SIZE, this.state.rows.length - this.unreadPinCount);
        while (current() && page.messages.length < Math.min(goal, page.total)) {
          const next = await read(page.messages.length);
          if (!next.messages.length) throw new Error("empty incomplete page");
          page.messages.push(...next.messages);
          page.total = next.total;
        }
      }
      if (!current() || this.stopped) return;
      if (page.messages.some((r) => !matchesMailbox(r, q)))
        throw new Error("scope mismatch");
      const rows = [
        ...new Map(
          [...previous, ...page.messages].map((r) => [messageKey(r), this.withOpeningRead(r)]),
        ).values(),
      ].slice(0, MAX_ROWS);
      const nextOffset = offset + page.messages.length;
      if (page.messages.length === 0 && nextOffset < page.total)
        throw new Error("empty incomplete page");
      boundedSet(this.rows, key, { rows, total: page.total, nextOffset }, 32);
      const selected = this.state.selectedRow;
      const selectedKey = selected && messageKey(selected);
      const inPage = selectedKey && rows.find((r) => messageKey(r) === selectedKey);
      const pinIndex =
        q.seen === false &&
        !q.search &&
        selected?.seen &&
        !inPage &&
        matchesMailbox(selected, q) &&
        (q.flagged === undefined || selected.flagged === q.flagged) &&
        (!q.role || this.foldersOf(selected.account).some(
          (folder) => folder.name === selected.folder && folder.role === q.role,
        ))
          ? unreadPinIndex(selected, rows, this.state.rows, q.accounts !== undefined)
          : -1;
      const visibleRows = pinIndex < 0 ? rows : [...rows];
      // The open read message keeps its place without taking up a daemon page slot.
      this.unreadPin = pinIndex >= 0 ? selectedKey : null;
      if (selected && pinIndex >= 0) visibleRows.splice(pinIndex, 0, selected);
      this.patch({
        rows: visibleRows,
        total: page.total + this.unreadPinCount,
        nextOffset,
        loadingRows: false,
        checked: this.state.checked.filter((i) =>
          visibleRows.some((r) => messageKey(i) === messageKey(r)),
        ),
        ...(inPage ? { selectedRow: inPage } : {}),
      });
    } catch (e) {
      if (current() && !this.stopped)
        this.patch({
          loadingRows: false,
          rowsError: describe(e, m.mailbox_error_rows()),
        });
    }
  }
  isSyncing = (account = queryOf(this.state.scope).account) => {
    const scopes = this.state.syncingScopes;
    return scopes.has(undefined) || (account === undefined ? scopes.size > 0 : scopes.has(account));
  };
  isSyncPending = (account = queryOf(this.state.scope).account) =>
    this.isSyncing(account) ||
    (account === undefined ? this.backgroundSyncs.size > 0 : this.backgroundSyncs.has(account));

  private cancelBackgroundSyncs() {
    for (const sync of this.backgroundSyncs.values()) clearTimeout(sync.timer);
    this.backgroundSyncs.clear();
    if (this.state.syncNotice !== null) this.patch({ syncNotice: null });
  }

  private async refreshAfterSync(account?: string) {
    if (account !== undefined) await this.folderLoads.get(account);
    await this.refresh();
  }

  private trackBackgroundSync(account: string, syncId: number) {
    const mailbox = this.bridge.mailbox;
    const status = mailbox.syncStatus?.bind(mailbox);
    if (!status) return;
    const deadline = Date.now() + BACKGROUND_SYNC_WAIT_MS;
    const current = () => this.backgroundSyncs.get(account) === sync;
    const poll = async () => {
      if (!current()) return;
      let result: { syncError?: string } = {};
      try {
        if (Date.now() < deadline && !(await status(account, syncId))) {
          if (current()) sync.timer = setTimeout(poll, BACKGROUND_SYNC_POLL_MS);
          return;
        }
      } catch (e) {
        result = { syncError: describe(e, m.mailbox_error_sync()) };
      }
      if (!current()) return;
      try {
        await this.refreshAfterSync(account);
      } finally {
        if (current()) {
          this.backgroundSyncs.delete(account);
          this.patch({
            syncNotice: this.backgroundSyncs.size ? m.mailbox_sync_background() : null,
            ...result,
          });
        }
      }
    };
    const sync = { timer: setTimeout(poll, BACKGROUND_SYNC_POLL_MS) };
    this.backgroundSyncs.set(account, sync);
  }
  /**
   * 「立即同步」：先敲醒后台检查新邮件（后台不支持时静默降级），再刷新当前列表。
   */
  syncNow = async (scope?: { account: string; folder?: string }) => {
    const account = scope?.account ?? queryOf(this.state.scope).account;
    if (this.state.snapshot.phase !== "online" || this.stopped || this.isSyncPending(account)) return;
    const epoch = this.state.snapshot.connection_epoch;
    this.patch({
      syncingScopes: new Map(this.state.syncingScopes).set(account, { folder: scope?.folder }),
      syncError: null,
    });
    const finish = (result: { syncError?: string; syncNotice?: string }) => {
      if (this.stopped) return;
      const syncingScopes = new Map(this.state.syncingScopes);
      syncingScopes.delete(account);
      this.patch({ syncingScopes, ...result });
    };
    let syncError: string | null = null;
    let background = false;
    try {
      if (scope && this.bridge.mailbox.syncScope) {
        const outcome = await this.bridge.mailbox.syncScope(scope.account, scope.folder);
        if (
          outcome !== "complete" &&
          !this.stopped &&
          epoch === this.state.snapshot.connection_epoch
        ) {
          background = true;
          this.trackBackgroundSync(scope.account, outcome.background.sync_id);
        }
      } else {
        await this.bridge.mailbox.syncNow?.(account);
      }
    } catch (e) {
      syncError = describe(e, m.mailbox_error_sync());
    }
    if (background) finish({ syncNotice: m.mailbox_sync_background() });
    try {
      await this.refreshAfterSync(scope?.account);
    } finally {
      if (!background) finish(syncError === null ? {} : { syncError });
    }
  };
  get serverSearchSupported() {
    return typeof this.bridge.mailbox.serverSearch === "function";
  }
  /** 服务器搜索的目标：选定账户（及文件夹），否则所有在线账户的收件箱。 */
  serverTargets(): FolderIdentity[] {
    const scope = this.state.scope;
    if (scope.kind !== "search") return [];
    const inbox = (account: string) => {
      const folders = this.foldersOf(account).filter((f) => f.present);
      const unknown = folders.filter((f) => f.is_search_inbox === undefined);
      return folders.find((f) => f.is_search_inbox === true)?.name ??
        unknown.find((f) => f.name.toUpperCase() === "INBOX")?.name ??
        unknown.find((f) => f.role === "inbox")?.name ??
        "INBOX";
    };
    if (scope.query.account)
      return [{ account: scope.query.account, folder: scope.query.folder ?? inbox(scope.query.account) }];
    return this.state.snapshot.accounts
      .filter((a) => a.state === "online" && (!scope.query.accounts || scope.query.accounts.includes(a.account)))
      .map((a) => ({ account: a.account, folder: inbox(a.account) }));
  }
  get canSearchServer() {
    const s = this.state;
    return (
      this.serverSearchSupported &&
      s.scope.kind === "search" &&
      !!s.scope.query.query?.trim() &&
      !hasStructuredFilters(s.scope.query) &&
      s.snapshot.phase === "online" &&
      s.remote.status !== "loading" &&
      this.serverTargets().length > 0
    );
  }
  async searchServer() {
    if (!this.canSearchServer || this.state.scope.kind !== "search") return;
    const scopeQuery = this.state.scope.query;
    const query = scopeQuery.query!.trim();
    const since = scopeQuery.since;
    const until = scopeQuery.until;
    const targets = this.serverTargets();
    const current = this.remoteRequests.begin();
    const epoch = this.state.snapshot.connection_epoch;
    this.patch({
      remote: { ...idleRemote(), status: "loading", query, accounts: targets.map((t) => t.account) },
    });
    const inRange = (r: UiSummary) =>
      (since === undefined || r.sort_at >= since) &&
      (until === undefined || (r.sort_at > 0 && r.sort_at <= until));
    const found: UiSummary[] = [];
    let failed = 0,
      lastError: unknown = null;
    for (const target of targets) {
      try {
        const rows = await this.bridge.mailbox.serverSearch!(
          target.account,
          target.folder,
          query,
          { since, until },
        );
        if (!current() || this.stopped) return;
        // 越界结果（别的账户 / 文件夹 / 日期范围外）一律丢弃。
        found.push(
          ...rows.filter(
            (r) =>
              r.account === target.account &&
              r.folder === target.folder &&
              inRange(r),
          ),
        );
      } catch (e) {
        if (!current() || this.stopped) return;
        failed++;
        lastError = e;
      }
    }
    if (!current() || this.stopped || epoch !== this.state.snapshot.connection_epoch) return;
    const local = new Set(this.state.rows.map(messageKey));
    const rows = [
      ...new Map(found.filter((r) => !local.has(messageKey(r))).map((r) => [messageKey(r), this.withOpeningRead(r)])).values(),
    ]
      .sort((a, b) => b.sort_at - a.sort_at)
      .slice(0, 200);
    this.patch({
      remote: {
        ...this.state.remote,
        status: failed === targets.length ? "error" : "ready",
        rows: failed === targets.length ? [] : rows,
        error:
          failed === targets.length
            ? describe(lastError, m.mailbox_error_server_search())
            : failed
              ? m.mailbox_error_server_search_partial()
              : null,
      },
    });
  }
  isRemote(identity: MessageIdentity) {
    const key = messageKey(identity);
    return (
      this.state.remote.rows.some((r) => messageKey(r) === key) &&
      !this.state.rows.some((r) => messageKey(r) === key)
    );
  }
  /** 打开一封服务器历史邮件：先拉回本地库，再原位渲染（HTML 走同一条隔离文档通道）。 */
  openRemote(row: UiSummary) {
    const key = messageKey(row);
    if (this.state.remote.fetched.includes(key)) {
      this.openRow(row, { markRead: true });
      return;
    }
    const fetch = this.bridge.mailbox.fetchServerMessage;
    if (!fetch) return;
    this.cancelReadDwell();
    const readCurrent = this.readDwellRequests.begin();
    this.navigationRequests.invalidate();
    this.threadRequests.invalidate();
    this.patch({
      threadOpen: false,
      threadRoot: null,
      threadRows: [],
      threadTotal: 0,
      threadOffset: 0,
      threadError: null,
      threadLoading: false,
    });
    const current = this.detailRequests.begin();
    this.setSelected(this.withOpeningRead(row));
    this.patch({ detail: null, detailError: null, loadingDetail: true });
    if (this.state.snapshot.phase !== "online") {
      this.patch({
        loadingDetail: false,
        detailError: m.mailbox_error_remote_offline(),
      });
      return;
    }
    void fetch
      .call(this.bridge.mailbox, row.account, row.folder, row.uid)
      .then((detail) => {
        if (!current() || this.stopped) return;
        if (!detail) {
          this.patch({
            loadingDetail: false,
            detailError: m.mailbox_error_remote_gone(),
          });
          return;
        }
        if (messageKey(detail) !== key) throw new Error("identity mismatch");
        detail = this.withOpeningRead(detail);
        boundedSet(this.details, key, detail, 24);
        this.patch({
          detail,
          selectedRow: detail,
          loadingDetail: false,
          remote: {
            ...this.state.remote,
            fetched: [...this.state.remote.fetched.filter((k) => k !== key), key],
          },
        });
        if (readCurrent()) this.scheduleReadDwell(detail);
      })
      .catch((e) => {
        if (current() && !this.stopped)
          this.patch({
            loadingDetail: false,
            detailError: describe(e, m.mailbox_error_remote_fetch()),
          });
      });
  }
  /** j / k：在当前列表（或服务器历史结果）里前后移动并打开。 */
  step(delta: 1 | -1): UiSummary | null {
    const s = this.state;
    const selected = s.selected ? messageKey(s.selected) : null;
    const remote = selected !== null && this.isRemote(s.selected!);
    const list = remote ? s.remote.rows : s.rows;
    if (!list.length) return null;
    const index = selected === null ? -1 : list.findIndex((r) => messageKey(r) === selected);
    const next =
      index < 0
        ? delta > 0
          ? 0
          : list.length - 1
        : Math.max(0, Math.min(list.length - 1, index + delta));
    const row = list[next]!;
    if (index === next) return row;
    if (remote) this.openRemote(row);
    else this.openRow(row, { markRead: true });
    return row;
  }
  /** s / u：切换当前邮件的星标或已读。尚未拉回本地的服务器结果不能写。 */
  async toggleSelected(flag: "seen" | "flagged") {
    const row = this.state.selectedRow;
    if (!row) return null;
    const key = messageKey(row);
    if (this.isRemote(row) && !this.state.remote.fetched.includes(key)) return null;
    return this.perform({ action: "mark", flag, value: !row[flag] }, [row]);
  }
  private anchor: string | null = null;
  toggle(row: UiSummary, range = false) {
    if (this.state.busy) return;
    const key = messageKey(row),
      checked = new Map(this.state.checked.map((i) => [messageKey(i), i]));
    const a = this.state.rows.findIndex((r) => messageKey(r) === this.anchor),
      b = this.state.rows.findIndex((r) => messageKey(r) === key);
    if (range && a >= 0 && b >= 0) {
      for (const r of this.state.rows.slice(Math.min(a, b), Math.max(a, b) + 1))
        checked.set(messageKey(r), r);
    } else if (checked.has(key)) checked.delete(key);
    else checked.set(key, row);
    this.anchor = key;
    this.patch({ checked: [...checked.values()], report: null });
  }
  selectLoaded(value: boolean) {
    if (!this.state.busy)
      this.patch({
        checked: value
          ? this.state.rows.map((r) => ({
              account: r.account,
              folder: r.folder,
              uid: r.uid,
            }))
          : [],
        report: null,
      });
  }
  targets(): UiSummary[] {
    const ids = this.state.checked.length
      ? this.state.checked
      : this.state.selected
        ? [this.state.selected]
        : [];
    return ids
      .map((i) =>
        [
          ...this.state.rows,
          ...this.state.threadRows,
          ...(this.state.selectedRow ? [this.state.selectedRow] : []),
        ].find((r) => messageKey(r) === messageKey(i)),
      )
      .filter((r): r is UiSummary => !!r);
  }
  dismissReport() {
    this.patch({ report: null });
  }
  closePrompt() {
    if (!this.state.busy) this.patch({ prompt: null });
  }
  /** Defaults to the checked set or current message; context menus pass the row they were opened on. */
  requestAction(kind: "move" | "delete", rows = this.targets()) {
    if (this.state.busy || this.state.snapshot.phase !== "online") return;
    if (!rows.length) return;
    if (kind === "move" && new Set(rows.map((r) => r.account)).size !== 1) {
      this.patch({
        report: {
          requested: rows.length,
          queued: 0,
          unconfirmed: rows.length,
          error: m.mailbox_error_cross_account(),
        },
      });
      return;
    }
    this.patch({ prompt: { kind, rows: rows.map((r) => ({ ...r })) } });
  }
  async perform(
    action: MailAction,
    targets = this.targets(),
  ): Promise<BatchReport | null> {
    if (
      this.state.busy ||
      !targets.length ||
      this.state.snapshot.phase !== "online"
    )
      return null;
    const frozen = [
      ...new Map(targets.map((r) => [messageKey(r), { ...r }])).values(),
    ];
    const context = this.context,
      epoch = this.state.snapshot.connection_epoch;
    const writesSeen = action.action === "mark" && action.flag === "seen";
    if (writesSeen) {
      const selected = this.state.selected && messageKey(this.state.selected);
      for (const row of frozen) {
        const key = messageKey(row);
        this.manualSeen.add(key);
        if (key === selected) this.cancelReadDwell();
      }
    }
    this.patch({ busy: true, report: null });
    let queued = 0,
      unchanged = 0;
    const accepted = new Set<string>();
    let error: string | null = null;
    try {
      const groups = new Map<string, UiSummary[]>();
      for (const row of frozen) {
        const k = folderKey(row);
        groups.set(k, [...(groups.get(k) ?? []), row]);
      }
      if (
        action.action === "move" &&
        new Set(frozen.map((r) => r.account)).size > 1
      )
        throw new Error(m.mailbox_error_cross_account());
      for (const group of groups.values())
        for (let offset = 0; offset < group.length; offset += 500) {
          const rows = group.slice(offset, offset + 500),
            first = rows[0];
          try {
            if (writesSeen) {
              const opening = rows.flatMap((row) => this.openingReads.get(messageKey(row)) ?? []);
              if (opening.length) await Promise.all(opening);
            }
            if (
              this.state.snapshot.phase !== "online" ||
              this.state.snapshot.connection_epoch !== epoch
            )
              throw new Error(m.mailbox_error_connection_changed());
            let request: WriteRequest;
            if (action.action === "archive") {
              const target = this.foldersOf(first.account).find(
                (f) =>
                  f.present && f.role === "archive" && f.name !== first.folder,
              );
              if (!target) {
                error=m.mailbox_error_no_archive({ account: this.label(first.account) });
                continue;
              }
              request = {
                action: "move",
                target: target.name,
                account: first.account,
                folder: first.folder,
                uids: rows.map((r) => r.uid),
              };
            } else
              request = {
                ...action,
                account: first.account,
                folder: first.folder,
                uids: rows.map((r) => r.uid),
              };
            const receipt = await this.bridge.mailbox.write(request);
            const result = validateReceipt(receipt, request.uids);
            queued += result.queued;
            unchanged += result.unchanged;
            for (const uid of result.accepted)
              accepted.add(messageKey({ ...first, uid }));
            if (result.error) error = result.error;
          } catch (e) {
            error = unconfirmedWrite(e);
          }
        }
    } catch (e) {
      error = e instanceof Error ? e.message : m.mailbox_error_unconfirmed();
    } finally {
      if (writesSeen)
        for (const row of frozen) this.manualSeen.delete(messageKey(row));
      this.rows.clear();
      this.details.clear();
      const selected = this.state.selectedRow;
      if (selected && accepted.has(messageKey(selected))) {
        if (action.action === "mark")
          this.patch({
            selectedRow: { ...selected, [action.flag]: action.value },
          });
        else this.clearSelection();
      }
      const report = {
        requested: frozen.length,
        queued,
        unconfirmed: frozen.length - queued - unchanged,
        unchanged,
        error,
      };
      if (context === this.context)
        this.patch({
          report,
          checked: [
            ...this.state.checked.filter(
              (r) => !frozen.some((f) => messageKey(f) === messageKey(r)),
            ),
            ...frozen.filter((r) => !accepted.has(messageKey(r))),
          ],
        });
      this.patch({ busy: false });
      // Refresh after the durable receipts, not optimistic JS mutations.
      if (this.state.snapshot.phase === "online") void this.refresh(false);
    }
    return {
      requested: frozen.length,
      queued,
      unconfirmed: frozen.length - queued - unchanged,
      unchanged,
      error,
    };
  }
  async openThread() {
    if (this.state.threadOpen) {
      this.threadRequests.invalidate();
      this.patch({ threadOpen: false, threadLoading: false });
      return;
    }
    const row = this.state.selectedRow;
    if (!row) return;
    this.threadRequests.invalidate();
    this.patch({
      threadOpen: true,
      threadLoading: false,
      threadRoot: { account: row.account, folder: row.folder, uid: row.uid },
      threadRows: [],
      threadId: 0,
      threadOffset: 0,
      threadTotal: 0,
      threadError: null,
    });
    await this.moreThread();
  }
  async moreThread() {
    const s = this.state;
    if (
      !s.threadRoot ||
      s.threadLoading ||
      s.threadRows.length >= MAX_ROWS ||
      s.snapshot.phase !== "online"
    )
      return;
    const current = this.threadRequests.begin(),
      root = s.threadRoot,
      offset = s.threadOffset;
    this.patch({ threadLoading: true, threadError: null });
    try {
      const page = await this.bridge.mailbox.thread(root, offset);
      if (!current() || this.stopped) return;
      if (offset > 0 && page.total !== s.threadTotal)
        throw new Error("thread membership changed");
      if (
        page.messages.some((message) => message.account !== root.account) ||
        (offset > 0 && s.threadId !== page.thread_id)
      )
        throw new Error("thread changed");
      const rows = [
        ...new Map(
          [...this.state.threadRows, ...page.messages].map((r) => [
            messageKey(r),
            this.withOpeningRead(r),
          ]),
        ).values(),
      ];
      if (page.messages.length === 0 && offset < page.total)
        throw new Error("empty incomplete page");
      this.patch({
        threadRows: rows,
        threadId: page.thread_id,
        threadTotal: page.total,
        threadOffset: offset + page.messages.length,
        threadLoading: false,
      });
    } catch (e) {
      if (current())
        this.patch({
          threadLoading: false,
          threadError: describe(e, m.mailbox_error_thread()),
        });
    }
  }
}
