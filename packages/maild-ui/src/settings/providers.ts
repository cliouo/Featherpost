import type { AccountDraft, Endpoint } from "./types";
import { BRAND } from "../brand";

/**
 * 内置邮箱服务商预设：地址里的 @domain → 收发服务器、端口、加密方式与凭据指引。
 * 纯数据 + 纯函数；不发网络请求。未知域名由「自动检测服务器」交给后端发现或按惯例猜测。
 */
export type Security = "implicit" | "starttls";
export type CredentialKind =
  | "authorization_code"
  | "app_password"
  | "password"
  | "bridge_password";

export interface ServerPreset {
  server: string;
  port: number;
  security: Security;
}

export interface ProviderPreset {
  id: string;
  /** 完整名称，用于标题与提示。 */
  name: string;
  /** 快捷胶囊上的短名。 */
  short: string;
  domains: readonly string[];
  incoming: ServerPreset;
  outgoing: ServerPreset;
  /** 同一服务商下不同域名使用不同主机（如 126.com → imap.126.com）。 */
  hosts?: Readonly<Record<string, { incoming: string; outgoing: string }>>;
  credential: CredentialKind;
  /** 密码框标签，如「客户端授权码（16 位）」。 */
  secretLabel: string;
  /** 获取凭据的简明步骤。 */
  steps: readonly string[];
  /** 官方设置/帮助页；只经宿主的外部链接通道打开。 */
  helpUrl?: string;
  /** 应用密码常显示为 `xxxx xxxx xxxx xxxx`，粘贴或提交时去掉所有空白。 */
  stripSecretSpaces: boolean;
  /** 诚实说明的限制（如微软个人账户已停用密码登录）。 */
  caveat?: string;
}

export const CREDENTIAL_LABEL: Record<CredentialKind, string> = {
  authorization_code: "客户端授权码",
  app_password: "应用专用密码",
  password: "邮箱密码",
  bridge_password: "Bridge 密码",
};

export const CUSTOM_PROVIDER = "custom";

const imap = (server: string, port = 993, security: Security = "implicit"): ServerPreset => ({ server, port, security });
const smtp = (server: string, port = 465, security: Security = "implicit"): ServerPreset => ({ server, port, security });

