import { m } from "@featherpost/i18n/messages";

import type { MailboxQuery, Scope } from "./mailbox/types";

export type DatePreset = "any" | "7d" | "30d" | "90d" | "custom";

export type SearchScopeTier = "folder" | "account" | "group" | "all";

/**
 * Frontend-only display metadata attached to `Scope` (search variant).
 * NEVER emitted on `MailboxQuery` or sent over IPC in `queryOf(scope)`.
 */
export interface SearchFilterMeta {
  datePreset?: DatePreset;
  /** Local calendar date string `YYYY-MM-DD` for custom start bound. */
  startDate?: string;
  /** Local calendar date string `YYYY-MM-DD` for custom end bound. */
  endDate?: string;
}

export interface SearchFilterDraft {
  from: string;
  to: string;
  subject: string;
  unreadOnly: boolean;
  flaggedOnly: boolean;
  hasAttachments: boolean;
  datePreset: DatePreset;
  startDate: string;
  endDate: string;
  scope: SearchScopeTier;
}

export interface SearchScopeContext {
  account?: string;
  accounts?: readonly string[];
  folderName?: string;
}

export type FilterChipId =
  | "from"
  | "to"
  | "subject"
  | "seen"
  | "flagged"
  | "has_attachments"
  | "date";

export interface FilterChip {
  id: FilterChipId;
  label: string;
}

export interface FilterDraftValidation {
  valid: boolean;
  error: string | null;
  invalidField?: "from" | "to" | "subject" | "startDate" | "endDate";
}

/** Matches backend `crate::search::MAX_QUERY_BYTES`. */
export const MAX_FILTER_BYTES = 512;

/** Maximum Unix timestamp accepted by JS safe integers and Rust `i64::MAX`. */
export const MAX_UNIX_SECONDS = Number.MAX_SAFE_INTEGER;

const LOCAL_DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const PRESET_DAYS: Readonly<Record<Exclude<DatePreset, "any" | "custom">, number>> = {
  "7d": 7,
  "30d": 30,
  "90d": 90,
};

function utf8ByteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * Parses a local `YYYY-MM-DD` string strictly, rejecting overflowed calendar
 * dates like `2026-02-31`.
 */
export function parseLocalDateParts(
  value: string,
): { year: number; month: number; day: number } | null {
  const match = LOCAL_DATE_RE.exec(value.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1 || day > 31) return null;
  const probe = new Date(year, month - 1, day, 0, 0, 0, 0);
  if (
    Number.isNaN(probe.getTime()) ||
    probe.getFullYear() !== year ||
    probe.getMonth() !== month - 1 ||
    probe.getDate() !== day
  ) {
    return null;
  }
  return { year, month, day };
}

/**
 * Converts a local `YYYY-MM-DD` calendar date to local `00:00:00` Unix seconds.
 * Returns `null` if malformed or outside `[0, MAX_UNIX_SECONDS]`.
 */
export function localDayStartUnix(dateStr: string): number | null {
  const parts = parseLocalDateParts(dateStr);
  if (!parts) return null;
  const start = new Date(parts.year, parts.month - 1, parts.day, 0, 0, 0, 0);
  const secs = Math.floor(start.getTime() / 1000);
  if (!Number.isSafeInteger(secs) || secs < 0 || secs > MAX_UNIX_SECONDS) {
    return null;
  }
  return secs;
}

/**
 * Converts a local `YYYY-MM-DD` calendar date to DST-safe inclusive end-of-day
 * Unix seconds: **local next day `00:00:00` minus 1 second**.
 * Cleanly handles 23h spring-forward and 25h fall-back DST days and aligns
 * inclusively with SQL `sort_at <= until`.
 */
export function localDayEndUnix(dateStr: string): number | null {
  const parts = parseLocalDateParts(dateStr);
  if (!parts) return null;
  const nextDay = new Date(parts.year, parts.month - 1, parts.day + 1, 0, 0, 0, 0);
  const secs = Math.floor(nextDay.getTime() / 1000) - 1;
  if (!Number.isSafeInteger(secs) || secs < 0 || secs > MAX_UNIX_SECONDS) {
    return null;
  }
  return secs;
}

/**
 * Formats a Unix seconds timestamp into a local `YYYY-MM-DD` calendar date string.
 */
