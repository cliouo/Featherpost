import { useEffect, useState } from "react";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import type { MailboxController } from "./controller";
import { localIsoDate, MAX_SAFE_UNIX_SECS } from "./format";
import type { MailboxQuery } from "./types";

export function parseLocalDateYmd(ymd: string): { y: number; m: number; d: number } | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd.trim());
  if (!match) return null;
  const y = Number(match[1]);
  const m = Number(match[2]);
  const d = Number(match[3]);
  if (!Number.isInteger(y) || m < 1 || m > 12 || d < 1 || d > 31) return null;
  const check = new Date(y, m - 1, d, 12, 0, 0);
  check.setFullYear(y);
  if (
    Number.isNaN(check.getTime()) ||
    check.getFullYear() !== y ||
    check.getMonth() !== m - 1 ||
    check.getDate() !== d
  ) {
    return null;
  }
  return { y, m, d };
}

export function localDayStartSecs(ymd: string): number | null {
  const parsed = parseLocalDateYmd(ymd);
  if (!parsed) return null;
  const { y, m, d } = parsed;
  const date = new Date(y, m - 1, d, 0, 0, 0);
  date.setFullYear(y);
  const secs = Math.floor(date.getTime() / 1000);
  return Number.isFinite(secs) && secs >= 0 && secs <= MAX_SAFE_UNIX_SECS ? secs : null;
}

/** 次日本地零点减 1 秒：天然覆盖夏令时 23/25 小时日与午夜跳变时区。 */
export function localDayEndSecs(ymd: string): number | null {
  const parsed = parseLocalDateYmd(ymd);
  if (!parsed) return null;
  const { y, m, d } = parsed;
  const nextMidnight = new Date(y, m - 1, d + 1, 0, 0, 0);
  if (y < 100) {
    const base = new Date(0);
    base.setFullYear(y, m - 1, d + 1);
    base.setHours(0, 0, 0, 0);
    const secs = Math.floor(base.getTime() / 1000) - 1;
    return Number.isFinite(secs) && secs >= 0 && secs <= MAX_SAFE_UNIX_SECS ? secs : null;
  }
  const secs = Math.floor(nextMidnight.getTime() / 1000) - 1;
  return Number.isFinite(secs) && secs >= 0 && secs <= MAX_SAFE_UNIX_SECS ? secs : null;
}