export const PROVIDERS: readonly ProviderPreset[] = [
  {
    id: "qq",
    name: "QQ 邮箱 / Foxmail",
    short: "QQ 邮箱",
    domains: ["qq.com", "foxmail.com", "vip.qq.com"],
    incoming: imap("imap.qq.com"),
    outgoing: smtp("smtp.qq.com"),
    credential: "authorization_code",
    secretLabel: "客户端授权码（16 位）",
    steps: [
      "在网页版 QQ 邮箱打开「设置 → 账号与安全 → 安全设置」",
      "开启「IMAP/SMTP 服务」，按提示完成验证",
      "生成授权码并粘贴到这里；它不是 QQ 密码",
    ],
    helpUrl: "https://service.mail.qq.com/",
    stripSecretSpaces: true,
  },
  {
    id: "netease",
    name: "网易邮箱",
    short: "网易 163",
    domains: ["163.com", "126.com", "yeah.net", "vip.163.com", "188.com"],
    incoming: imap("imap.163.com"),
    outgoing: smtp("smtp.163.com"),
    hosts: {
      "126.com": { incoming: "imap.126.com", outgoing: "smtp.126.com" },
      "yeah.net": { incoming: "imap.yeah.net", outgoing: "smtp.yeah.net" },
      "vip.163.com": { incoming: "imap.vip.163.com", outgoing: "smtp.vip.163.com" },
      "188.com": { incoming: "imap.188.com", outgoing: "smtp.188.com" },
    },
    credential: "authorization_code",
    secretLabel: "客户端授权码",
    steps: [
      "在网页版网易邮箱打开「设置 → POP3/SMTP/IMAP」",
      "开启「IMAP/SMTP 服务」，按提示完成手机验证",
      "新增授权密码并粘贴到这里；它不是登录密码",
    ],
    helpUrl: "https://help.mail.163.com/",
    stripSecretSpaces: true,
  },
  {
    id: "gmail",
    name: "Gmail / Google",
    short: "Gmail",
    domains: ["gmail.com", "googlemail.com"],
    incoming: imap("imap.gmail.com"),
    outgoing: smtp("smtp.gmail.com"),
    credential: "app_password",
    secretLabel: "应用专用密码（16 位）",
    steps: [
      "Google 账号需先开启两步验证",
      "打开「Google 账号 → 安全性 → 应用专用密码」并新建一个",
      "复制显示的 16 位密码；中间的空格会自动去掉",
    ],
    helpUrl: "https://myaccount.google.com/apppasswords",
    stripSecretSpaces: true,
  },
  {
    id: "icloud",
    name: "iCloud 邮件",
    short: "iCloud",
    domains: ["icloud.com", "me.com", "mac.com"],
    incoming: imap("imap.mail.me.com"),
    outgoing: smtp("smtp.mail.me.com", 587, "starttls"),
    credential: "app_password",
    secretLabel: "App 专用密码",
    steps: [
      "Apple 账户需开启双重认证",
      "在 account.apple.com 打开「登录与安全 → App 专用密码」并生成",
      "粘贴生成的密码；它不是 Apple 账户密码",
    ],
    helpUrl: "https://account.apple.com/",
    stripSecretSpaces: true,
  },
  {
    id: "exmail",
    name: "腾讯企业邮",
    short: "企业邮箱",
    domains: ["exmail.qq.com"],
    incoming: imap("imap.exmail.qq.com"),
    outgoing: smtp("smtp.exmail.qq.com"),
    credential: "authorization_code",
    secretLabel: "邮箱密码或客户端专用密码",
    steps: [
      "使用公司分配的完整邮箱地址",
      "若开启了「安全登录」，需在企业邮「设置 → 账户 → 客户端专用密码」生成后粘贴",
      "IMAP/SMTP 由管理员统一开关；无法连接时请联系管理员",
    ],
    helpUrl: "https://exmail.qq.com/",
    stripSecretSpaces: true,
  },
  {
    id: "aliyun",
    name: "阿里云邮箱",
    short: "阿里云",
    domains: ["aliyun.com"],
    incoming: imap("imap.aliyun.com"),
    outgoing: smtp("smtp.aliyun.com"),
    credential: "password",
    secretLabel: "邮箱密码",
    steps: [
      "在网页版阿里邮箱「设置 → 账户 → POP3/SMTP 及 IMAP/SMTP」中开启 IMAP",
      "使用邮箱登录密码；开启了独立密码时使用独立密码",
    ],
    helpUrl: "https://mail.aliyun.com/",
    stripSecretSpaces: false,
  },
  {
    id: "outlook",
    name: "Outlook / Hotmail",
    short: "Outlook",
    domains: ["outlook.com", "hotmail.com", "live.com", "msn.com"],
    incoming: imap("outlook.office365.com"),
    outgoing: smtp("smtp-mail.outlook.com", 587, "starttls"),
    credential: "app_password",
    secretLabel: "应用密码",
    steps: [
      "Microsoft 365 工作或学校账户：请管理员为你开启 IMAP 与 SMTP AUTH",
      "若组织允许应用密码，在「Microsoft 账户 → 安全性」生成后粘贴",
    ],
    helpUrl: "https://account.microsoft.com/security",
    stripSecretSpaces: true,
    caveat:
      `微软已停止个人 Outlook.com / Hotmail 账户在第三方客户端中使用密码登录，只接受 OAuth。${BRAND.productName} 目前尚不支持 OAuth，个人账户探测会在「认证」阶段失败。`,
  },
  {
    id: "fastmail",
    name: "Fastmail",
    short: "Fastmail",
    domains: ["fastmail.com", "fastmail.fm"],
    incoming: imap("imap.fastmail.com"),
    outgoing: smtp("smtp.fastmail.com"),
    credential: "app_password",
    secretLabel: "应用专用密码",
    steps: [
      "打开 Fastmail「设置 → 隐私与安全 → 应用专用密码」",
      "新建一个允许 IMAP 与 SMTP 的密码并粘贴",
    ],
    helpUrl: "https://www.fastmail.help/",
    stripSecretSpaces: true,
  },
  {
    id: "yahoo",
    name: "Yahoo Mail",
    short: "Yahoo",
    domains: ["yahoo.com", "ymail.com", "rocketmail.com"],
    incoming: imap("imap.mail.yahoo.com"),
    outgoing: smtp("smtp.mail.yahoo.com"),
    credential: "app_password",
    secretLabel: "应用专用密码",
    steps: [
      "打开 Yahoo「账户信息 → 账户安全」",
      "选择「生成应用密码」，复制后粘贴",
    ],
    helpUrl: "https://login.yahoo.com/account/security",
    stripSecretSpaces: true,
  },
  {
    id: "zoho",
    name: "Zoho Mail",
    short: "Zoho",
    domains: ["zoho.com", "zohomail.com", "zohomail.cn"],
    incoming: imap("imap.zoho.com"),
    outgoing: smtp("smtp.zoho.com"),
    hosts: {
      "zohomail.cn": { incoming: "imap.zoho.com.cn", outgoing: "smtp.zoho.com.cn" },
    },
    credential: "app_password",
    secretLabel: "应用专用密码",
    steps: [
      "在 Zoho Mail「设置 → 邮件账户 → IMAP 访问」中启用 IMAP",
      "开启两步验证时，在 Zoho 账户「安全 → 应用专用密码」生成后粘贴",
    ],
    helpUrl: "https://accounts.zoho.com/",
    stripSecretSpaces: true,
  },
  {
    id: "proton",
    name: "Proton Mail Bridge",
    short: "Proton",
    domains: ["proton.me", "pm.me", "protonmail.com", "protonmail.ch"],
    incoming: imap("127.0.0.1", 1143, "starttls"),
    outgoing: smtp("127.0.0.1", 1025, "starttls"),
    credential: "bridge_password",
    secretLabel: "Bridge 密码",
    steps: [
      "在本机安装并登录 Proton Mail Bridge（需付费计划）",
      "在 Bridge 的邮箱详情里复制 IMAP/SMTP 用户名与 Bridge 密码",
      `Bridge 必须保持运行，${BRAND.productName} 才能收发`,
    ],
    helpUrl: "https://proton.me/mail/bridge",
    stripSecretSpaces: false,
    caveat: "Bridge 只在本机 127.0.0.1 提供服务；端口若在 Bridge 设置里改过，请展开连接参数同步修改。",
  },
];