export function formatLocalDateFromUnix(secs: number): string {
  const d = new Date(secs * 1000);
  const year = String(d.getFullYear()).padStart(4, "0");
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/**
 * Resolves a relative preset (`7d` / `30d` / `90d`) to local midnight `days` ago.
 * Called once on Apply and reused across pagination and refreshes.
 */
export function resolveDatePresetSince(
  preset: Exclude<DatePreset, "any" | "custom">,
  now: Date = new Date(),
): number {
  const days = PRESET_DAYS[preset];
  const sinceDate = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate() - days,
    0,
    0,
    0,
    0,
  );
  const secs = Math.floor(sinceDate.getTime() / 1000);
  return Math.max(0, Math.min(MAX_UNIX_SECONDS, secs));
}

/**
 * Returns `true` when any structured search filter is active.
 * Routing/display fields (`search`, `account`, `accounts`, `folder`, `role`)
 * and free-text `query` are excluded.
 *
 * Also drives the P0 server-search guard: `canSearchServer` is only allowed
 * when `query` is non-empty AND `hasStructuredFilters(query)` is `false`.
 */
export function hasStructuredFilters(
  query: MailboxQuery | null | undefined,
): boolean {
  if (!query) return false;
  return Boolean(
    query.from?.trim() ||
      query.to?.trim() ||
      query.subject?.trim() ||
      query.seen !== undefined ||
      query.flagged !== undefined ||
      query.has_attachments !== undefined ||
      query.since !== undefined ||
      query.until !== undefined,
  );
}

/**
 * Returns `true` when the search query has a non-empty keyword OR any explicit
 * structured filter (including `seen: false`). Routing/display fields
 * (`search`, `account`, `accounts`, `folder`, `role`) do not count.
 */
export function hasActiveSearch(
  query: MailboxQuery | null | undefined,
): boolean {
  if (!query) return false;
  return Boolean(query.query?.trim() || hasStructuredFilters(query));
}

/**
 * Returns the canonical search query shape (trimmed strings, omitted undefined
 * fields, stable key ordering). Excludes frontend-only `Scope.meta`.
 */
export function canonicalSearchQuery(query: MailboxQuery): MailboxQuery {
  const out: MailboxQuery = { search: true };
  if (query.account) out.account = query.account;
  if (query.accounts) out.accounts = [...query.accounts];
  if (query.folder) out.folder = query.folder;
  const keyword = query.query?.trim();
  if (keyword) out.query = keyword;
  const from = query.from?.trim();
  if (from) out.from = from;
  const to = query.to?.trim();
  if (to) out.to = to;
  const subject = query.subject?.trim();
  if (subject) out.subject = subject;
  if (query.seen !== undefined) out.seen = query.seen;
  if (query.flagged !== undefined) out.flagged = query.flagged;
  if (query.has_attachments !== undefined) out.has_attachments = query.has_attachments;
  if (query.since !== undefined) out.since = query.since;
  if (query.until !== undefined) out.until = query.until;
  return out;
}

/**
 * Canonical cache/remount key for a `Scope`. For search scopes, derives the key
 * solely from `canonicalSearchQuery(scope.query)` — excluding `Scope.meta` so
 * display metadata changes never split controller caches or remount lists.
 */
export function canonicalScopeKey(scope: Scope): string {
  if (scope.kind === "search") {
    return JSON.stringify({
      kind: "search",
      query: canonicalSearchQuery(scope.query),
    });
  }
  return JSON.stringify(scope);
}

/**
 * Available scope tiers for Scheme A 3-tier sync:
 * - Standard folder: `["folder", "account", "all"]`
 * - Single account without folder: `["account", "all"]`
 * - Unified account group: `["group", "all"]`
 * - Unified smart view: `["all"]`
 */
export function availableScopeTiers(
  context: SearchScopeContext,
): readonly SearchScopeTier[] {
  if (context.account && context.folderName) {
    return ["folder", "account", "all"];
  }
  if (context.account) {
    return ["account", "all"];
  }
  if (context.accounts) {
    return ["group", "all"];
  }
  return ["all"];
}

export function defaultScopeTier(context: SearchScopeContext): SearchScopeTier {
  return availableScopeTiers(context)[0] ?? "all";
}

export function scopeTierFromQuery(
  query: MailboxQuery,
  context: SearchScopeContext,
): SearchScopeTier {
  const tiers = availableScopeTiers(context);
  if (query.account && query.folder && tiers.includes("folder")) {
    return "folder";
  }
  if (query.account && tiers.includes("account")) {
    return "account";
  }
  if (query.accounts && tiers.includes("group")) {
    return "group";
  }
  return "all";
}

