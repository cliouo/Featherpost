import type {
  SettingsBridge,
  AccountEdit,
  AccountProfile,
  AccountAction,
  Rules,
  NotificationStatus,
  AddressPage,
  AddressAction,
  Packages,
  PackageAction,
  PackageKind,
  PackageChoice,
  PackagePreview,
  Probe,
  ProxyConfigResult,
  ProxyConfigUpdate,
  ProxyPreferenceEvent,
  RulesResult,
  TestProxyRequest,
  TestProxyResult,
} from "../src/settings/types";
import { BRAND } from "../src/brand";
export const account = (): AccountProfile => ({
  id: "acct-a",
  revision: 2,
  display_name: "工作邮箱",
  address: "me@example.test",
  enabled: true,
  source: "managed",
  editable: true,
  incoming: {
    kind: "imap",
    server: "imap.example.test",
    port: 993,
    security: "implicit",
    username: "me",
    mailbox: "INBOX",
    idle: true,
    poll_secs: 60,
  },
  outgoing: {
    kind: "smtp",
    server: "smtp.example.test",
    port: 587,
    security: "starttls",
    username: "me",
    credential: "incoming",
  },
  credentials: { incoming: "saved", outgoing: "shared" },
  runtime: { state: "online", detail: "" },
});
export const rules = (): Rules => ({
  show_sensitive: false,
  silent_keywords: [],
  quiet_hours: [],
  timezone_offset_minutes: 0,
});
export const modernRules = (): Rules => ({
  ...rules(),
  follow_system_timezone: false,
  notify_new_mail: false,
  muted_accounts: [],
});
export const preview = (): PackagePreview => ({
  kind: "plugin",
  id: "test.provider",
  name: "Example Provider",
  version: "1.1.0",
  lifecycle: "independent",
  capabilities: ["transport_provider"],
  permissions: [],
  has_ui: false,
});
export class FakeSettings implements SettingsBridge {
  profileRows = [account()];
  catalog: Packages = { default: BRAND.bundledFrontendId, frontends: [], plugins: [] };
  accountWrites: AccountEdit[] = [];
  accountActions: AccountAction[] = [];
  addressWrites: AddressAction[] = [];
  packageWrites: PackageAction[] = [];
  installs: string[] = [];
  cancels: string[] = [];
  probes: AccountEdit[] = [];
  savedRules: Rules[] = [];
  notificationRequests: boolean[] = [];
  probeResult: Probe | null = null;
  proxyState: ProxyConfigResult = {
    config: { mode: "system", custom: null },
    has_password: false,
    effective_system: {
      http: null,
      https: null,
      socks5: null,
      bypass: [],
      exclude_simple_hostnames: false,
      pac_url: null,
      source: "none",
    },
    persisted: true,
  };
  proxyWrites: ProxyConfigUpdate[] = [];
  proxyTests: TestProxyRequest[] = [];
  proxyTestResult: TestProxyResult = {
    ok: true,
    route: "direct",
    stage: "ready",
    latency_ms: 42,
    detail_code: null,
    detail: "",
  };
  proxyError: Error | null = null;
  proxyHandlers = new Set<(event: ProxyPreferenceEvent) => void>();
  discoverProvider?: SettingsBridge["discoverProvider"];
  openExternalUrl?: SettingsBridge["openExternalUrl"];
  async profiles() {
    return structuredClone(this.profileRows);
  }
  async probe(request: AccountEdit): Promise<Probe> {
    this.probes.push(request);
    if (this.probeResult) return structuredClone(this.probeResult);
    return {
      incoming: { ok: true, stage: "ready" },
      outgoing: { ok: true, stage: "ready" },
    };
  }
  async saveAccount(request: AccountEdit): Promise<AccountProfile> {
    this.accountWrites.push(request);
    return { ...account(), ...request.draft };
  }
  async accountAction(request: AccountAction) {
    this.accountActions.push(request);
  }
  async rules(): Promise<RulesResult> {
    return { rules: rules(), persisted: null };
  }
  async saveRules(patch: Rules) {
    this.savedRules.push(patch);
    return { rules: patch, persisted: false };
  }
  async notification(request = false): Promise<NotificationStatus> {
    this.notificationRequests.push(request);
    return {
      provider: "none",
      availability: "unsupported",
      authorization: "not_applicable",
      fresh: true,
      request_in_flight: false,
    };
  }
  async addresses(_account: string, _offset: number): Promise<AddressPage> {
    return { addresses: [], total: 0 };
  }
  async addressAction(request: AddressAction) {
    this.addressWrites.push(request);
  }
  async packages() {
    return this.catalog;
  }
  async packageAction(request: PackageAction) {
    this.packageWrites.push(request);
  }
  async pickPackage(_kind: PackageKind): Promise<PackageChoice | null> {
    return {
      token: "native-token",
      source: "Selected package",
      preview: preview(),
    };
  }
  async cancelPackage(token: string) {
    this.cancels.push(token);
  }
  async installPackage(token: string) {
    this.installs.push(token);
    return { id: "test.provider", version: "1.1.0", action: "installed" };
  }
  async rollbackPreview(_kind: PackageKind, _id: string) {
    return preview();
  }
  async rollback(_preview: PackagePreview) {}
  async proxyConfig(): Promise<ProxyConfigResult> {
    if (this.proxyError) throw this.proxyError;
    return structuredClone(this.proxyState);
  }
  async setProxyConfig(update: ProxyConfigUpdate): Promise<ProxyConfigResult> {
    if (this.proxyError) throw this.proxyError;
    this.proxyWrites.push(structuredClone(update));
    const prevCustom = this.proxyState.config.custom;
    let nextCustom = prevCustom ? structuredClone(prevCustom) : null;
    let hasPassword = this.proxyState.has_password;
    if (update.custom) {
      const clear = Boolean(update.clear_password || update.custom.clear_password);
      if (clear) {
        hasPassword = false;
      } else if (update.custom.password !== undefined && update.custom.password !== null && update.custom.password !== "") {
        hasPassword = true;
      }
      nextCustom = {
        protocol: update.custom.protocol,
        host: update.custom.host,
        port: update.custom.port,
        username: update.custom.username ?? null,
        bypass: [...update.custom.bypass],
      };
    } else if (update.clear_password) {
      hasPassword = false;
    }
    this.proxyState = {
      ...this.proxyState,
      config: { mode: update.mode, custom: nextCustom },
      has_password: hasPassword,
    };
    return structuredClone(this.proxyState);
  }
  async testProxy(request: TestProxyRequest): Promise<TestProxyResult> {
    if (this.proxyError) throw this.proxyError;
    this.proxyTests.push(structuredClone(request));
    return structuredClone(this.proxyTestResult);
  }
  async onProxyChanged(handler: (event: ProxyPreferenceEvent) => void) {
    this.proxyHandlers.add(handler);
    return () => {
      this.proxyHandlers.delete(handler);
    };
  }
  pushProxy(event: ProxyPreferenceEvent) {
    this.proxyState = {
      config: structuredClone(event.config),
      has_password: event.has_password,
      effective_system: structuredClone(event.effective_system),
      persisted: event.persisted,
    };
    for (const handler of this.proxyHandlers) handler(structuredClone(event));
  }
}