/** 快捷胶囊顺序；最后一个是「自定义 IMAP」。 */
export const QUICK_PROVIDERS: readonly string[] = ["qq", "netease", "gmail", "icloud", "exmail", CUSTOM_PROVIDER];

export const STANDARD_PORTS = {
  imap: { implicit: 993, starttls: 143 },
  smtp: { implicit: 465, starttls: 587 },
} as const;

export function providerById(id: string | null | undefined): ProviderPreset | null {
  return PROVIDERS.find((p) => p.id === id) ?? null;
}

export function domainOf(address: string): string | null {
  const at = address.trim().lastIndexOf("@");
  if (at <= 0) return null;
  const domain = address.trim().slice(at + 1).toLowerCase().replace(/\.$/, "");
  return /^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(domain) ? domain : null;
}

export function matchProvider(address: string): ProviderPreset | null {
  const domain = domainOf(address);
  if (!domain) return null;
  return PROVIDERS.find((p) => p.domains.includes(domain)) ?? null;
}

/** 预设在某个域名下的实际收发服务器。 */
export function serversFor(preset: ProviderPreset, domain: string | null): { incoming: ServerPreset; outgoing: ServerPreset } {
  const host = domain ? preset.hosts?.[domain] : undefined;
  return {
    incoming: { ...preset.incoming, server: host?.incoming ?? preset.incoming.server },
    outgoing: { ...preset.outgoing, server: host?.outgoing ?? preset.outgoing.server },
  };
}

