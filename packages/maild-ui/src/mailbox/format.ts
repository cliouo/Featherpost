const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

const time = new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit' });
const monthDay = new Intl.DateTimeFormat('zh-CN', { month: 'numeric', day: 'numeric' });
const full = new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric', month: 'long', day: 'numeric', weekday: 'short', hour: '2-digit', minute: '2-digit', timeZoneName: 'short',
});

/** 列表用：今天只给时刻，今年给月日，更早给年月日。排序不看这个（用内核 sort_at）。 */
export function shortDate(seconds: number, now = new Date()): string {
  const date = new Date(seconds * 1000);
  if (sameDay(date, now)) return time.format(date);
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(date, yesterday)) return `昨天 ${time.format(date)}`;
  if (date.getFullYear() === now.getFullYear()) return monthDay.format(date);
  return `${date.getFullYear()}/${date.getMonth() + 1}/${date.getDate()}`;
}

export function fullDate(seconds: number): string {
  return full.format(new Date(seconds * 1000));
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
