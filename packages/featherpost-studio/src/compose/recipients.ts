import type { UiContactSuggestion } from "../bridge/types";

export const RECIPIENT_FIELDS = ["to", "cc", "bcc"] as const;
export type RecipientFieldName = (typeof RECIPIENT_FIELDS)[number];

/** Mirrors `crates/maild/src/drafts.rs` (`MAX_DRAFT_RECIPIENTS` / `MAX_RECIPIENTS`). */
export const MAX_RECIPIENTS_TOTAL = 100;
/** Mirrors `crates/maild-client/src/lib.rs` (`MAX_MAILBOX_BYTES`) and `drafts.rs` (`MAX_DRAFT_ADDRESS_BYTES`). */
export const MAX_MAILBOX_BYTES = 320;
/** Mirrors `crates/maild/src/contacts.rs` (`MAX_DISPLAY_NAME_BYTES`). */
export const MAX_DISPLAY_NAME_BYTES = 128;
/** Mirrors `crates/maild/src/contacts.rs` (`MAX_CONTACT_QUERY_BYTES`). */
export const MAX_CONTACT_QUERY_BYTES = 256;

export interface ParsedRecipient {
  /** Bare canonical ASCII `addr-spec` when valid, otherwise trimmed raw input. */
  value: string;
  /** Optional friendly plain-text display name (frontend memory only, never sent on wire). */
  displayName?: string;
  /** True iff `value` satisfies `maild_client::validate_send_field(SendField::To, value)`. */
  valid: boolean;
}

export interface RecipientFieldState {
  chips: string[];
  pending: string;
  editingIndex: number | null;
  editingOriginal: string | null;
}

export type RecipientMap = Record<RecipientFieldName, RecipientFieldState>;

export type RecipientLimitError = "too_many" | "too_long" | "control_char";

export interface FallbackContactRow {
  account: string;
  sender: string;
  recipient: string;
  cc?: string;
  sort_at?: number;
}

export interface FallbackSenderInfo {
  account: string;
  address: string;
  label?: string;
}

const encoder = new TextEncoder();

export function utf8ByteLength(value: string): number {
  return encoder.encode(value).length;
}

export function hasControlChar(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
}

function isLocalByteValid(code: number): boolean {
  if (
    (code >= 0x30 && code <= 0x39) || // 0-9
    (code >= 0x41 && code <= 0x5a) || // A-Z
    (code >= 0x61 && code <= 0x7a) // a-z
  ) {
    return true;
  }
  switch (code) {
    case 0x21: // !
    case 0x23: // #
    case 0x24: // $
    case 0x25: // %
    case 0x26: // &
    case 0x27: // '
    case 0x2a: // *
    case 0x2b: // +
    case 0x2d: // -
    case 0x2f: // /
    case 0x3d: // =
    case 0x3f: // ?
    case 0x5e: // ^
    case 0x5f: // _
    case 0x60: // `
    case 0x7b: // {
    case 0x7c: // |
    case 0x7d: // }
    case 0x7e: // ~
    case 0x2e: // .
      return true;
    default:
      return false;
  }
}

function isDomainLabelByteValid(code: number): boolean {
  return (
    (code >= 0x30 && code <= 0x39) ||
    (code >= 0x41 && code <= 0x5a) ||
    (code >= 0x61 && code <= 0x7a) ||
    code === 0x2d // -
  );
}

/**
 * Exact TypeScript equivalent of `maild_client::validate_send_field(SendField::To, value)`.
 */
export function isValidAddrSpec(value: string): boolean {
  if (!value || utf8ByteLength(value) > MAX_MAILBOX_BYTES || hasControlChar(value)) {
    return false;
  }
  const at = value.indexOf("@");
  if (at <= 0 || at === value.length - 1) return false;
  const local = value.slice(0, at);
  const domain = value.slice(at + 1);

  if (
    local.startsWith(".") ||
    local.endsWith(".") ||
    local.includes("..")
  ) {
    return false;
  }
  for (let i = 0; i < local.length; i++) {
    const code = local.charCodeAt(i);
    if (code > 0x7f || !isLocalByteValid(code)) return false;
  }

  if (!domain || domain.length > 253) return false;
  const labels = domain.split(".");
  for (const label of labels) {
    if (
      !label ||
      label.length > 63 ||
      label.startsWith("-") ||
      label.endsWith("-")
    ) {
      return false;
    }
    for (let i = 0; i < label.length; i++) {
      const code = label.charCodeAt(i);
      if (code > 0x7f || !isDomainLabelByteValid(code)) return false;
    }
  }
  return true;
}

