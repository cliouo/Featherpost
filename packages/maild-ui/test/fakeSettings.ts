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
  RulesResult,
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
}
