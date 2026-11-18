export interface Endpoint {
  kind: string;
  server?: string;
  port?: number;
  security?: string;
  username?: string;
  mailbox?: string;
  idle?: boolean;
  poll_secs?: number;
  provider?: string;
  credential?: string;
}
export interface AccountProfile {
  id: string;
  revision: number;
  display_name: string;
  address: string;
  enabled: boolean;
  source: string;
  editable: boolean;
  incoming: Endpoint;
  outgoing?: Endpoint | null;
  credentials: { incoming: string; outgoing?: string | null };
  runtime: { state: string; detail: string };
}
export interface AccountDraft {
  display_name: string;
  address: string;
  incoming: Endpoint;
  outgoing: Endpoint | null;
}
export interface AccountEdit {
  id: string | null;
  revision: number | null;
  draft: AccountDraft;
  secrets: { incoming?: string; outgoing?: string };
}
export interface ProbeStage {
  ok: boolean;
  stage: string;
  /** 后端诊断原文（服务器回应摘要）。旧后端会清空或省略。 */
  detail?: string | null;
}
export interface Probe {
  incoming: ProbeStage;
  outgoing?: ProbeStage | null;
}
export interface Rules {
  show_sensitive: boolean;
  silent_keywords: string[];
  quiet_hours: { start: string; end: string }[];
  timezone_offset_minutes: number;
  /** 静默时段跟随系统本地时区与夏令时；旧后端不回传此键，界面据此判断是否支持。 */
  follow_system_timezone?: boolean;
  /** 收件箱新邮件系统通知；旧后端不回传此键，界面据此判断是否支持。 */
  notify_new_mail?: boolean;
  /** 不弹通知的账户 id。 */
  muted_accounts?: string[];
}
/** 与内核 RulesConfig 同名的别名，便于对照。 */
export type RulesConfig = Rules;
/** 后端服务器发现结果：未知域名点「自动检测服务器」时使用。 */
export interface DiscoveredServers {
  incoming: { server: string; port: number; security: string };
  outgoing?: { server: string; port: number; security: string } | null;
  /** 发现来源，如 autoconfig / srv / guess。 */
  source?: string;
}
export interface RulesResult {
  rules: Rules;
  persisted: boolean | null;
}
export interface NotificationStatus {
  provider: string;
  availability: string;
  authorization: string;
  request_in_flight: boolean;
  fresh: boolean;
}
export interface Address {
  id: string;
  account: string;
  address: string;
  kind: string;
  label: string;
  state: string;
  created_at: number;
  expires_at?: number | null;
}
export interface AddressPage {
  addresses: Address[];
  total: number;
}
export interface Frontend {
  id: string;
  name: string;
  version: string;
  lifecycle: string;
  capabilities: string[];
  permissions: string[];
  builtin: boolean;
  has_identity: boolean;
}
export interface Plugin {
  id: string;
  name: string;
  version: string;
  lifecycle: string;
  capabilities: string[];
  enabled: boolean;
  state: string;
  provider_registered: boolean;
  has_ui: boolean;
}
export interface Packages {
  default: string;
  frontends: Frontend[];
  plugins: Plugin[];
}
export type PackageKind = "plugin" | "frontend";
export interface PackagePreview {
  kind: PackageKind;
  id: string;
  name: string;
  version: string;
  lifecycle: string;
  capabilities: string[];
  permissions: string[];
  has_ui: boolean;
}
export interface PackageChoice {
  token: string;
  source: string;
  preview: PackagePreview;
}
export type AccountAction =
  | { action: "enable"; id: string; revision: number; enabled: boolean }
  | { action: "remove"; id: string; revision: number };
export type AddressAction =
  | {
      action: "create";
      account: string;
      kind: string;
      address: string | null;
      label: string;
      expires_secs: number | null;
    }
  | { action: "update"; id: string; state?: string; label?: string }
  | { action: "remove"; id: string };
export type PackageAction =
  | { action: "enable"; id: string; enabled: boolean }
  | { action: "remove"; kind: PackageKind; id: string }
  | { action: "open" | "default"; id: string }
  | { action: "refresh" | "recovery" };
export interface SettingsBridge {
  profiles(): Promise<AccountProfile[]>;
  probe(request: AccountEdit): Promise<Probe>;
  saveAccount(request: AccountEdit): Promise<AccountProfile>;
  accountAction(request: AccountAction): Promise<void>;
  rules(): Promise<RulesResult>;
  saveRules(patch: Rules): Promise<RulesResult>;
  notification(request?: boolean): Promise<NotificationStatus>;
  addresses(account: string, offset: number): Promise<AddressPage>;
  addressAction(request: AddressAction): Promise<void>;
  packages(): Promise<Packages>;
  packageAction(request: PackageAction): Promise<void>;
  pickPackage(kind: PackageKind): Promise<PackageChoice | null>;
  cancelPackage(token: string): Promise<void>;
  installPackage(
    token: string,
  ): Promise<{ id: string; version: string; action: string }>;
  rollbackPreview(kind: PackageKind, id: string): Promise<PackagePreview>;
  rollback(preview: PackagePreview): Promise<void>;
  /** 可选：后端按域名发现收发服务器；未提供或失败时界面按惯例预填。 */
  discoverProvider?(address: string): Promise<DiscoveredServers | null>;
  /** 可选：经宿主安全打开服务商官方帮助页（仅 https）。 */
  openExternalUrl?(url: string): Promise<void>;
}
