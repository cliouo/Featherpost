import { getUiLocale, matchTag, type UiLocale } from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";

export { useUiLocale } from "@featherpost/i18n/react";
export type { UiLocale };

/**
 * 当前协商出的界面语言（ADR-0048 决策 8），`Intl` 格式化与 `localeCompare` 也用它。
 * 未收到偏好时为 `zh-CN`。服务商数据与用户自起的名字不是界面文案，不随它翻译。
 */
export function uiLocale(): UiLocale {
  return getUiLocale();
}

/** manifest `[l10n.<tag>]` 的一条（ADR-0048 决策 5）；缺的字段回落顶层。 */
export interface PackageL10nEntry {
  name?: string;
  description?: string;
}

/** 列表、预览与 `listFrontends` 里带名字的包：顶层是兜底，`l10n` 由 daemon 原样透传。 */
export interface LocalizablePackage {
  name: string;
  description?: string | null;
  l10n?: Readonly<Record<string, PackageL10nEntry>> | null;
}

/**
 * 包的名字与说明按当前界面语言成文（ADR-0048 决策 5）：在 `l10n` 里找与界面语言匹配的标签
 * （与界面协商同一规则，`zh-Hant*` 不算简体中文），逐字段取第一个有值的条目，都没有就用顶层
 * `name` / `description`。只在渲染时调用——`previewPluginPackage` 的结果必须原样回传，不能改写。
 */
export function packageText(pkg: LocalizablePackage): { name: string; description: string | null } {
  const locale = getUiLocale();
  const entries = Object.entries(pkg.l10n ?? {})
    .filter(([tag]) => matchTag(tag) === locale)
    .map(([, entry]) => entry);
  const field = (key: keyof PackageL10nEntry) =>
    entries.map((entry) => entry[key]).find((value) => typeof value === "string" && value.trim() !== "");
  return {
    name: field("name") ?? pkg.name,
    description: field("description") ?? pkg.description ?? null,
  };
}

const IDENTITY_FAILURES: Record<string, () => string> = {
  not_installed: m.common_identity_failure_not_installed,
  rejected: m.common_identity_failure_rejected,
  principal_mismatch: m.common_identity_failure_principal_mismatch,
  unavailable: m.common_identity_failure_unavailable,
};

/**
 * 身份绑定失败的说明（ADR-0048 决策 3）：按壳给的稳定 code 用界面语言成文；没有失败时为 `null`。
 * actor 拼的中文原因只作诊断，不显示给用户；认不出的 code（或老壳没有 code）给通用说明。
 */
export function identityFailureText(snapshot: {
  identity_failure: string | null;
  identity_failure_code?: string | null;
}): string | null {
  if (snapshot.identity_failure === null && !snapshot.identity_failure_code) return null;
  const code = snapshot.identity_failure_code ?? "";
  return Object.prototype.hasOwnProperty.call(IDENTITY_FAILURES, code)
    ? IDENTITY_FAILURES[code]()
    : m.common_identity_failure_unknown();
}

/** 账户的收信插件名：在扩展清单里按 ID 找到就按界面语言成文，找不到就显示 ID。 */
export function providerName(
  plugins: ReadonlyArray<LocalizablePackage & { id: string }> | undefined,
  id: string,
): string {
  const plugin = plugins?.find((candidate) => candidate.id === id);
  return plugin ? packageText(plugin).name : id;
}

interface DisplayFolder {
  name: string;
  display_name: string;
  role: string;
}

const folderNames: Record<
  string,
  { label: () => string; aliases: readonly string[] }
> = {
  inbox: { label: m.mailbox_folder_inbox, aliases: ["inbox"] },
  sent: {
    label: m.mailbox_folder_sent,
    aliases: ["sent", "sent mail", "sent messages", "sent items"],
  },
  drafts: { label: m.mailbox_folder_drafts, aliases: ["draft", "drafts"] },
  archive: { label: m.mailbox_folder_archive, aliases: ["archive", "archives"] },
  trash: {
    label: m.mailbox_folder_trash,
    aliases: ["trash", "deleted items", "deleted messages"],
  },
  junk: {
    label: m.mailbox_folder_junk,
    aliases: ["junk", "junk email", "junk e-mail", "spam"],
  },
  all: { label: m.mailbox_folder_all, aliases: ["all mail", "all messages"] },
};
folderNames.spam = folderNames.junk;

/** Translate recognized system folders only, without ever changing their bridge identity. */
export function folderLabel(
  folder: DisplayFolder | null | undefined,
  fallback = "",
): string {
  if (!folder) return fallback;
  const label = folder.display_name || folder.name;
  const separator = Math.max(
    folder.name.lastIndexOf("/"),
    folder.name.lastIndexOf("."),
  );
  const leaf = folder.name.slice(separator + 1);
  // An explicit display name is user/provider data, not a string to guess a translation for.
  if (
    label.toLowerCase() !== folder.name.toLowerCase() &&
    label.toLowerCase() !== leaf.toLowerCase()
  )
    return label;
  const role =
    folder.role || (folder.name.toLowerCase() === "inbox" ? "inbox" : "");
  const entry = Object.prototype.hasOwnProperty.call(folderNames, role)
    ? folderNames[role]
    : undefined;
  if (!entry) return label;
  const labelSeparator = Math.max(
    label.lastIndexOf("/"),
    label.lastIndexOf("."),
  );
  const visibleLeaf = label.slice(labelSeparator + 1);
  if (!entry.aliases.includes(visibleLeaf.toLowerCase())) return label;
  return label.slice(0, labelSeparator + 1) + entry.label();
}

const lifecycleLabels: Record<string, () => string> = {
  independent: m.extensions_lifecycle_independent,
  on_demand: m.extensions_lifecycle_on_demand,
  supervised: m.extensions_lifecycle_supervised,
};

export function lifecycleLabel(value: string): string {
  const label = Object.prototype.hasOwnProperty.call(lifecycleLabels, value)
    ? lifecycleLabels[value]
    : undefined;
  return label
    ? label()
    : value
      ? m.extensions_lifecycle_unknown({ value })
      : m.extensions_lifecycle_undeclared();
}

/** 授权词与贡献类型的界面说明；值是消息函数，每次调用按当前语言成文。 */
export const permissionLabels: Readonly<Record<string, () => string>> = {
  messages_read: m.extensions_permission_messages_read,
  messages_send: m.extensions_permission_messages_send,
  drafts_read: m.extensions_permission_drafts_read,
  drafts_write: m.extensions_permission_drafts_write,
  mailbox_write: m.extensions_permission_mailbox_write,
  accounts_manage: m.extensions_permission_accounts_manage,
  addresses_manage: m.extensions_permission_addresses_manage,
  plugins_manage: m.extensions_permission_plugins_manage,
  settings_read: m.extensions_permission_settings_read,
  settings_write: m.extensions_permission_settings_write,
  notifications_read: m.extensions_permission_notifications_read,
  frontend_open: m.extensions_permission_frontend_open,
  transport_provider: m.extensions_permission_transport_provider,
  events_stream: m.extensions_permission_events_stream,
  inbox: m.extensions_permission_inbox,
  message: m.extensions_permission_message,
  compose: m.extensions_permission_compose,
  settings: m.extensions_permission_settings,
  background: m.extensions_permission_background,
};

export function permissionLabel(value: string): string {
  const label = Object.prototype.hasOwnProperty.call(permissionLabels, value)
    ? permissionLabels[value]
    : undefined;
  return label ? label() : m.extensions_permission_unknown();
}
