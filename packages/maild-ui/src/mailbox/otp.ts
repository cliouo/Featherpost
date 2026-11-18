/**
 * 验证码提取（纯函数）：必须同时满足
 * 1. 文中出现验证码语境词（验证码 / 校验码 / verification / OTP / code …）；
 * 2. 候选是独立边界的 4～8 位数字，且离语境词足够近（优先语境词之后）。
 * 日期（20261117、2026-11-17）、长单号、手机号与分组号码、金额、链接里的数字一律不算。
 */
const CONTEXT =
  /验证码|校验码|动态码|动态密码|确认码|安全码|激活码|登录码|认证码|verification|verify|one[- ]?time|\botp\b|passcode|security code|log[- ]?in code|sign[- ]?in code|confirmation code|\bcodes?\b|\bpin\b/gi;
/** 「code」前面是这些词时不是验证码语境（邮编、优惠码等）。 */
const NOT_OTP = /(zip|postal|post|area|country|promo|promotion|coupon|discount|invite|invitation|referral|gift|source|error|status|tracking|邮编|区号|优惠|兑换|邀请)\s*$/i;
const UNIT_AFTER = /^\s*(年|月|日|元|块|美元|人民币|积分|分钟|秒|小时|天|次|件|笔|号|楼|室|%|usd|cny|rmb|mins?\b|minutes?\b|seconds?\b|hours?\b|days?\b)/i;
const SUBJECT_LIMIT = 300;
const BODY_LIMIT = 6000;
const AFTER_WINDOW = 80;
const BEFORE_WINDOW = 30;

interface Span {
  start: number;
  end: number;
}
interface Candidate extends Span {
  value: string;
}

function isDate(value: string) {
  return value.length === 8 && /^(19|20)\d{2}(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])$/.test(value);
}

function contexts(text: string): Span[] {
  const out: Span[] = [];
  for (const m of text.matchAll(CONTEXT)) {
    const start = m.index ?? 0;
    const word = m[0].toLowerCase();
    if ((word === "code" || word === "codes" || word === "pin") && NOT_OTP.test(text.slice(Math.max(0, start - 16), start))) continue;
    out.push({ start, end: start + m[0].length });
  }
  return out;
}

function candidates(text: string): Candidate[] {
  const out: Candidate[] = [];
  for (const m of text.matchAll(/(?<![A-Za-z0-9])\d{4,8}(?![A-Za-z0-9])/g)) {
    const value = m[0];
    const start = m.index ?? 0;
    const end = start + value.length;
    const before = text.slice(Math.max(0, start - 2), start);
    const after = text.slice(end, end + 2);
    // 分组号码 / 日期 / 时间 / 小数：数字经单个分隔符与另一段数字相连。
    if (/\d[-./:, \t]$/.test(before) || /^[-./:, \t]\d/.test(after)) continue;
    // 货币、电话前缀、编号符号。
    if (/[¥$€£#+＃]\s?$/.test(before)) continue;
    if (isDate(value)) continue;
    if (UNIT_AFTER.test(text.slice(end, end + 12))) continue;
    // 链接里的数字（追踪参数等）不算。
    const tokenStart = Math.max(text.lastIndexOf(" ", start), text.lastIndexOf("\n", start)) + 1;
    const token = text.slice(tokenStart, end);
    if (/:\/\/|^www\./i.test(token)) continue;
    out.push({ value, start, end });
  }
  return out;
}

export function detectOtpCode(subject: string, body: string): string | null {
  const head = (subject ?? "").slice(0, SUBJECT_LIMIT);
  const text = `${head}\n${(body ?? "").slice(0, BODY_LIMIT)}`;
  const words = contexts(text);
  if (!words.length) return null;
  const found = candidates(text);
  if (!found.length) return null;
  let best: Candidate | null = null;
  let bestScore = Infinity;
  for (const c of found)
    for (const w of words) {
      // 候选与语境词重叠不可能（一个是数字一个是词），只比较前后距离。
      let score: number;
      if (c.start >= w.end) {
        const gap = c.start - w.end;
        if (gap > AFTER_WINDOW) continue;
        score = gap;
      } else if (c.end <= w.start) {
        const gap = w.start - c.end;
        if (gap > BEFORE_WINDOW) continue;
        score = gap * 2 + 8;
      } else continue;
      if (score < bestScore) {
        bestScore = score;
        best = c;
      }
    }
  if (best) return best.value;
  // 主题写明是验证码、正文里只有一个合格数字：采用它。
  if (words.some((w) => w.end <= head.length)) {
    const values = [...new Set(found.filter((c) => c.start > head.length).map((c) => c.value))];
    if (values.length === 1) return values[0]!;
  }
  return null;
}