/**
 * RFC 5321 canonicalization: preserves local-part original casing, lowercases ASCII domain.
 */
export function canonicalAddrSpec(addr: string): string | null {
  const trimmed = addr.trim();
  const unwrapped =
    trimmed.startsWith("<") && trimmed.endsWith(">")
      ? trimmed.slice(1, -1).trim()
      : trimmed;
  if (!isValidAddrSpec(unwrapped)) return null;
  const at = unwrapped.indexOf("@");
  return `${unwrapped.slice(0, at)}@${unwrapped.slice(at + 1).toLowerCase()}`;
}

/**
 * Deduplication key within a single recipient field:
 * preserves local-part original casing while deduplicating domain case-insensitively.
 */
export function recipientDedupKey(value: string): string {
  return canonicalAddrSpec(value) ?? value.trim();
}

export function cleanDisplayName(
  raw: string | null | undefined,
  canonicalAddress?: string,
): string | undefined {
  if (!raw) return undefined;
  let text = raw.trim();
  // Unwrap outer matching double quotes and unescape \" / \\
  if (text.length >= 2 && text.startsWith('"') && text.endsWith('"')) {
    let unescaped = "";
    for (let i = 1; i < text.length - 1; i++) {
      const ch = text[i]!;
      if (ch === "\\" && i + 1 < text.length - 1) {
        unescaped += text[i + 1]!;
        i++;
      } else {
        unescaped += ch;
      }
    }
    text = unescaped;
  }
  // Strip control characters, collapse whitespace, cap to MAX_DISPLAY_NAME_BYTES UTF-8 bytes
  let out = "";
  let bytes = 0;
  let pendingSpace = false;
  for (const ch of text) {
    const code = ch.codePointAt(0)!;
    if (code < 0x20 || code === 0x7f) continue;
    if (/\s/.test(ch)) {
      pendingSpace = out.length > 0;
      continue;
    }
    const chBytes = utf8ByteLength(ch);
    if (pendingSpace) {
      if (bytes + 1 + chBytes > MAX_DISPLAY_NAME_BYTES) break;
      out += " ";
      bytes += 1;
      pendingSpace = false;
    } else if (bytes + chBytes > MAX_DISPLAY_NAME_BYTES) {
      break;
    }
    out += ch;
    bytes += chBytes;
  }
  const finalName = out.trim().replace(/^"+|"+$/g, "").trim();
  if (!finalName) return undefined;
  if (
    canonicalAddress &&
    finalName.toLowerCase() === canonicalAddress.toLowerCase()
  ) {
    return undefined;
  }
  return finalName;
}

const INLINE_DELIMITERS = new Set([",", ";", "\uFF0C", "\uFF1B"]);

function isLineOrTabBreak(ch: string): boolean {
  return ch === "\n" || ch === "\r" || ch === "\t";
}

/**
 * Quote-, escape-, and angle-bracket-aware recipient delimiter splitter.
 * Splits on `,`, `;`, `，`, `；` when outside quotes and `<...>`, and on `\r`/`\n`/`\t`.
 */
export function splitRecipientTokens(input: string): string[] {
  const parts: string[] = [];
  let inQuotes = false;
  let escaped = false;
  let angleDepth = 0;
  let start = 0;

  for (let i = 0; i < input.length; i++) {
    const ch = input[i]!;
    if (isLineOrTabBreak(ch)) {
      const seg = input.slice(start, i).trim();
      if (seg) parts.push(seg);
      start = i + 1;
      inQuotes = false;
      escaped = false;
      angleDepth = 0;
      continue;
    }
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inQuotes) {
      if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inQuotes = false;
      }
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
      continue;
    }
    if (ch === "<") {
      angleDepth++;
      continue;
    }
    if (ch === ">") {
      if (angleDepth > 0) angleDepth--;
      continue;
    }
    if (angleDepth === 0 && INLINE_DELIMITERS.has(ch)) {
      const seg = input.slice(start, i).trim();
      if (seg) parts.push(seg);
      start = i + 1;
    }
  }

  const tail = input.slice(start).trim();
  if (tail) parts.push(tail);
  return parts;
}

