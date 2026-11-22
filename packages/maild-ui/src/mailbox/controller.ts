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
import { errorCode } from "../bridge/errors";
import { Store } from "../host/store";
import type {
  BatchReport,
  IndexStatus,
  MailAction,
  MailboxQuery,
  Profile,
  Scope,
  SmartFolder,
  WriteRequest,
} from "./types";
const PAGE_SIZE = 50,
  MAX_ROWS = 1000;
export const folderKey = ({ account, folder }: FolderIdentity) =>
  JSON.stringify([account, folder]);
export const messageKey = ({ account, folder, uid }: MessageIdentity) =>
  JSON.stringify([account, folder, uid]);
const scopeKey = (scope: Scope) => JSON.stringify(scope);
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
  /** 「立即同步」进行中：已请求后台检查新邮件，正在刷新列表。 */
  syncing: boolean;
  syncError: string | null;
  remote: RemoteSearch;
}
interface Cached {
  rows: UiSummary[];
  total: number;
  nextOffset: number;
}
const describe = (error: unknown, fallback: string) =>
  errorCode(error) === "offline"
    ? "后台连接不可用，已读取的内容仍可查看。"
    : errorCode(error) === "denied"
      ? "当前前端没有这项操作的授权。"
      : errorCode(error) === "not_found"
        ? "这封邮件已不在本地缓存中，请刷新。"
        : fallback;
