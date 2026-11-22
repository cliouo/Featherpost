/** The shipped interface is Chinese; provider data and user-authored names are not UI copy. */
export const UI_LOCALE = "zh-CN";

interface DisplayFolder {
  name: string;
  display_name: string;
  role: string;
}

const folderNames: Record<
  string,
  { label: string; aliases: readonly string[] }
> = {
  inbox: { label: "收件箱", aliases: ["inbox"] },
  sent: {
    label: "已发送",
    aliases: ["sent", "sent mail", "sent messages", "sent items"],
  },
  drafts: { label: "草稿", aliases: ["draft", "drafts"] },
  archive: { label: "归档", aliases: ["archive", "archives"] },
  trash: {
    label: "已删除",
    aliases: ["trash", "deleted items", "deleted messages"],
  },
  junk: {
    label: "垃圾邮件",
    aliases: ["junk", "junk email", "junk e-mail", "spam"],
  },
  all: { label: "全部邮件", aliases: ["all mail", "all messages"] },
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
  return label.slice(0, labelSeparator + 1) + entry.label;
}

export function lifecycleLabel(value: string): string {
  const label = (
    {
      independent: "独立运行",
      on_demand: "按需启动",
      supervised: "由后台托管运行",
    } as Record<string, string>
  )[value];
  return typeof label === "string"
    ? label
    : value
      ? `未知运行方式（${value}）`
      : "未声明运行方式";
}

export const permissionLabels: Record<string, string> = {
  messages_read: "读取邮件与账户状态",
  messages_send: "发送邮件",
  drafts_read: "读取草稿",
  drafts_write: "编辑草稿",
  mailbox_write: "修改邮件与同步队列",
  accounts_manage: "管理账户与凭据",
  addresses_manage: "管理发件身份",
  plugins_manage: "安装和管理其他扩展",
  settings_read: "读取应用设置",
  settings_write: "修改应用设置",
  notifications_read: "读取系统通知授权",
  frontend_open: "接收导航意图、读取前端清单",
  transport_provider: "提供邮件收发服务",
  events_stream: "订阅事件",
  inbox: "收件箱（读取邮件）",
  message: "阅读邮件（读取邮件）",
  compose: "写信（发送邮件）",
  settings: "设置（读写设置）",
  background: "后台入口",
};

export function permissionLabel(value: string): string {
  const label = permissionLabels[value];
  return typeof label === "string" ? label : "未识别的功能或权限";
}