/**
 * Returns true if `input` contains any delimiter outside quotes and `<...>`,
 * or any newline/tab separator.
 */
export function hasTopLevelDelimiter(input: string): boolean {
  let inQuotes = false;
  let escaped = false;
  let angleDepth = 0;
  for (let i = 0; i < input.length; i++) {
    const ch = input[i]!;
    if (isLineOrTabBreak(ch)) return true;
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inQuotes) {
      if (ch === "\\") escaped = true;
      else if (ch === '"') inQuotes = false;
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
    } else if (ch === "<") {
      angleDepth++;
    } else if (ch === ">") {
      if (angleDepth > 0) angleDepth--;
    } else if (angleDepth === 0 && INLINE_DELIMITERS.has(ch)) {
      return true;
    }
  }
  return false;
}

/**
 * Checks whether typing `key` at the end of `currentQuery` is a top-level delimiter
 * (not inside double quotes or angle brackets).
 */
export function isDelimiterKeyAtEnd(key: string, currentQuery: string): boolean {
  if (!INLINE_DELIMITERS.has(key)) return false;
  let inQuotes = false;
  let escaped = false;
  let angleDepth = 0;
  for (let i = 0; i < currentQuery.length; i++) {
    const ch = currentQuery[i]!;
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inQuotes) {
      if (ch === "\\") escaped = true;
      else if (ch === '"') inQuotes = false;
      continue;
    }
    if (ch === '"') inQuotes = true;
    else if (ch === "<") angleDepth++;
    else if (ch === ">" && angleDepth > 0) angleDepth--;
  }
  return !inQuotes && !escaped && angleDepth === 0;
}

/**
 * Splits a single token into `{ namePart, addrPart }` if it has top-level `<...>`.
 */
function extractAngleBracketParts(
  token: string,
): { namePart: string; addrPart: string } | null {
  let inQuotes = false;
  let escaped = false;
  let openIdx = -1;
  let closeIdx = -1;

  for (let i = 0; i < token.length; i++) {
    const ch = token[i]!;
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inQuotes) {
      if (ch === "\\") escaped = true;
      else if (ch === '"') inQuotes = false;
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
    } else if (ch === "<" && openIdx === -1) {
      openIdx = i;
    } else if (ch === ">" && openIdx !== -1 && closeIdx === -1) {
      closeIdx = i;
    }
  }

  if (openIdx === -1 || closeIdx === -1) return null;
  if (token.slice(closeIdx + 1).trim() !== "") return null;
  return {
    namePart: token.slice(0, openIdx).trim(),
    addrPart: token.slice(openIdx + 1, closeIdx).trim(),
  };
}

/**
 * Normalizes a single recipient token:
 * - `"Alice <alice@X.com>"` -> `{ value: "alice@x.com", displayName: "Alice", valid: true }`
 * - `"Alice@X.com"` -> `{ value: "Alice@x.com", valid: true }`
 * - `"alice@"` -> `{ value: "alice@", valid: false }`
 */
export function parseRecipientToken(raw: string): ParsedRecipient {
  const trimmed = raw.trim();
  const angled = extractAngleBracketParts(trimmed);
  if (angled) {
    const canonical = canonicalAddrSpec(angled.addrPart);
    if (canonical) {
      const displayName = cleanDisplayName(angled.namePart, canonical);
      return displayName
        ? { value: canonical, displayName, valid: true }
        : { value: canonical, valid: true };
    }
    return { value: trimmed, valid: false };
  }
  const canonical = canonicalAddrSpec(trimmed);
  if (canonical) {
    return { value: canonical, valid: true };
  }
  return { value: trimmed, valid: false };
}

/**
 * Splits and normalizes a recipient string into a deduplicated ordered list.
 */