/** 未知域名的惯例猜测：imap.<domain>:993 / smtp.<domain>:465。 */
export function guessServers(domain: string): { incoming: ServerPreset; outgoing: ServerPreset } {
  return { incoming: imap(`imap.${domain}`), outgoing: smtp(`smtp.${domain}`) };
}

/** 把服务器参数写进草稿；保留用户名、文件夹、IDLE、发信凭据方式等其余字段。 */
export function applyServers(
  draft: AccountDraft,
  servers: { incoming: ServerPreset; outgoing: ServerPreset },
): AccountDraft {
  const username = draft.address.trim();
  const incoming: Endpoint = {
    ...draft.incoming,
    kind: "imap",
    ...servers.incoming,
    username: draft.incoming.username || username,
  };
  const outgoing: Endpoint | null = draft.outgoing
    ? {
        ...draft.outgoing,
        kind: "smtp",
        ...servers.outgoing,
        username: draft.outgoing.username || username,
      }
    : null;
  return { ...draft, incoming, outgoing };
}

export function applyProvider(draft: AccountDraft, preset: ProviderPreset): AccountDraft {
  return applyServers(draft, serversFor(preset, domainOf(draft.address)));
}

/**
 * 切换 TLS ↔ STARTTLS：端口仍是旧模式的标准端口时同步换成新模式的标准端口；
 * 用户自己填的非标准端口原样保留。
 */
export function portForSecurity(
  protocol: "imap" | "smtp",
  port: number | undefined,
  from: string | undefined,
  to: string,
): number | undefined {
  const table = STANDARD_PORTS[protocol] as Record<string, number>;
  const previous = table[from ?? "implicit"];
  const next = table[to];
  if (next === undefined) return port;
  if (port === undefined || Number.isNaN(port) || port === previous) return next;
  return port;
}

/** 应用密码清理：去掉首尾与分组空白。返回清理后的值与是否改动过。 */
export function cleanSecret(value: string, strip: boolean): { value: string; changed: boolean } {
  if (!strip) return { value, changed: false };
  const cleaned = value.replace(/\s+/g, "");
  return { value: cleaned, changed: cleaned !== value };
}

/** 这些阶段失败说明服务器参数本身可疑，自动展开连接参数面板。 */
export const SERVER_STAGES: readonly string[] = ["dns", "connect", "tls", "mailbox"];

export function stageLabel(value: string) {
  return (
    (
      {
        dns: "服务器名称",
        connect: "连接",
        tls: "TLS",
        auth: "认证",
        mailbox: "文件夹",
        capability: "服务能力",
        ready: "就绪",
        provider: "服务扩展",
      } as Record<string, string>
    )[value] ?? "未知阶段"
  );
}

/** 失败阶段的简明排障建议；服务商有特别说明时优先给出。 */
export function probeAdvice(stage: string, side: "incoming" | "outgoing", preset: ProviderPreset | null): string {
  const what = side === "incoming" ? "IMAP" : "SMTP";
  switch (stage) {
    case "dns":
      return `找不到 ${what} 服务器，请核对服务器地址拼写，或点「自动检测服务器」。`;
    case "connect":
      return `能解析服务器但连不上，请核对端口，并确认网络或代理没有拦截 ${what}。`;
    case "tls":
      return `加密握手失败：993 / 465 通常配 TLS，143 / 587 通常配 STARTTLS。`;
    case "auth":
      if (preset?.caveat && preset.id === "outlook") return preset.caveat;
      return preset
        ? `服务器拒绝了登录。${preset.name} 需要${CREDENTIAL_LABEL[preset.credential]}，而不是网页登录密码；请确认已开启 IMAP/SMTP 并重新生成。`
        : "服务器拒绝了登录，请核对用户名与密码；许多服务商要求使用应用专用密码或授权码。";
    case "mailbox":
      return "登录成功，但找不到收信文件夹，请核对「收信文件夹」（通常为 INBOX）。";
    case "capability":
      return `服务器缺少 ${BRAND.productName} 需要的能力，请确认服务商已开启 IMAP。`;
    case "provider":
      return "邮箱服务扩展未就绪，请在「扩展」中确认它已启用。";
    default:
      return "请核对连接参数后重试。";
  }
}
