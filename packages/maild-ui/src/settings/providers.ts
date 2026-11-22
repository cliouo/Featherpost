import { m } from "@featherpost/i18n/messages";

import type { AccountDraft, Endpoint } from "./types";
import { BRAND } from "../brand";

/**
 * 内置邮箱服务商预设：地址里的 @domain → 收发服务器、端口、加密方式与凭据指引。
 * 纯数据 + 纯函数；不发网络请求。未知域名由「自动检测服务器」交给后端发现或按惯例猜测。
 *
 * 服务商名称（`name`、`short`）、域名与主机名是服务商数据，不随界面语言翻译（ADR-0048 非目标）；
 * 凭据标签、获取步骤与限制说明是界面文案，写成 getter，每次读取按当前语言成文。
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

/** 句中引用的凭据名（「需要{凭据}」「如何获取{凭据}」）；getter 每次按当前语言成文。 */
export const CREDENTIAL_LABEL: Readonly<Record<CredentialKind, string>> = {
  get authorization_code() {
    return m.settings_credential_authorization_code();
  },
  get app_password() {
    return m.settings_credential_app_password();
  },
  get password() {
    return m.settings_credential_password();
  },
  get bridge_password() {
    return m.settings_credential_bridge_password();
  },
};

export const CUSTOM_PROVIDER = "custom";

const imap = (server: string, port = 993, security: Security = "implicit"): ServerPreset => ({ server, port, security });
const smtp = (server: string, port = 465, security: Security = "implicit"): ServerPreset => ({ server, port, security });