export function parseRecipientList(input: string): ParsedRecipient[] {
  const tokens = splitRecipientTokens(input);
  const out: ParsedRecipient[] = [];
  const indexByKey = new Map<string, number>();

  for (const token of tokens) {
    const parsed = parseRecipientToken(token);
    if (!parsed.value) continue;
    const key = recipientDedupKey(parsed.value);
    const existingIdx = indexByKey.get(key);
    if (existingIdx !== undefined) {
      if (!out[existingIdx]!.displayName && parsed.displayName) {
        out[existingIdx] = { ...out[existingIdx]!, displayName: parsed.displayName };
      }
      continue;
    }
    indexByKey.set(key, out.length);
    out.push(parsed);
  }
  return out;
}

export function emptyRecipientFieldState(chips: readonly string[] = []): RecipientFieldState {
  const parsed = parseRecipientList(chips.join("\n"));
  return {
    chips: parsed.map((p) => p.value),
    pending: "",
    editingIndex: null,
    editingOriginal: null,
  };
}

export function createRecipientMap(draft: {
  to: readonly string[];
  cc: readonly string[];
  bcc: readonly string[];
}): { recipients: RecipientMap; names: Record<string, string> } {
  const names: Record<string, string> = {};
  const fromArray = (items: readonly string[]): RecipientFieldState => {
    const chips: string[] = [];
    const seen = new Set<string>();
    for (const raw of items) {
      const parsed = parseRecipientToken(raw);
      if (!parsed.value) continue;
      const key = recipientDedupKey(parsed.value);
      if (parsed.displayName && !names[key]) {
        names[key] = parsed.displayName;
      }
      if (seen.has(key)) continue;
      seen.add(key);
      chips.push(parsed.value);
    }
    return {
      chips,
      pending: "",
      editingIndex: null,
      editingOriginal: null,
    };
  };
  return {
    recipients: {
      to: fromArray(draft.to),
      cc: fromArray(draft.cc),
      bcc: fromArray(draft.bcc),
    },
    names,
  };
}

/**
 * Merges incoming parsed recipients into `existing` chips at `editingIndex` (in-place)
 * or at the end (`editingIndex === null`), preserving chip order and deduplicating
 * within the field (domain case-insensitively, preserving local-part casing).
 */
export function mergeRecipientTokens(
  existing: readonly string[],
  incoming: readonly ParsedRecipient[],
  editingIndex: number | null = null,
): { chips: string[]; names: Record<string, string> } {
  const names: Record<string, string> = {};
  for (const item of incoming) {
    if (item.displayName) {
      names[recipientDedupKey(item.value)] = item.displayName;
    }
  }

  if (
    editingIndex !== null &&
    editingIndex >= 0 &&
    editingIndex < existing.length
  ) {
    const before = existing.slice(0, editingIndex);
    const after = existing.slice(editingIndex + 1);
    const otherKeys = new Set<string>();
    for (const item of before) otherKeys.add(recipientDedupKey(item));
    for (const item of after) otherKeys.add(recipientDedupKey(item));

    const replacement: string[] = [];
    const localSeen = new Set<string>();
    for (const item of incoming) {
      if (!item.value) continue;
      const key = recipientDedupKey(item.value);
      if (otherKeys.has(key) || localSeen.has(key)) continue;
      localSeen.add(key);
      replacement.push(item.value);
    }
    return { chips: [...before, ...replacement, ...after], names };
  }

  const chips = [...existing];
  const seen = new Set(existing.map(recipientDedupKey));
  for (const item of incoming) {
    if (!item.value) continue;
    const key = recipientDedupKey(item.value);
    if (seen.has(key)) continue;
    seen.add(key);
    chips.push(item.value);
  }
  return { chips, names };
}

/**
 * Computes the effective ordered recipient list for a field, combining committed chips
 * and any uncommitted pending input (including in-place edit state).
 */
export function effectiveRecipientValues(field: RecipientFieldState): string[] {
  if (field.editingIndex !== null) {
    const incoming = parseRecipientList(field.pending);
    return mergeRecipientTokens(field.chips, incoming, field.editingIndex).chips;
  }
  if (!field.pending.trim()) {
    return [...field.chips];
  }
  const incoming = parseRecipientList(field.pending);
  return mergeRecipientTokens(field.chips, incoming, null).chips;
}

/**
 * Checks draft-save recipient hard limits (control chars, 320 bytes per item, 100 total across fields).
 * Never mutates or drops entries so the user can inspect and fix them.
 */
