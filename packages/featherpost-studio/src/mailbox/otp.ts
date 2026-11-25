/**
 * 验证码提取（纯函数）：必须同时满足
 * 1. 文中出现验证码语境词（验证码 / 校验码 / verification / OTP / code …）；
 * 2. 候选是独立边界的 4～8 位数字，且离语境词足够近（优先语境词之后）。
 * 日期（20261117、2026-11-17、Nov 18, 2026、18 November 2026）、版权年份（© 2026）、
 * 长单号、手机号与分组号码、金额、链接里的数字一律不算；MMDDYYYY / DDMMYYYY 不误杀。
 */
// i18n-lint-allow-cjk: 验证码语境词是内容识别规则（ADR-0048 非目标），与界面语言无关。
const CONTEXT =
  /验证码|校验码|动态码|动态密码|确认码|安全码|激活码|登录码|认证码|提取码|授权码|一次性密码|口令|\b(?:verification|verify|one[- ]?time|otp\b|passcode|security code|log[- ]?in code|sign[- ]?in code|auth code|confirmation code|pin code|2fa\b|codes?\b|pin\b)/gi;
/** 「code」前面是这些词时不是验证码语境（邮编、优惠码等）。 */
// i18n-lint-allow-cjk: 内容识别规则，不是界面文案。
const NOT_OTP = /(zip|postal|post|area|country|promo|promotion|coupon|discount|invite|invitation|referral|gift|source|error|status|tracking|邮编|区号|优惠|兑换|邀请)\s*$/i;
// i18n-lint-allow-cjk: 数字后的单位词是内容识别规则，不是界面文案。
const UNIT_AFTER = /^[ \t\u00a0]*(年|月|日|元|块|美元|人民币|积分|分钟|秒|小时|天|次|件|笔|号|楼|室|%|usd|cny|rmb|eur|gbp|mins?\b|minutes?\b|seconds?\b|hours?\b|days?\b)/i;
const WESTERN_YEAR_PREFIX_WORDS = new Set([
  "jan",
  "january",
  "feb",
  "february",
  "mar",
  "march",
  "apr",
  "april",
  "may",
  "jun",
  "june",
  "jul",
  "july",
  "aug",
  "august",
  "sep",
  "sept",
  "september",
  "oct",
  "october",
  "nov",
  "november",
  "dec",
  "december",
  "copyright",
]);
const SUBJECT_LIMIT = 300;
const BODY_LIMIT = 6000;
const AFTER_WINDOW = 80;
const BEFORE_WINDOW = 30;
const WESTERN_YEAR_WINDOW = 20;

interface Span {
  start: number;
  end: number;
}
interface Candidate extends Span {
  value: string;
}

function isDate(value: string) {
  if (value.length !== 8) return false;
  const year = Number(value.slice(0, 4));
  const month = Number(value.slice(4, 6));
  const day = Number(value.slice(6, 8));
  return year >= 1990 && year <= 2099 && month >= 1 && month <= 12 && day >= 1 && day <= 31;
}

/** 同行前约 20 字符内存在英文月份名/缩写、`©` 或 `copyright` 时，将 1990..=2099 视为年份。 */
function precededByWesternDateOrCopyright(text: string, start: number): boolean {
  if (start <= 0) return false;
  const lineStart =
    Math.max(text.lastIndexOf("\n", start - 1), text.lastIndexOf("\r", start - 1)) + 1;
  const windowStart = Math.max(lineStart, start - WESTERN_YEAR_WINDOW);
  if (text.slice(windowStart, start).includes("©")) return true;
  const linePrefix = text.slice(lineStart, start);
  for (const m of linePrefix.matchAll(/[A-Za-z]+/g)) {
    const wordStart = lineStart + (m.index ?? 0);
    const wordEnd = wordStart + m[0].length;
    if (start - wordStart <= WESTERN_YEAR_WINDOW) {
      const leftOk = wordStart === 0 || !/[A-Za-z0-9]/.test(text[wordStart - 1]!);
      const rightOk = wordEnd === text.length || !/[A-Za-z0-9]/.test(text[wordEnd]!);
      if (leftOk && rightOk && WESTERN_YEAR_PREFIX_WORDS.has(m[0].toLowerCase())) {
        return true;
      }
    }
  }
  return false;
}

function isAsciiUrlChar(ch: string): boolean {
  const code = ch.charCodeAt(0);
  return (
    code >= 0x21 &&
    code <= 0x7e &&
    ch !== "<" &&
    ch !== ">" &&
    ch !== '"' &&
    ch !== "'" &&
    ch !== "(" &&
    ch !== ")" &&
    ch !== "[" &&
    ch !== "]"
  );
}

function insideUrlToken(text: string, start: number, end: number): boolean {
  let tokenStart = start;
  while (tokenStart > 0 && isAsciiUrlChar(text[tokenStart - 1]!)) {
    tokenStart--;
  }
  const token = text.slice(tokenStart, end);
  return /:\/\/|^www\./i.test(token);
}

function contexts(text: string): Span[] {
  const out: Span[] = [];
  for (const m of text.matchAll(CONTEXT)) {
    const start = m.index ?? 0;
    const word = m[0].toLowerCase();
    if (
      (word === "code" || word === "codes" || word === "pin") &&
      NOT_OTP.test(text.slice(Math.max(0, start - 16), start))
    )
      continue;
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
    const beforeCurrency = text.slice(Math.max(0, start - 12), start);
    const after = text.slice(end, end + 2);
    // 分组号码 / 日期 / 时间 / 小数：数字经单个分隔符与另一段数字相连。
    if (/\d[-./:, \t]$/.test(before) || /^[-./:, \t]\d/.test(after)) continue;
    // 货币符号（允许同行隔空白）与电话前缀、编号符号。
    // i18n-lint-allow-cjk: 全角货币与编号符号是内容识别规则。
    if (/[¥￥$€£][ \t\u00a0]*$/.test(beforeCurrency) || /[#+＃][ \t]?$/.test(before)) continue;
    if (value.length === 4) {
      const year = Number(value);
      const nextChar = text[end];
      // i18n-lint-allow-cjk: 「年」后缀判定年份，是内容识别规则。
      if (
        year >= 1990 &&
        year <= 2099 &&
        (nextChar === "年" ||
          nextChar === "-" ||
          nextChar === "/" ||
          precededByWesternDateOrCopyright(text, start))
      ) {
        continue;
      }
    }
    if (isDate(value)) continue;
    if (UNIT_AFTER.test(text.slice(end, end + 12))) continue;
    // 链接里的数字（追踪参数等）不算；仅扫 ASCII URL 字符，避免中文标点连写误伤后续验证码。
    if (insideUrlToken(text, start, end)) continue;
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