export function scopeFieldsForTier(
  tier: SearchScopeTier,
  context: SearchScopeContext,
): Pick<MailboxQuery, "account" | "accounts" | "folder"> {
  if (tier === "folder" && context.account && context.folderName) {
    return { account: context.account, folder: context.folderName };
  }
  if (tier === "account" && context.account) {
    return { account: context.account };
  }
  if (tier === "group" && context.accounts) {
    return { accounts: [...context.accounts] };
  }
  return {};
}

export function emptyFilterDraft(scope: SearchScopeTier = "all"): SearchFilterDraft {
  return {
    from: "",
    to: "",
    subject: "",
    unreadOnly: false,
    flaggedOnly: false,
    hasAttachments: false,
    datePreset: "any",
    startDate: "",
    endDate: "",
    scope,
  };
}

export function validateSearchFilterDraft(
  draft: SearchFilterDraft,
): FilterDraftValidation {
  for (const field of ["from", "to", "subject"] as const) {
    const trimmed = draft[field].trim();
    if (trimmed && utf8ByteLength(trimmed) > MAX_FILTER_BYTES) {
      return {
        valid: false,
        error: m.studio_search_filter_error_too_long(),
        invalidField: field,
      };
    }
  }
  if (draft.datePreset === "custom") {
    const startTrimmed = draft.startDate.trim();
    const endTrimmed = draft.endDate.trim();
    let startUnix: number | null = null;
    let endUnix: number | null = null;
    if (startTrimmed) {
      startUnix = localDayStartUnix(startTrimmed);
      if (startUnix === null) {
        return {
          valid: false,
          error: m.studio_search_filter_error_date_invalid(),
          invalidField: "startDate",
        };
      }
    }
    if (endTrimmed) {
      endUnix = localDayEndUnix(endTrimmed);
      if (endUnix === null) {
        return {
          valid: false,
          error: m.studio_search_filter_error_date_invalid(),
          invalidField: "endDate",
        };
      }
    }
    if (startUnix !== null && endUnix !== null && startUnix > endUnix) {
      return {
        valid: false,
        error: m.studio_search_filter_error_date_range(),
        invalidField: "startDate",
      };
    }
  }
  return { valid: true, error: null };
}

export function fromMailboxQuery(
  query: MailboxQuery | null | undefined,
  meta: SearchFilterMeta | undefined,
  scopeTier: SearchScopeTier,
): SearchFilterDraft {
  if (!query) return emptyFilterDraft(scopeTier);
  let datePreset: DatePreset = "any";
  let startDate = "";
  let endDate = "";
  const hasDateBounds = query.since !== undefined || query.until !== undefined;
  if (hasDateBounds) {
    if (
      meta?.datePreset === "7d" ||
      meta?.datePreset === "30d" ||
      meta?.datePreset === "90d"
    ) {
      datePreset = meta.datePreset;
    } else {
      datePreset = "custom";
      startDate =
        meta?.startDate ??
        (query.since !== undefined ? formatLocalDateFromUnix(query.since) : "");
      endDate =
        meta?.endDate ??
        (query.until !== undefined ? formatLocalDateFromUnix(query.until) : "");
    }
  }
  return {
    from: query.from?.trim() ?? "",
    to: query.to?.trim() ?? "",
    subject: query.subject?.trim() ?? "",
    unreadOnly: query.seen === false,
    flaggedOnly: query.flagged === true,
    hasAttachments: query.has_attachments === true,
    datePreset,
    startDate,
    endDate,
    scope: scopeTier,
  };
}

export function toMailboxQuery(
  draft: SearchFilterDraft,
  keyword: string,
  context: SearchScopeContext,
  now: Date = new Date(),
): { query: MailboxQuery; meta?: SearchFilterMeta } {
  const scopeFields = scopeFieldsForTier(draft.scope, context);
  const query: MailboxQuery = { ...scopeFields };

  const cleanKeyword = keyword.trim();
  if (cleanKeyword) query.query = cleanKeyword;
  const from = draft.from.trim();
  if (from) query.from = from;
  const to = draft.to.trim();
  if (to) query.to = to;
  const subject = draft.subject.trim();
  if (subject) query.subject = subject;
  if (draft.unreadOnly) query.seen = false;
  if (draft.flaggedOnly) query.flagged = true;
  if (draft.hasAttachments) query.has_attachments = true;

  let meta: SearchFilterMeta | undefined;
  if (
    draft.datePreset === "7d" ||
    draft.datePreset === "30d" ||
    draft.datePreset === "90d"
  ) {
    query.since = resolveDatePresetSince(draft.datePreset, now);
    meta = { datePreset: draft.datePreset };
  } else if (draft.datePreset === "custom") {
    const startTrimmed = draft.startDate.trim();
    const endTrimmed = draft.endDate.trim();
    const since = startTrimmed ? localDayStartUnix(startTrimmed) : null;
    const until = endTrimmed ? localDayEndUnix(endTrimmed) : null;
    if (since !== null) query.since = since;
    if (until !== null) query.until = until;
    if (since !== null || until !== null) {
      meta = {
        datePreset: "custom",
        ...(since !== null ? { startDate: startTrimmed } : {}),
        ...(until !== null ? { endDate: endTrimmed } : {}),
      };
    }
  }

  return meta ? { query, meta } : { query };
}