function boundedSet<K, V>(cache: Map<K, V>, key: K, value: V, limit: number) {
  cache.delete(key);
  cache.set(key, value);
  while (cache.size > limit) cache.delete(cache.keys().next().value as K);
}
export function queryOf(scope: Scope): MailboxQuery {
  if (scope.kind === "unread") return { ...queryOf(scope.base), seen: false };
  if (scope.kind === "search") return { ...scope.query, search: true };
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
export function scopeTitle(scope: Scope) {
  if (scope.kind === "unread") return scopeTitle(scope.base);
  if (scope.kind === "smart" && scope.group) return `${scope.group.name} · 收件箱`;
  return scope.kind === "folder"
    ? scope.title
    : scope.kind === "search"
      ? "搜索结果"
      : {
          inbox: "统一收件箱",
          all: "全部邮件",
          unread: "未读邮件",
          starred: "星标邮件",
        }[scope.name];
}
export class MailboxController {
  readonly store: Store<MailboxState>;
  private rowRequests = new RequestGeneration();
  private navigationRequests = new RequestGeneration();
  private detailRequests = new RequestGeneration();
  private threadRequests = new RequestGeneration();
  private metadataRequests = new RequestGeneration();
  private remoteRequests = new RequestGeneration();
  private folders = new Map<string, UiFolder[]>();
  private rows = new Map<string, Cached>();
  private details = new Map<string, UiMessage>();
  private folderLoads = new Map<string, Promise<void>>();
  private context = 0;
  private stopped = false;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
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
      syncing: false,
      syncError: null,
      remote: idleRemote(),
    });
  }
  navigationGuard(){return this.navigationRequests.begin();}
  invalidateNavigation(){this.navigationRequests.invalidate();}
  get state() {
    return this.store.getSnapshot();
  }
  get canLoadMore() {
    return (
      this.state.nextOffset < this.state.total &&
      this.state.rows.length < MAX_ROWS &&
      !this.state.loadingRows &&
      this.state.snapshot.phase === "online"
    );
  }
  get capped() {
    return (
      this.state.rows.length >= MAX_ROWS &&
      this.state.nextOffset < this.state.total
    );
  }
  label(account: string) {
    const p = this.state.profiles.find((p) => p.id === account);
    return p?.display_name || p?.address || account;
  }
  private patch(p: Partial<MailboxState>) {
    this.store.patch(p);
  }
  stop() {
    this.stopped = true;
    this.invalidate();
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
  }
  private invalidate() {
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
            folderError: describe(e, "文件夹信息暂时不可用，请刷新。"),
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
        fs.find((f) => f.role === "inbox" && f.present) ??
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
  search(query: MailboxQuery) {
    if (this.state.scope.kind !== "search")
      this.previousSearch = {
        scope: this.state.scope,
        selection: this.state.selectedRow,
      };
    this.changeScope({ kind: "search", query: { ...query, search: true } });
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
    this.openRow(row, preserveThread);
  }
  private openRow(row: UiSummary, preserveThread = false) {
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
    const key = messageKey(row);
    const cached = this.details.get(key);
    this.setSelected(row);
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
          : "这封邮件的正文尚未在本次打开时读取，后台连接恢复后可查看。",
      });
      return;
    }
    void this.bridge
      .message(row)
      .then((detail) => {
        if (!current() || this.stopped) return;
        if (messageKey(detail) !== key) throw new Error("identity mismatch");
        boundedSet(this.details, key, detail, 24);
        this.patch({ detail, selectedRow: detail, loadingDetail: false });
      })
      .catch((e) => {
        if (current() && !this.stopped)
          this.patch({
            loadingDetail: false,
            detailError: describe(e, "暂时无法读取正文，请稍后重试。"),
          });
      });
  }
  async locate(account: string, uid: string, folder?: string) {
    if (!folder) {
      this.selectAccount(account);
      this.patch({
        rowsError: "这条旧通知没有文件夹信息，请从账户列表选择邮件。",
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
      this.openRow(message);
    } catch (e) {
      if (current())
        this.patch({
          detailError: describe(e, "无法定位这封邮件，请在收件箱查找。"),
        });
    }
  }
  private setSelected(row: UiSummary | null) {
    const identity = row
      ? { account: row.account, folder: row.folder, uid: row.uid }
      : null;
    this.patch({ selected: identity, selectedRow: row });
    this.onSelection?.(
      row ? { identity: identity!, subject: row.subject } : null,
    );
  }
  clearSelection() {
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
      this.openRow(row, true);
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
    const previous = append ? this.state.rows : [];
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
            this.patch({ indexError: "暂时无法确认搜索索引是否完整。" });
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
        const goal = Math.max(PAGE_SIZE, this.state.rows.length);
        while (current() && page.messages.length < Math.min(goal, page.total)) {
          const next = await read(page.messages.length);
          if (!next.messages.length) throw new Error("empty incomplete page");
          page.messages.push(...next.messages);
          page.total = next.total;
        }
      }
      if (!current() || this.stopped) return;
      if (
        page.messages.some(
          (r) =>
            (q.account && r.account !== q.account) ||
            (q.accounts && !q.accounts.includes(r.account)) ||
            (q.folder && r.folder !== q.folder),
        )
      )
        throw new Error("scope mismatch");
      const rows = [
        ...new Map(
          [...previous, ...page.messages].map((r) => [messageKey(r), r]),
        ).values(),
      ].slice(0, MAX_ROWS);
      const nextOffset = offset + page.messages.length;
      if (page.messages.length === 0 && nextOffset < page.total)
        throw new Error("empty incomplete page");
      boundedSet(this.rows, key, { rows, total: page.total, nextOffset }, 32);
      this.patch({
        rows,
        total: page.total,
        nextOffset,
        loadingRows: false,
        checked: this.state.checked.filter((i) =>
          rows.some((r) => messageKey(i) === messageKey(r)),
        ),
      });
      // Reading a cross-folder thread or notification does not depend on presence in this page.
      const selected = this.state.selected;
      if (selected) {
        const row = rows.find((r) => messageKey(r) === messageKey(selected));
        if (row) this.patch({ selectedRow: row });
      }
    } catch (e) {
      if (current() && !this.stopped)
        this.patch({
          loadingRows: false,
          rowsError: describe(e, "暂时无法读取邮件列表，请重试。"),
        });
    }
  }
  /**
   * 「立即同步」：先敲醒后台检查新邮件（后台不支持时静默降级），再刷新当前列表。
   */
  syncNow = async () => {
    if (this.state.snapshot.phase !== "online" || this.stopped || this.state.syncing) return;
    const account = queryOf(this.state.scope).account;
    this.patch({ syncing: true, syncError: null });
    let syncError: string | null = null;
    try {
      await this.bridge.mailbox.syncNow?.(account);
    } catch (e) {
      syncError = describe(e, "后台未能立即检查新邮件，已刷新本地列表。");
    }
    try {
      await this.refresh();
    } finally {
      if (!this.stopped) this.patch({ syncing: false, syncError });
    }
  };
  get serverSearchSupported() {
    return typeof this.bridge.mailbox.serverSearch === "function";
  }
  /** 服务器搜索的目标：选定账户（及文件夹），否则所有在线账户的收件箱。 */
  serverTargets(): FolderIdentity[] {
    const scope = this.state.scope;
    if (scope.kind !== "search") return [];
    const inbox = (account: string) =>
      this.foldersOf(account).find((f) => f.role === "inbox" && f.present)?.name ?? "INBOX";
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
      ...new Map(found.filter((r) => !local.has(messageKey(r))).map((r) => [messageKey(r), r])).values(),
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
            ? describe(lastError, "服务器搜索暂不可用：后台尚未提供此能力，或服务器没有响应。")
            : failed
              ? "部分账户的服务器搜索没有完成，结果可能不全。"
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
      this.openRow(row);
      return;
    }
    const fetch = this.bridge.mailbox.fetchServerMessage;
    if (!fetch) return;
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
    this.setSelected(row);
    this.patch({ detail: null, detailError: null, loadingDetail: true });
    if (this.state.snapshot.phase !== "online") {
      this.patch({
        loadingDetail: false,
        detailError: "这封邮件还在服务器上，后台连接恢复后可拉取。",
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
            detailError: "服务器上已找不到这封邮件，可能已被移动或删除。",
          });
          return;
        }
        if (messageKey(detail) !== key) throw new Error("identity mismatch");
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
      })
      .catch((e) => {
        if (current() && !this.stopped)
          this.patch({
            loadingDetail: false,
            detailError: describe(e, "暂时无法从服务器拉取这封邮件，请稍后重试。"),
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
    else this.openRow(row);
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
          error: "不能跨账户移动，请按账户分别选择。",
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
        throw new Error("不能跨账户移动，请按账户分别选择。");
      for (const group of groups.values())
        for (let offset = 0; offset < group.length; offset += 500) {
          const rows = group.slice(offset, offset + 500),
            first = rows[0];
          try {
            if (
              this.state.snapshot.phase !== "online" ||
              this.state.snapshot.connection_epoch !== epoch
            )
              throw new Error("连接已变化，剩余操作没有继续提交。");
            let request: WriteRequest;
            if (action.action === "archive") {
              const target = this.foldersOf(first.account).find(
                (f) =>
                  f.present && f.role === "archive" && f.name !== first.folder,
              );
              if (!target) {
                error=`${this.label(first.account)} 没有可用归档文件夹，请使用“移动”选择目标。`;
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
            if (
              !Number.isInteger(receipt.queued) ||
              receipt.queued < 0 ||
              receipt.queued > rows.length
            )
              throw new Error("后台回执无效，请刷新核对。");
            if (
              receipt.queued_uids &&
              receipt.unchanged_uids &&
              receipt.missing_uids
            ) {
              const ids = [
                ...receipt.queued_uids,
                ...receipt.unchanged_uids,
                ...receipt.missing_uids,
              ];
              if (
                ids.length !== rows.length ||
                new Set(ids).size !== ids.length ||
                ids.some((id) => !rows.some((r) => r.uid === id)) ||
                receipt.queued_uids.length !== receipt.queued
              )
                throw new Error("后台回执身份不完整，请刷新核对。");
              queued += receipt.queued;
              unchanged += receipt.unchanged_uids.length;
              for (const row of rows)
                if (
                  receipt.queued_uids.includes(row.uid) ||
                  receipt.unchanged_uids.includes(row.uid)
                )
                  accepted.add(messageKey(row));
              if (receipt.missing_uids.length)
                error = "部分邮件已变化或不在本地，请刷新核对。";
            } else {
              queued += receipt.queued;
              if (receipt.queued === rows.length)
                for (const row of rows) accepted.add(messageKey(row));
              else error = "部分邮件未确认，请刷新核对。";
            }
          } catch (e) {
            error = errorCode(e)
              ? "部分提交未确认。请先刷新或查看同步活动，切勿直接重复移动。"
              : e instanceof Error
                ? e.message
                : "操作未确认。";
          }
        }
    } catch (e) {
      error = e instanceof Error ? e.message : "操作未确认。";
    } finally {
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
        page.messages.some((m) => m.account !== root.account) ||
        (offset > 0 && s.threadId !== page.thread_id)
      )
        throw new Error("thread changed");
      const rows = [
        ...new Map(
          [...s.threadRows, ...page.messages].map((r) => [messageKey(r), r]),
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
          threadError: describe(e, "会话暂时无法完整读取，请关闭后重新展开。"),
        });
    }
  }
}
