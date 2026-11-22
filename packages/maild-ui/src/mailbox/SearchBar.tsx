import { m } from "@featherpost/i18n/messages";

import { folderLabel, useUiLocale } from "../i18n";
import { Choice } from "../settings/Choice";
import { useEffect, useState } from "react";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import type { MailboxController } from "./controller";
import { localIsoDate, MAX_SAFE_UNIX_SECS, plural } from "./format";
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

/** 筛选条件名：移除按钮的无障碍名称用。 */
const FILTER_FIELD: Record<string, () => string> = {
  query: m.mailbox_search_field_query,
  account: m.mailbox_search_field_account,
  folder: m.mailbox_search_field_folder,
  from: m.mailbox_search_field_from,
  since: m.mailbox_search_field_since,
  until: m.mailbox_search_field_until,
  has_attachments: m.mailbox_search_field_attachments,
};
/** 筛选条件小标签上的短名。 */
const FILTER_CHIP: Record<string, () => string> = {
  query: m.mailbox_search_chip_query,
  account: m.mailbox_search_chip_account,
  folder: m.mailbox_search_chip_folder,
  from: m.mailbox_search_chip_from,
  since: m.mailbox_search_chip_since,
  until: m.mailbox_search_chip_until,
  has_attachments: m.mailbox_search_chip_attachments,
};
const own = (table: Record<string, () => string>, key: string) =>
  Object.prototype.hasOwnProperty.call(table, key) ? table[key]!() : undefined;

const day = (v?: number) => {
  if (v === undefined) return "";
  if (v === 0) {
    const epoch = new Date(0);
    return `${String(epoch.getFullYear()).padStart(4, "0")}-${String(epoch.getMonth() + 1).padStart(2, "0")}-${String(epoch.getDate()).padStart(2, "0")}`;
  }
  return localIsoDate(v);
};
export function SearchBar({ controller }: { controller: MailboxController }) {
  useUiLocale();
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
      setError(m.mailbox_search_invalid_range());
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
          aria-label={m.mailbox_search_label()}
          placeholder={m.mailbox_search_placeholder()}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <kbd>⌘K</kbd>
        <button
          className="text-button"
          type="submit"
          disabled={s.snapshot.phase !== "online"}
        >
          {m.mailbox_search_submit()}
        </button>
        <button
          className="text-button"
          type="button"
          aria-expanded={advanced}
          onClick={() => setAdvanced(!advanced)}
        >
          {m.mailbox_search_filters()}
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
            {m.mailbox_search_exit()}
          </button>
        )}
      </form>
      {advanced && (
        <div className="mail-search-filters">
          <label>
            {m.mailbox_search_field_account()}
            <Choice
              value={account}
              onValueChange={(value) => {
                setAccount(value);
                setFolder("");
              }}
            >
              <option value="">{m.mailbox_search_all_accounts()}</option>
              {s.snapshot.accounts.map((a) => (
                <option key={a.account} value={a.account}>
                  {controller.label(a.account)}
                </option>
              ))}
            </Choice>
          </label>
          <label>
            {m.mailbox_search_field_folder()}
            <Choice
              value={folder}
              disabled={!account}
              onValueChange={setFolder}
            >
              <option value="">{m.mailbox_search_all_folders()}</option>
              {controller.foldersOf(account).map((f) => (
                <option key={f.name} value={f.name}>
                  {folderLabel(f)}
                  {f.present ? "" : m.mailbox_search_folder_history()}
                </option>
              ))}
            </Choice>
          </label>
          <label>
            {m.mailbox_search_field_from()}
            <input
              value={from}
              onChange={(e) => setFrom(e.target.value)}
              placeholder={m.mailbox_search_from_placeholder()}
            />
          </label>
          <label>
            {m.mailbox_search_field_since()}
            <input
              type="date"
              value={since}
              onChange={(e) => setSince(e.target.value)}
            />
          </label>
          <label>
            {m.mailbox_search_field_until()}
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
            {m.mailbox_search_has_attachments()}
          </label>
          <button
            className="primary-button"
            disabled={s.snapshot.phase !== "online"}
            onClick={submit}
          >
            {m.mailbox_search_apply()}
          </button>
        </div>
      )}
      {active && (
        <div className="search-coverage">
          <span>{m.mailbox_search_local_only()}</span>
          {controller.serverSearchSupported && s.remote.status === "idle" && (
            <button
              type="button"
              className="text-button"
              disabled={!controller.canSearchServer}
              title={
                s.scope.kind === "search" && !s.scope.query.query?.trim()
                  ? m.mailbox_remote_need_query()
                  : m.mailbox_search_server_title()
              }
              onClick={() => void controller.searchServer()}
            >
              {m.mailbox_remote_search()}
            </button>
          )}
          {s.remote.status === "loading" && <span role="status">{m.mailbox_search_server_loading()}</span>}
          {s.index && !s.index.done && (
            <span role="status">
              {plural(s.index.pending, {
                one: m.mailbox_search_indexing_one,
                other: m.mailbox_search_indexing_other,
              })}
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
                aria-label={m.mailbox_search_chip_remove({ field: own(FILTER_FIELD, key) ?? key })}
                onClick={() => remove(key as keyof MailboxQuery)}
              >
                {own(FILTER_CHIP, key)}
                {key === "has_attachments"
                  ? ""
                  : m.mailbox_search_chip_value({
                      value:
                        key === "account"
                          ? controller.label(String(value))
                          : key === "since" || key === "until"
                            ? day(Number(value))
                            : String(value),
                    })}{" "}
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