const day = (v?: number) => {
  if (v === undefined) return "";
  if (v === 0) {
    const epoch = new Date(0);
    return `${String(epoch.getFullYear()).padStart(4, "0")}-${String(epoch.getMonth() + 1).padStart(2, "0")}-${String(epoch.getDate()).padStart(2, "0")}`;
  }
  return localIsoDate(v);
};
export function SearchBar({ controller }: { controller: MailboxController }) {
  const s = useStore(controller.store),
    [query, setQuery] = useState(""),
    [advanced, setAdvanced] = useState(false),
    [account, setAccount] = useState(""),
    [folder, setFolder] = useState(""),
    [from, setFrom] = useState(""),
    [since, setSince] = useState(""),
    [until, setUntil] = useState(""),
    [attachment, setAttachment] = useState(false),
    [error, setError] = useState("");
  const active = s.scope.kind === "search";
  useEffect(() => {
    if (s.scope.kind === "search") {
      const q = s.scope.query;
      setQuery(q.query ?? "");
      setAccount(q.account ?? "");
      setFolder(q.folder ?? "");
      setFrom(q.from ?? "");
      setSince(day(q.since));
      setUntil(day(q.until));
      setAttachment(q.has_attachments ?? false);
    }
  }, [s.scope]);
  function submit() {
    const start = since ? localDayStartSecs(since) : undefined;
    const end = until ? localDayEndSecs(until) : undefined;
    if (
      (since && start === null) ||
      (until && end === null) ||
      (start !== null && start !== undefined && end !== null && end !== undefined && start > end)
    ) {
      setError("请选择有效的日期范围。");
      return;
    }
    setError("");
    setAdvanced(false);
    controller.search({
      search: true,
      query: query.trim(),
      account: account || undefined,
      folder: folder || undefined,
      from: from.trim() || undefined,
      since: start ?? undefined,
      until: end ?? undefined,
      has_attachments: attachment || undefined,
    });
  }
  function remove(key: keyof MailboxQuery) {
    if (s.scope.kind === "search") {
      const q = { ...s.scope.query };
      delete q[key];
      if (key === "account") delete q.folder;
      controller.search(q);
    }
  }
  return (
    <div className="mail-search" data-tauri-drag-region="deep">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <Icon name="search" />
        <input
          data-mail-search
          aria-label="搜索本地邮件"
          placeholder="搜索本地邮件，按 Enter 开始（/ 聚焦）"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <kbd>⌘K</kbd>
        <button
          className="text-button"
          type="submit"
          disabled={s.snapshot.phase !== "online"}
        >
          搜索
        </button>
        <button
          className="text-button"
          type="button"
          aria-expanded={advanced}
          onClick={() => setAdvanced(!advanced)}
        >
          筛选
        </button>
        {active && (
          <button
            className="text-button"
            type="button"
            onClick={() => {
              controller.exitSearch();
              setQuery("");
              setAdvanced(false);
            }}
          >
            退出搜索
          </button>
        )}
      </form>
      {advanced && (
        <div className="mail-search-filters">
          <label>
            账户
            <select
              value={account}
              onChange={(e) => {
                setAccount(e.target.value);
                setFolder("");
              }}
            >
              <option value="">全部账户</option>
              {s.snapshot.accounts.map((a) => (
                <option key={a.account} value={a.account}>
                  {controller.label(a.account)}
                </option>
              ))}
            </select>
          </label>
          <label>
            文件夹
            <select
              value={folder}
              disabled={!account}
              onChange={(e) => setFolder(e.target.value)}
            >
              <option value="">全部文件夹</option>
              {controller.foldersOf(account).map((f) => (
                <option key={f.name} value={f.name}>
                  {f.display_name}
                  {f.present ? "" : "（本机历史）"}
                </option>
              ))}
            </select>
          </label>
          <label>
            发件人
            <input
              value={from}
              onChange={(e) => setFrom(e.target.value)}
              placeholder="名称或地址"
            />
          </label>
          <label>
            起始日期
            <input
              type="date"
              value={since}
              onChange={(e) => setSince(e.target.value)}
            />
          </label>
          <label>
            截止日期
            <input
              type="date"
              value={until}
              onChange={(e) => setUntil(e.target.value)}
            />
          </label>
          <label className="mail-checkbox">
            <input
              type="checkbox"
              checked={attachment}
              onChange={(e) => setAttachment(e.target.checked)}
            />
            有附件
          </label>
          <button
            className="primary-button"
            disabled={s.snapshot.phase !== "online"}
            onClick={submit}
          >
            应用筛选
          </button>
        </div>
      )}
      {active && (
        <div className="search-coverage">
          <span>仅搜索本机已缓存邮件</span>
          {controller.serverSearchSupported && s.remote.status === "idle" && (
            <button
              type="button"
              className="text-button"
              disabled={!controller.canSearchServer}
              title={
                s.scope.kind === "search" && !s.scope.query.query?.trim()
                  ? "请先输入关键词"
                  : "本地只缓存最近的邮件；在服务器上查找更早的匹配"
              }
              onClick={() => void controller.searchServer()}
            >
              在服务器搜索更早邮件
            </button>
          )}
          {s.remote.status === "loading" && <span role="status">正在搜索服务器…</span>}
          {s.index && !s.index.done && (
            <span role="status">
              正在建立索引，还有 {s.index.pending} 封未索引，结果尚未完整。
            </span>
          )}
          {s.indexError && <span role="status">{s.indexError}</span>}
          {Object.entries(s.scope.kind === "search" ? s.scope.query : {})
            .filter(
              ([k, v]) =>
                k !== "search" && v !== undefined && v !== false && v !== "",
            )
            .map(([key, value]) => (
              <button
                type="button"
                className="filter-chip"
                key={key}
                aria-label={`移除${({ query: "关键词", account: "账户", folder: "文件夹", from: "发件人", since: "起始日期", until: "截止日期", has_attachments: "附件" } as Record<string, string>)[key]}条件`}
                onClick={() => remove(key as keyof MailboxQuery)}
              >
                {
                  (
                    {
                      query: "关键词",
                      account: "账户",
                      folder: "文件夹",
                      from: "发件人",
                      since: "从",
                      until: "到",
                      has_attachments: "有附件",
                    } as Record<string, string>
                  )[key]
                }
                {key === "has_attachments"
                  ? ""
                  : `：${key === "account" ? controller.label(String(value)) : key === "since" || key === "until" ? day(Number(value)) : String(value)}`}{" "}
                ×
              </button>
            ))}
        </div>
      )}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