export function activeFilterChips(
  query: MailboxQuery | null | undefined,
  meta?: SearchFilterMeta,
): FilterChip[] {
  if (!query) return [];
  const chips: FilterChip[] = [];

  const from = query.from?.trim();
  if (from) {
    chips.push({ id: "from", label: m.studio_search_chip_from({ value: from }) });
  }
  const to = query.to?.trim();
  if (to) {
    chips.push({ id: "to", label: m.studio_search_chip_to({ value: to }) });
  }
  const subject = query.subject?.trim();
  if (subject) {
    chips.push({
      id: "subject",
      label: m.studio_search_chip_subject({ value: subject }),
    });
  }
  if (query.seen === false) {
    chips.push({ id: "seen", label: m.studio_search_chip_unread() });
  } else if (query.seen === true) {
    chips.push({ id: "seen", label: m.studio_search_chip_read() });
  }
  if (query.flagged === true) {
    chips.push({ id: "flagged", label: m.studio_search_chip_flagged() });
  }
  if (query.has_attachments === true) {
    chips.push({ id: "has_attachments", label: m.studio_search_chip_attachments() });
  }
  if (query.since !== undefined || query.until !== undefined) {
    let dateLabel: string;
    if (meta?.datePreset === "7d") {
      dateLabel = m.studio_search_chip_date_7d();
    } else if (meta?.datePreset === "30d") {
      dateLabel = m.studio_search_chip_date_30d();
    } else if (meta?.datePreset === "90d") {
      dateLabel = m.studio_search_chip_date_90d();
    } else {
      const start =
        meta?.startDate ??
        (query.since !== undefined ? formatLocalDateFromUnix(query.since) : undefined);
      const end =
        meta?.endDate ??
        (query.until !== undefined ? formatLocalDateFromUnix(query.until) : undefined);
      if (start && end) {
        dateLabel = m.studio_search_chip_date_range({ start, end });
      } else if (start) {
        dateLabel = m.studio_search_chip_date_since({ start });
      } else {
        dateLabel = m.studio_search_chip_date_until({ end: end ?? "" });
      }
    }
    chips.push({ id: "date", label: dateLabel });
  }
  return chips;
}

export function activeFilterCount(
  query: MailboxQuery | null | undefined,
): number {
  if (!query) return 0;
  let count = 0;
  if (query.from?.trim()) count++;
  if (query.to?.trim()) count++;
  if (query.subject?.trim()) count++;
  if (query.seen !== undefined) count++;
  if (query.flagged !== undefined) count++;
  if (query.has_attachments !== undefined) count++;
  if (query.since !== undefined || query.until !== undefined) count++;
  return count;
}

export function removeFilterChip(
  query: MailboxQuery,
  meta: SearchFilterMeta | undefined,
  chipId: FilterChipId,
): { query: MailboxQuery; meta?: SearchFilterMeta } {
  const next: MailboxQuery = { ...query };
  let nextMeta: SearchFilterMeta | undefined = meta ? { ...meta } : undefined;
  switch (chipId) {
    case "from":
      delete next.from;
      break;
    case "to":
      delete next.to;
      break;
    case "subject":
      delete next.subject;
      break;
    case "seen":
      delete next.seen;
      break;
    case "flagged":
      delete next.flagged;
      break;
    case "has_attachments":
      delete next.has_attachments;
      break;
    case "date":
      delete next.since;
      delete next.until;
      nextMeta = undefined;
      break;
  }
  return nextMeta ? { query: next, meta: nextMeta } : { query: next };
}

export function clearStructuredFilters(query: MailboxQuery): MailboxQuery {
  const next: MailboxQuery = {};
  if (query.account) next.account = query.account;
  if (query.accounts) next.accounts = [...query.accounts];
  if (query.folder) next.folder = query.folder;
  const keyword = query.query?.trim();
  if (keyword) next.query = keyword;
  return next;
}