export const PROVIDERS: readonly ProviderPreset[] = [
  {
    id: "qq",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    name: "QQ 邮箱 / Foxmail",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    short: "QQ 邮箱",
    domains: ["qq.com", "foxmail.com", "vip.qq.com"],
    incoming: imap("imap.qq.com"),
    outgoing: smtp("smtp.qq.com"),
    credential: "authorization_code",
    get secretLabel() {
      return m.settings_provider_qq_secret();
    },
    get steps() {
      return [
        m.settings_provider_qq_step_1(),
        m.settings_provider_qq_step_2(),
        m.settings_provider_qq_step_3(),
      ];
    },
    helpUrl: "https://service.mail.qq.com/",
    stripSecretSpaces: true,
  },
  {
    id: "netease",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    name: "网易邮箱",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
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
    get secretLabel() {
      return m.settings_provider_netease_secret();
    },
    get steps() {
      return [
        m.settings_provider_netease_step_1(),
        m.settings_provider_netease_step_2(),
        m.settings_provider_netease_step_3(),
      ];
    },
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
    get secretLabel() {
      return m.settings_provider_gmail_secret();
    },
    get steps() {
      return [
        m.settings_provider_gmail_step_1(),
        m.settings_provider_gmail_step_2(),
        m.settings_provider_gmail_step_3(),
      ];
    },
    helpUrl: "https://myaccount.google.com/apppasswords",
    stripSecretSpaces: true,
  },
  {
    id: "icloud",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    name: "iCloud 邮件",
    short: "iCloud",
    domains: ["icloud.com", "me.com", "mac.com"],
    incoming: imap("imap.mail.me.com"),
    outgoing: smtp("smtp.mail.me.com", 587, "starttls"),
    credential: "app_password",
    get secretLabel() {
      return m.settings_provider_icloud_secret();
    },
    get steps() {
      return [
        m.settings_provider_icloud_step_1(),
        m.settings_provider_icloud_step_2(),
        m.settings_provider_icloud_step_3(),
      ];
    },
    helpUrl: "https://account.apple.com/",
    stripSecretSpaces: true,
  },
  {
    id: "exmail",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    name: "腾讯企业邮",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    short: "企业邮箱",
    domains: ["exmail.qq.com"],
    incoming: imap("imap.exmail.qq.com"),
    outgoing: smtp("smtp.exmail.qq.com"),
    credential: "authorization_code",
    get secretLabel() {
      return m.settings_provider_exmail_secret();
    },
    get steps() {
      return [
        m.settings_provider_exmail_step_1(),
        m.settings_provider_exmail_step_2(),
        m.settings_provider_exmail_step_3(),
      ];
    },
    helpUrl: "https://exmail.qq.com/",
    stripSecretSpaces: true,
  },
  {
    id: "aliyun",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    name: "阿里云邮箱",
    // i18n-lint-allow-cjk: 服务商名称是品牌数据，不是界面文案
    short: "阿里云",
    domains: ["aliyun.com"],
    incoming: imap("imap.aliyun.com"),
    outgoing: smtp("smtp.aliyun.com"),
    credential: "password",
    get secretLabel() {
      return m.settings_provider_secret_mailbox_password();
    },
    get steps() {
      return [
        m.settings_provider_aliyun_step_1(),
        m.settings_provider_aliyun_step_2(),
      ];
    },
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
    get secretLabel() {
      return m.settings_provider_outlook_secret();
    },
    get steps() {
      return [
        m.settings_provider_outlook_step_1(),
        m.settings_provider_outlook_step_2(),
      ];
    },
    helpUrl: "https://account.microsoft.com/security",
    stripSecretSpaces: true,
    get caveat() {
      return m.settings_provider_outlook_caveat({ product: BRAND.productName });
    },
  },
  {
    id: "fastmail",
    name: "Fastmail",
    short: "Fastmail",
    domains: ["fastmail.com", "fastmail.fm"],
    incoming: imap("imap.fastmail.com"),
    outgoing: smtp("smtp.fastmail.com"),
    credential: "app_password",
    get secretLabel() {
      return m.settings_provider_secret_app_password();
    },
    get steps() {
      return [
        m.settings_provider_fastmail_step_1(),
        m.settings_provider_fastmail_step_2(),
      ];
    },
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
    get secretLabel() {
      return m.settings_provider_secret_app_password();
    },
    get steps() {
      return [
        m.settings_provider_yahoo_step_1(),
        m.settings_provider_yahoo_step_2(),
      ];
    },
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
    get secretLabel() {
      return m.settings_provider_secret_app_password();
    },
    get steps() {
      return [
        m.settings_provider_zoho_step_1(),
        m.settings_provider_zoho_step_2(),
      ];
    },
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
    get secretLabel() {
      return m.settings_provider_proton_secret();
    },
    get steps() {
      return [
        m.settings_provider_proton_step_1(),
        m.settings_provider_proton_step_2(),
        m.settings_provider_proton_step_3({ product: BRAND.productName }),
      ];
    },
    helpUrl: "https://proton.me/mail/bridge",
    stripSecretSpaces: false,
    get caveat() {
      return m.settings_provider_proton_caveat();
    },
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

const STAGE_LABEL: Readonly<Record<string, () => string>> = {
  dns: m.settings_probe_stage_dns,
  connect: m.settings_probe_stage_connect,
  tls: m.settings_probe_stage_tls,
  auth: m.settings_probe_stage_auth,
  mailbox: m.settings_probe_stage_mailbox,
  capability: m.settings_probe_stage_capability,
  ready: m.settings_probe_stage_ready,
  provider: m.settings_probe_stage_provider,
};

export function stageLabel(value: string) {
  const label = Object.prototype.hasOwnProperty.call(STAGE_LABEL, value)
    ? STAGE_LABEL[value]
    : undefined;
  return label ? label() : m.settings_probe_stage_unknown();
}

/** 失败阶段的简明排障建议；服务商有特别说明时优先给出。 */
export function probeAdvice(stage: string, side: "incoming" | "outgoing", preset: ProviderPreset | null): string {
  const what = side === "incoming" ? "IMAP" : "SMTP";
  switch (stage) {
    case "dns":
      return m.settings_probe_advice_dns({ protocol: what });
    case "connect":
      return m.settings_probe_advice_connect({ protocol: what });
    case "tls":
      return m.settings_probe_advice_tls();
    case "auth":
      if (preset?.caveat && preset.id === "outlook") return preset.caveat;
      return preset
        ? m.settings_probe_advice_auth_provider({
            provider: preset.name,
            credential: CREDENTIAL_LABEL[preset.credential],
          })
        : m.settings_probe_advice_auth();
    case "mailbox":
      return m.settings_probe_advice_mailbox();
    case "capability":
      return m.settings_probe_advice_capability({ product: BRAND.productName });
    case "provider":
      return m.settings_probe_advice_provider();
    default:
      return m.settings_probe_advice_default();
  }
}