export function checkRecipientDraftLimits(
  values: Record<RecipientFieldName, readonly string[]>,
): Partial<Record<RecipientFieldName, RecipientLimitError>> {
  const errors: Partial<Record<RecipientFieldName, RecipientLimitError>> = {};
  let total = 0;

  for (const field of RECIPIENT_FIELDS) {
    const list = values[field];
    total += list.length;
    for (const item of list) {
      if (hasControlChar(item)) {
        errors[field] = "control_char";
        break;
      }
      if (utf8ByteLength(item) > MAX_MAILBOX_BYTES) {
        errors[field] = "too_long";
        break;
      }
    }
  }

  if (total > MAX_RECIPIENTS_TOTAL) {
    const targetField: RecipientFieldName =
      values.to.length > MAX_RECIPIENTS_TOTAL
        ? "to"
        : values.to.length + values.cc.length > MAX_RECIPIENTS_TOTAL
          ? "cc"
          : values.bcc.length > 0
            ? "bcc"
            : "to";
    if (!errors[targetField]) {
      errors[targetField] = "too_many";
    }
  }

  return errors;
}

/**
 * Extracts and ranks current-account in-memory contact suggestions for offline or
 * legacy-daemon fallback. Strictly scoped to `account` (never crosses accounts or drafts).
 */
export function buildFallbackContactSuggestions(
  account: string,
  rows: readonly FallbackContactRow[],
  senders: readonly FallbackSenderInfo[],
  query: string,
  limit = 8,
): UiContactSuggestion[] {
  if (!account) return [];
  const cap = Math.max(1, Math.min(20, limit));
  const selfKeys = new Set<string>();
  const selfEntries: Array<{ address: string; name?: string }> = [];

  for (const sender of senders) {
    if (sender.account !== account) continue;
    const canonical = canonicalAddrSpec(sender.address);
    if (!canonical) continue;
    selfKeys.add(canonical);
    const name = cleanDisplayName(sender.label, canonical);
    selfEntries.push({ address: canonical, name });
  }

  interface ScoredContact {
    address: string;
    name?: string;
    score: number;
    lastAt: number;
  }

  const byKey = new Map<string, ScoredContact>();
  const record = (rawHeader: string | undefined, weight: number, sortAt: number) => {
    if (!rawHeader) return;
    for (const item of parseRecipientList(rawHeader)) {
      if (!item.valid) continue;
      const key = item.value;
      const effectiveWeight = selfKeys.has(key) ? Math.max(1, Math.floor(weight / 4)) : weight;
      const prev = byKey.get(key);
      if (prev) {
        prev.score += effectiveWeight;
        if (sortAt > prev.lastAt) {
          prev.lastAt = sortAt;
          if (item.displayName) prev.name = item.displayName;
        } else if (!prev.name && item.displayName) {
          prev.name = item.displayName;
        }
      } else {
        byKey.set(key, {
          address: key,
          name: item.displayName,
          score: effectiveWeight,
          lastAt: sortAt,
        });
      }
    }
  };

  for (const row of rows) {
    if (row.account !== account) continue;
    const at = row.sort_at ?? 0;
    record(row.sender, 4, at);
    record(row.recipient, 3, at);
    record(row.cc, 2, at);
  }

  for (const self of selfEntries) {
    if (!byKey.has(self.address)) {
      byKey.set(self.address, {
        address: self.address,
        name: self.name,
        score: 1,
        lastAt: 0,
      });
    }
  }

  const terms = query
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean);

  const matched = [...byKey.values()].filter((entry) => {
    if (!terms.length) return true;
    const addrLower = entry.address.toLowerCase();
    const nameLower = (entry.name ?? "").toLowerCase();
    return terms.every((term) => addrLower.includes(term) || nameLower.includes(term));
  });

  matched.sort((a, b) => {
    if (b.score !== a.score) return b.score - a.score;
    if (b.lastAt !== a.lastAt) return b.lastAt - a.lastAt;
    return a.address.localeCompare(b.address);
  });

  return matched.slice(0, cap).map((entry) =>
    entry.name
      ? { address: entry.address, name: entry.name }
      : { address: entry.address },
  );
}
