export const UI_LOCALE = 'zh-CN';

/** JavaScript `Date` 可精确表示的正向 Unix 秒上限（`8.64e15` 毫秒）。 */
export const MAX_SAFE_UNIX_SECS = 8_640_000_000_000;

const FORTY_EIGHT_HOURS_MS = 48 * 60 * 60 * 1000;

const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

interface Formatters {
  tz: string;
  time: Intl.DateTimeFormat;
  monthDay: Intl.DateTimeFormat;
  date: Intl.DateTimeFormat;
  full: Intl.DateTimeFormat;
}

let cachedFormatters: Formatters | null = null;

/**
 * 按当前系统时区惰性缓存 `Intl.DateTimeFormat`。
 * 绝不向选项传入固定 `timeZone`，以始终采用操作系统本地时区；当窗口最小化期间系统时区切换时，
 * `resolvedOptions().timeZone` 变化会自动触发重建，避免沿用旧时区实例。
 */
function formatters(): Formatters {
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone ?? '';
  if (!cachedFormatters || cachedFormatters.tz !== tz) {
    cachedFormatters = {
      tz,
      time: new Intl.DateTimeFormat(UI_LOCALE, { hour: '2-digit', minute: '2-digit' }),
      monthDay: new Intl.DateTimeFormat(UI_LOCALE, { month: 'numeric', day: 'numeric' }),
      date: new Intl.DateTimeFormat(UI_LOCALE, { year: 'numeric', month: 'numeric', day: 'numeric' }),
      full: new Intl.DateTimeFormat(UI_LOCALE, {
        year: 'numeric',
        month: 'long',
        day: 'numeric',
        weekday: 'short',
        hour: '2-digit',
        minute: '2-digit',
        timeZoneName: 'short',
      }),
    };
  }
  return cachedFormatters;
}

/**
 * 安全地将正数 Unix 秒转换为 `Date`。
 * `<= 0`（如服务端搜索缺失日期时的兜底 `0`）、非有限值或超出 `Date` 上限时一律返回 `null`，
 * 避免西半球出现 `1969/12/31` 或 `Intl` / `toISOString()` 抛出 `RangeError`。
 */
export function toDate(seconds: number | null | undefined): Date | null {
  if (
    typeof seconds !== 'number' ||
    !Number.isFinite(seconds) ||
    seconds <= 0 ||
    seconds > MAX_SAFE_UNIX_SECS
  ) {
    return null;
  }
  const date = new Date(seconds * 1000);
  return Number.isNaN(date.getTime()) ? null : date;
}

/** 列表用：今天只给时刻，昨天给「昨天 HH:MM」，今年给月日，更早给年月日；无效时间返回 `"—"`。 */
export function shortDate(seconds: number | null | undefined, now = new Date()): string {
  const date = toDate(seconds);
  if (!date) return '—';
  const f = formatters();
  if (sameDay(date, now)) return f.time.format(date);
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
  if (sameDay(date, yesterday)) return `昨天 ${f.time.format(date)}`;
  if (date.getFullYear() === now.getFullYear()) return f.monthDay.format(date);
  return f.date.format(date);
}

export function fullDate(seconds: number | null | undefined): string {
  const date = toDate(seconds);
  return date ? formatters().full.format(date) : '—';
}

export function isoDateTime(seconds: number | null | undefined): string | undefined {
  return toDate(seconds)?.toISOString();
}

export function dateOnly(seconds: number | null | undefined): string {
  const date = toDate(seconds);
  return date ? formatters().date.format(date) : '—';
}

export function clockTime(seconds: number | null | undefined): string {
  const date = toDate(seconds);
  return date ? formatters().time.format(date) : '';
}

/** 本地日历 `YYYY-MM-DD`（用于 `<input type="date">` 与搜索筛选回显）；无效时返回 `""`。 */
export function localIsoDate(seconds: number | null | undefined): string {
  const date = toDate(seconds);
  if (!date) return '';
  const y = String(date.getFullYear()).padStart(4, '0');
  const m = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/**
 * 一次性身份到期文案：
 * - 无效或缺失返回 `""`；
 * - 当天/次日/前一日或剩余正时间 `< 48` 小时时附带具体时刻，避免到期当天只显示日期而与「已过期」并列引发歧义。
 */
export function expiryLabel(expiresAtSeconds: number | null | undefined, now = new Date()): string {
  const date = toDate(expiresAtSeconds);
  if (!date) return '';
  const f = formatters();
  const hm = f.time.format(date);
  if (sameDay(date, now)) return `今天 ${hm} 到期`;
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  if (sameDay(date, tomorrow)) return `明天 ${hm} 到期`;
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
  if (sameDay(date, yesterday)) return `昨天 ${hm} 到期`;
  const remainingMs = date.getTime() - now.getTime();
  if (remainingMs > 0 && remainingMs < FORTY_EIGHT_HOURS_MS) {
    return `${f.date.format(date)} ${hm} 到期`;
  }
  return `${f.date.format(date)} 到期`;
}

/** `"显示名 <addr>"` → 显示名；只有地址时给地址 local part。 */
export function displayName(address: string): string {
  const match = /^\s*"?([^"<]*?)"?\s*<([^>]+)>\s*$/.exec(address);
  if (match) return match[1].trim() || match[2];
  return address.trim();
}

export function addressOnly(address: string): string {
  const match = /<([^>]+)>/.exec(address);
  return (match ? match[1] : address).trim();
}

export function initials(address: string): string {
  const name = displayName(address);
  if (!name) return '?';
  const cjk = /[㐀-鿿]/.test(name);
  return cjk ? name.slice(0, 1) : name.slice(0, 2).toUpperCase();
}

export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
