import { m } from "@featherpost/i18n/messages";
import { accountLabel } from "./accountLabel";

import type { UiSnapshot } from "../bridge/types";
import { Store } from "../host/store";
import type {
  AccountDraft,
  AccountEdit,
  AccountProfile,
  Address,
  AddressPage,
  DiscoveredServers,
  Endpoint,
  NotificationStatus,
  PackageChoice,
  PackageKind,
  PackagePreview,
  Packages,
  Probe,
  Rules,
  SettingsBridge,
} from "./types";
import {
  CREDENTIAL_LABEL,
  CUSTOM_PROVIDER,
  SERVER_STAGES,
  applyServers,
  cleanSecret,
  domainOf,
  guessServers,
  matchProvider,
  portForSecurity,
  providerById,
  serversFor,
  type ServerPreset,
} from "./providers";

export const SETTINGS_VIEW = "maild.settings.page";
export const failure = (e: unknown) =>
  e instanceof Error ? e.message : m.settings_error_generic();
const copy = <T>(v: T): T => structuredClone(v);
export const newDraft = (): AccountDraft => ({
  display_name: "",
  address: "",
  incoming: {
    kind: "imap",
    server: "",
    port: 993,
    security: "implicit",
    username: "",
    mailbox: "INBOX",
    idle: true,
    poll_secs: 60,
  },
  outgoing: {
    kind: "smtp",
    server: "",
    port: 587,
    security: "starttls",
    username: "",
    credential: "incoming",
  },
});
/** 添加账户向导的界面状态；不参与「未保存」判断，也不含任何秘密。 */
export interface AccountWizard {
  /** 已选或已匹配的预设 id；`custom` 表示自定义 IMAP；null 表示尚未识别。 */
  provider: string | null;
  /** 用户亲手改过服务器 / 端口 / 加密方式：之后不再按地址自动覆盖。 */
  manual: boolean;
  /** 「服务器连接参数」面板是否展开。 */
  expanded: boolean;
  /** 用户亲手改过显示名称：之后不再跟随地址。 */
  nameTouched: boolean;
  secretNotice: string | null;
  discovering: boolean;
  discovered: string | null;
}
export const blankWizard = (): AccountWizard => ({
  provider: null,
  manual: false,
  expanded: false,
  nameTouched: false,
  secretNotice: null,
  discovering: false,
  discovered: null,
});
const SERVER_KEYS: readonly (keyof Endpoint)[] = ["server", "port", "security"];
export interface AddressForm {
  account: string;
  address: string;
  label: string;
  kind: "alias" | "disposable";
  days: string;
}
export interface SettingsState {
  section: string;
  online: boolean;
  loading: boolean;
  busy: boolean;
  closeLocked: boolean;
  error: string | null;
  notice: string | null;
  profiles: AccountProfile[] | null;
  packages: Packages | null;
  editor: AccountEdit | null;
  editorBaseline: string;
  probe: Probe | null;
  wizard: AccountWizard;
  rules: Rules | null;
  rulesBaseline: string;
  /** 最近一次读取 / 保存规则的持久化回执；null = 后台未说明。 */
  rulesPersisted: boolean | null;
  notification: NotificationStatus | null;
  addressAccount: string;
  addresses: AddressPage | null;
  addressOffset: number;
  addressForm: AddressForm | null;
  choice: PackageChoice | null;
  rollback: PackagePreview | null;
  confirm: {
    title: string;
    body: string;
    label: string;
    danger: boolean;
  } | null;
}
export class SettingsController {
  readonly store = new Store<SettingsState>({
    section: "maild.settings.accounts",
    online: false,
    loading: false,
    busy: false,
    closeLocked: false,
    error: null,
    notice: null,
    profiles: null,
    packages: null,
    editor: null,
    editorBaseline: "",
    probe: null,
    wizard: blankWizard(),
    rules: null,
    rulesBaseline: "",
    rulesPersisted: null,
    notification: null,
    addressAccount: "",
    addresses: null,
    addressOffset: 0,
    addressForm: null,
    choice: null,
    rollback: null,
    confirm: null,
  });
  private epoch = -1;
  private generation = 0;
  private query = 0;
  private action = 0;
  private confirmation: (() => void) | null = null;
  private confirmationCancel: (() => void) | null = null;
  private confirmationIsClose = false;
  constructor(
    readonly bridge: SettingsBridge,
    private readonly changed: () => void = () => {},
  ) {}
  get dirty() {
    const s = this.store.getSnapshot();
    return !!(
      (s.editor && JSON.stringify(s.editor) !== s.editorBaseline) ||
      (s.rules && JSON.stringify(s.rules) !== s.rulesBaseline) ||
      s.addressForm ||
      s.choice ||
      s.rollback
    );
  }
  connection(snapshot: UiSnapshot) {
    const s = this.store.getSnapshot(),
      online = snapshot.phase === "online";
    if (this.epoch === snapshot.connection_epoch && s.online === online) return;
    this.epoch = snapshot.connection_epoch;
    this.generation++;
    this.query++;
    this.action++;
    this.cancelConfirmation();
    this.store.patch({
      online,
      busy: false,
      loading: false,
      probe: null,
      choice: null,
      rollback: null,
      ...(s.editor ? { editor: { ...s.editor, secrets: {} } } : {}),
      ...(s.busy
        ? {
            error: m.settings_error_connection_changed(),
          }
        : {}),
    });
    if (s.choice)
      void this.bridge.cancelPackage(s.choice.token).catch(() => {});
  }
  activate(section: string) {
    this.store.patch({ section, error: null, notice: null });
    void this.load();
  }
  async load() {
    const s = this.store.getSnapshot();
    if (!s.online || s.busy || s.closeLocked) return;
    const query = ++this.query,
      generation = this.generation,
      section = s.section;
    this.store.patch({ loading: true, error: null });
    try {
      if (
        section === "maild.settings.accounts" ||
        section === "maild.settings.addresses" ||
        section === "maild.settings.extensions"
      ) {
        if (section === "maild.settings.extensions") {
          const packages = await this.bridge.packages();
          if (query !== this.query || generation !== this.generation) return;
          this.store.patch({ packages });
          try {
            const profiles = await this.bridge.profiles();
            if (query !== this.query || generation !== this.generation) return;
            this.store.patch({ profiles });
          } catch {
            if (query === this.query && generation === this.generation)
              this.store.patch({
                profiles: null,
                error: m.extensions_error_profiles_unavailable(),
              });
          }
          return;
        }
        const profiles = await this.bridge.profiles();
        if (query !== this.query || generation !== this.generation) return;
        this.store.patch({ profiles });
        if (section === "maild.settings.accounts") {
          const packages = await this.bridge.packages();
          if (query !== this.query || generation !== this.generation) return;
          this.store.patch({ packages });
        } else {
          const account = profiles.some((p) => p.id === s.addressAccount)
            ? s.addressAccount
            : (profiles[0]?.id ?? "");
          const addresses = account
            ? await this.bridge.addresses(account, 0)
            : { addresses: [], total: 0 };
          if (query !== this.query || generation !== this.generation) return;
          const form = this.store.getSnapshot().addressForm;
          if (form && form.account !== account) {
            this.store.patch({
              error: m.settings_address_error_account_gone(),
            });
            return;
          }
          this.store.patch({
            addressAccount: account,
            addresses,
            addressOffset: 0,
          });
        }
      } else if (section === "maild.settings.notifications") {
        const r = await this.bridge.rules();
        if (query !== this.query || generation !== this.generation) return;
        if (!this.dirty)
          this.store.patch({
            rules: r.rules,
            rulesBaseline: JSON.stringify(r.rules),
            rulesPersisted: r.persisted,
          });
        const notification = await this.bridge.notification();
        if (query !== this.query || generation !== this.generation) return;
        this.store.patch({ notification });
        // 「按账户静音」需要账户名单；读不到只影响这一块。
        try {
          const profiles = await this.bridge.profiles();
          if (query === this.query && generation === this.generation)
            this.store.patch({ profiles });
        } catch {
          /* 名单缺失时界面显示说明 */
        }
      }
    } catch (e) {
      if (query === this.query && generation === this.generation)
        this.store.patch({ error: failure(e) });
    } finally {
      if (query === this.query && generation === this.generation)
        this.store.patch({ loading: false });
    }
  }
  beginAccount(p?: AccountProfile, providerId?: string) {
    if (
      this.store.getSnapshot().busy ||
      this.store.getSnapshot().closeLocked ||
      (p && !p.editable)
    )
      return;
    const preset = p ? null : providerById(providerId);
    const editor: AccountEdit = p
      ? {
          id: p.id,
          revision: p.revision,
          draft: {
            display_name: p.display_name,
            address: p.address,
            incoming: copy(p.incoming),
            outgoing: copy(p.outgoing ?? null),
          },
          secrets: {},
        }
      : {
          id: null,
          revision: null,
          draft: preset
            ? applyServers(newDraft(), serversFor(preset, null))
            : newDraft(),
          secrets: {},
        };
    const wizard: AccountWizard = p
      ? {
          ...blankWizard(),
          provider: matchProvider(p.address)?.id ?? null,
          manual: true,
          expanded: true,
          nameTouched: true,
        }
      : {
          ...blankWizard(),
          provider:
            preset?.id ??
            (providerId === CUSTOM_PROVIDER ? CUSTOM_PROVIDER : null),
          expanded: providerId === CUSTOM_PROVIDER,
        };
    this.store.patch({
      editor,
      editorBaseline: JSON.stringify(editor),
      probe: null,
      wizard,
      error: null,
      notice: null,
    });
  }
  /** 从邮箱欢迎页 / 账户提醒进入：切到「账户」分区，直接打开（预选服务商的）添加向导。 */
  openSetup(providerId?: string) {
    const s = this.store.getSnapshot();
    if (s.busy || s.closeLocked) return;
    if (s.editor && this.dirty) {
      this.store.patch({ section: "maild.settings.accounts" });
      return;
    }
    this.store.patch({ section: "maild.settings.accounts", editor: null });
    this.beginAccount(undefined, providerId);
    void this.load();
  }
  /** 账户异常的「修复」入口：只切到账户列表，不打断正在编辑的内容。 */
  openAccounts() {
    const s = this.store.getSnapshot();
    if (s.busy || s.closeLocked) return;
    this.store.patch({ section: "maild.settings.accounts", error: null });
    void this.load();
  }
  private setWizard(patch: Partial<AccountWizard>) {
    this.store.patch({
      wizard: { ...this.store.getSnapshot().wizard, ...patch },
    });
  }
  /** 当前识别到的服务商预设（自定义或未识别时为 null）。 */
  get preset() {
    return providerById(this.store.getSnapshot().wizard.provider);
  }
  /** 快捷胶囊：选定服务商并套用其服务器参数。 */
  chooseProvider(id: string) {
    const s = this.store.getSnapshot(),
      e = s.editor;
    if (!e || e.id || s.busy || e.draft.incoming.kind !== "imap") return;
    const preset = providerById(id);
    if (!preset && id !== CUSTOM_PROVIDER) return;
    const domain = domainOf(e.draft.address);
    const servers = preset
      ? serversFor(
          preset,
          preset.domains.includes(domain ?? "") ? domain : null,
        )
      : null;
    this.editAccount({
      ...e,
      draft: servers ? applyServers(e.draft, servers) : e.draft,
    });
    this.setWizard({
      provider: preset?.id ?? CUSTOM_PROVIDER,
      manual: false,
      expanded: !preset,
      discovered: null,
    });
  }
  setDisplayName(display_name: string) {
    const e = this.store.getSnapshot().editor;
    if (!e) return;
    this.editAccount({ ...e, draft: { ...e.draft, display_name } });
    this.setWizard({ nameTouched: true });
  }
  /** 输入邮箱地址：用户名跟随；未手改服务器时按 @domain 自动套用预设。 */
  setAddress(address: string) {
    const s = this.store.getSnapshot(),
      e = s.editor;
    if (!e) return;
    const imap = e.draft.incoming.kind === "imap";
    let draft = { ...e.draft, address };
    let wizard = s.wizard;
    if (!e.id && imap) {
      draft = {
        ...draft,
        incoming: { ...draft.incoming, username: address },
        outgoing: draft.outgoing
          ? { ...draft.outgoing, username: address }
          : null,
      };
      if (!wizard.nameTouched) draft.display_name = address.trim();
      if (!wizard.manual) {
        const match = matchProvider(address);
        const preset = match ?? providerById(wizard.provider);
        if (match && match.id !== wizard.provider)
          wizard = {
            ...wizard,
            provider: match.id,
            expanded: false,
            discovered: null,
          };
        if (preset)
          draft = applyServers(draft, serversFor(preset, domainOf(address)));
      }
    } else if (!e.id && !wizard.nameTouched)
      draft.display_name = address.trim();
    this.editAccount({ ...e, draft });
    if (wizard !== s.wizard) this.store.patch({ wizard });
  }
  /**
   * 编辑收 / 发信参数。改服务器、端口或加密方式视为手工配置；
   * 切换 TLS ↔ STARTTLS 时，若端口仍是旧模式的标准端口则同步切换。
   */
  editEndpoint(side: "incoming" | "outgoing", patch: Partial<Endpoint>) {
    const e = this.store.getSnapshot().editor;
    const current = e?.draft[side];
    if (!e || !current) return;
    const next: Endpoint = { ...current, ...patch };
    if (
      patch.security !== undefined &&
      patch.security !== current.security &&
      patch.port === undefined &&
      (current.kind === "imap" || current.kind === "smtp")
    )
      next.port = portForSecurity(
        current.kind,
        current.port,
        current.security,
        patch.security,
      );
    this.editAccount({ ...e, draft: { ...e.draft, [side]: next } });
    if (SERVER_KEYS.some((key) => key in patch))
      this.setWizard({ manual: true });
  }
  /** 密码 / 授权码：对应用密码类服务商自动去掉分组空格并温和提示。 */
  setSecret(role: "incoming" | "outgoing", raw: string) {
    const e = this.store.getSnapshot().editor;
    if (!e) return;
    const preset = this.preset;
    const { value, changed } = cleanSecret(
      raw,
      preset?.stripSecretSpaces ?? false,
    );
    this.editAccount({ ...e, secrets: { ...e.secrets, [role]: value } });
    if (changed && preset)
      this.setWizard({
        secretNotice: m.settings_wizard_secret_spaces_removed({
          credential: CREDENTIAL_LABEL[preset.credential],
        }),
      });
  }
  toggleServers(open?: boolean) {
    const w = this.store.getSnapshot().wizard;
    this.setWizard({ expanded: open ?? !w.expanded });
  }
  /** 未知域名：优先后端发现，否则按 imap.<domain> / smtp.<domain> 预填并展开参数供核对。 */
  async discover() {
    const s = this.store.getSnapshot(),
      e = s.editor;
    if (
      !e ||
      s.busy ||
      s.wizard.discovering ||
      e.draft.incoming.kind !== "imap"
    )
      return;
    const address = e.draft.address.trim();
    const domain = domainOf(address);
    if (!domain) {
      this.store.patch({ error: m.settings_wizard_error_address_required() });
      return;
    }
    const known = matchProvider(address);
    if (known) {
      this.chooseProvider(known.id);
      return;
    }
    const generation = this.generation;
    this.setWizard({ discovering: true, discovered: null });
    let found: DiscoveredServers | null = null;
    if (this.bridge.discoverProvider && s.online)
      try {
        found = await this.bridge.discoverProvider(address);
      } catch {
        found = null;
      }
    const now = this.store.getSnapshot().editor;
    if (
      generation !== this.generation ||
      !now ||
      now.draft.address.trim() !== address
    ) {
      this.setWizard({ discovering: false });
      return;
    }
    const guess = guessServers(domain);
    const pick = (
      v: { server: string; port: number; security: string } | null | undefined,
      fallback: ServerPreset,
    ): ServerPreset =>
      v && v.server && Number.isInteger(v.port) && v.port > 0 && v.port <= 65535
        ? {
            server: v.server,
            port: v.port,
            security: v.security === "starttls" ? "starttls" : "implicit",
          }
        : fallback;
    const servers = found
      ? {
          incoming: pick(found.incoming, guess.incoming),
          outgoing: pick(found.outgoing, guess.outgoing),
        }
      : guess;
    this.editAccount({ ...now, draft: applyServers(now.draft, servers) });
    this.setWizard({
      provider: CUSTOM_PROVIDER,
      manual: true,
      expanded: true,
      discovering: false,
      discovered: found
        ? m.settings_wizard_discovered({
            server: servers.incoming.server,
            port: servers.incoming.port,
          })
        : m.settings_wizard_guessed({
            incoming: servers.incoming.server,
            outgoing: servers.outgoing.server,
          }),
    });
  }
  /** 打开服务商官方帮助页；宿主不支持时把链接复制给用户。 */
  async openHelp(url: string) {
    if (!/^https:\/\//.test(url)) return;
    try {
      if (!this.bridge.openExternalUrl) throw new Error("unsupported");
      await this.bridge.openExternalUrl(url);
    } catch {
      let copied = false;
      try {
        await globalThis.navigator?.clipboard?.writeText(url);
        copied = !!globalThis.navigator?.clipboard;
      } catch {
        copied = false;
      }
      this.store.patch({
        notice: copied
          ? m.settings_help_link_copied({ url })
          : m.settings_help_link_open_manually({ url }),
      });
    }
  }
  /** 提交前清理秘密，避免粘贴带出的空格让探测在「认证」阶段失败。 */
  private cleaned(e: AccountEdit): AccountEdit {
    const strip = this.preset?.stripSecretSpaces ?? false;
    const secrets: AccountEdit["secrets"] = {};
    if (e.secrets.incoming !== undefined)
      secrets.incoming = cleanSecret(e.secrets.incoming, strip).value;
    if (e.secrets.outgoing !== undefined)
      secrets.outgoing = cleanSecret(e.secrets.outgoing, strip).value;
    return { ...e, secrets };
  }
  editAccount(editor: AccountEdit) {
    const secrets: AccountEdit["secrets"] = {};
    if (editor.draft.incoming.kind === "imap" && editor.secrets.incoming)
      secrets.incoming = editor.secrets.incoming;
    if (
      editor.draft.outgoing?.kind === "smtp" &&
      editor.draft.outgoing.credential === "separate" &&
      editor.secrets.outgoing
    )
      secrets.outgoing = editor.secrets.outgoing;
    editor = { ...editor, secrets };
    if (!this.store.getSnapshot().busy && !this.store.getSnapshot().closeLocked)
      this.store.patch({ editor, probe: null, error: null });
  }
  setRules(rules: Rules) {
    if (!this.store.getSnapshot().busy && !this.store.getSnapshot().closeLocked)
      this.store.patch({ rules });
  }
  private async write<T>(
    work: () => Promise<T>,
    success: (value: T) => void,
  ): Promise<boolean> {
    const s = this.store.getSnapshot();
    if (!s.online || s.busy || s.closeLocked) return false;
    const generation = this.generation,
      action = ++this.action;
    this.query++;
    this.store.patch({ busy: true, loading: false, error: null, notice: null });
    try {
      const r = await work();
      if (generation !== this.generation || action !== this.action)
        return false;
      success(r);
      return true;
    } catch (e) {
      if (generation === this.generation && action === this.action)
        this.store.patch({ error: failure(e) });
      return false;
    } finally {
      if (generation === this.generation && action === this.action)
        this.store.patch({ busy: false });
    }
  }
  async probe() {
    const e = this.store.getSnapshot().editor;
    if (!e) return;
    await this.write(
      () => this.bridge.probe(copy(this.cleaned(e))),
      (probe) => {
        const serverFault = [probe.incoming, probe.outgoing].some(
          (p) => p && !p.ok && SERVER_STAGES.includes(p.stage),
        );
        this.store.patch({
          probe,
          ...(serverFault
            ? { wizard: { ...this.store.getSnapshot().wizard, expanded: true } }
            : {}),
        });
      },
    );
  }
  async saveAccount() {
    const s = this.store.getSnapshot(),
      e = s.editor;
    if (!e) return;
    if (
      !e.id &&
      (!s.probe?.incoming.ok || (e.draft.outgoing && !s.probe?.outgoing?.ok))
    ) {
      this.store.patch({ error: m.settings_account_error_probe_required() });
      return;
    }
    const ok = await this.write(
      () => this.bridge.saveAccount(copy(this.cleaned(e))),
      (p) => {
        // The mutation response is authoritative; a concurrent list read may still
        // contain the previous revision while the daemon refreshes its projection.
        const profiles = this.store.getSnapshot().profiles ?? [];
        this.store.patch({
          profiles: profiles.some((profile) => profile.id === p.id)
            ? profiles.map((profile) => (profile.id === p.id ? p : profile))
            : [...profiles, p],
          editor: null,
          probe: null,
          notice: m.settings_account_saved({ name: accountLabel(p) }),
        });
        this.changed();
      },
    );
    if (!ok) {
      const now = this.store.getSnapshot().editor;
      if (now === e)
        this.store.patch({ editor: { ...e, secrets: {} }, probe: null });
    }
  }
  confirmAccount(p: AccountProfile, remove: boolean) {
    this.ask(
      remove
        ? m.settings_account_confirm_remove_title({
            name: accountLabel(p),
          })
        : p.enabled
          ? m.settings_account_confirm_pause_title({
              name: accountLabel(p),
            })
          : m.settings_account_confirm_enable_title({
              name: accountLabel(p),
            }),
      `${p.address}\n${remove ? m.settings_account_confirm_remove_body() : m.settings_account_confirm_toggle_body()}`,
      remove
        ? m.settings_account_action_remove()
        : p.enabled
          ? m.settings_account_action_pause()
          : m.settings_account_action_enable(),
      remove,
      () => {
        void this.write(
          () =>
            this.bridge.accountAction(
              remove
                ? { action: "remove", id: p.id, revision: p.revision }
                : {
                    action: "enable",
                    id: p.id,
                    revision: p.revision,
                    enabled: !p.enabled,
                  },
            ),
          () => {
            this.store.patch({ notice: m.settings_account_updated() });
            this.changed();
          },
        ).then((ok) => {
          if (ok) void this.loadKeepingNotice();
        });
      },
    );
  }
  async saveRules() {
    const rules = this.store.getSnapshot().rules;
    if (!rules) return;
    const patch = {
      ...copy(rules),
      silent_keywords: rules.silent_keywords.filter((x) => x.length > 0),
    };
    await this.write(
      () => this.bridge.saveRules(patch),
      (r) =>
        this.store.patch({
          rules: r.rules,
          rulesBaseline: JSON.stringify(r.rules),
          rulesPersisted: r.persisted,
          notice: r.persisted
            ? m.settings_rules_saved()
            : m.settings_rules_applied_session(),
        }),
    );
  }
  async requestNotifications() {
    await this.write(
      () => this.bridge.notification(true),
      (notification) =>
        this.store.patch({
          notification,
          notice: m.settings_notifications_requested(),
        }),
    );
  }
  async selectAddressAccount(
    account: string,
    offset = 0,
    preserveForm = false,
  ) {
    const s = this.store.getSnapshot();
    if (s.busy || !s.online || s.closeLocked) return;
    const query = ++this.query,
      generation = this.generation;
    this.store.patch({
      loading: true,
      error: null,
      addressAccount: account,
      addressOffset: offset,
      addresses: null,
      addressForm:
        preserveForm && s.addressForm?.account === account
          ? s.addressForm
          : null,
    });
    try {
      const addresses = await this.bridge.addresses(account, offset);
      if (query === this.query && generation === this.generation)
        this.store.patch({ addresses });
    } catch (e) {
      if (query === this.query && generation === this.generation)
        this.store.patch({ error: failure(e) });
    } finally {
      if (query === this.query && generation === this.generation)
        this.store.patch({ loading: false });
    }
  }
  async createAddress() {
    const s = this.store.getSnapshot(),
      f = s.addressForm;
    if (!f) return;
    if (f.account !== s.addressAccount) {
      this.store.patch({ error: m.settings_address_error_account_changed() });
      return;
    }
    const p = s.profiles?.find((p) => p.id === f.account);
    if (p?.outgoing?.kind !== "smtp") {
      this.store.patch({ error: m.settings_address_error_primary_only() });
      return;
    }
    const days = f.days === "" ? null : Number(f.days);
    if (days !== null && (!Number.isInteger(days) || days < 1 || days > 365)) {
      this.store.patch({ error: m.settings_address_error_days() });
      return;
    }
    const ok = await this.write(
      () =>
        this.bridge.addressAction({
          action: "create",
          account: s.addressAccount,
          kind: f.kind,
          address: f.address.trim() || null,
          label: f.label,
          expires_secs:
            f.kind === "disposable" && days !== null ? days * 86400 : null,
        }),
      () => {
        this.store.patch({
          addressForm: null,
          notice: m.settings_address_registered(),
        });
        this.changed();
      },
    );
    if (ok) await this.selectAddressAccount(s.addressAccount);
  }
  confirmAddress(a: Address, remove: boolean) {
    this.ask(
      remove
        ? m.settings_address_confirm_remove_title()
        : a.state === "active"
          ? m.settings_address_confirm_disable_title()
          : m.settings_address_confirm_enable_title(),
      `${a.address}\n${m.settings_address_confirm_body()}`,
      remove
        ? m.settings_address_action_remove()
        : a.state === "active"
          ? m.settings_address_action_disable()
          : m.settings_address_action_enable(),
      remove,
      () => {
        void this.write(
          () =>
            this.bridge.addressAction(
              remove
                ? { action: "remove", id: a.id }
                : {
                    action: "update",
                    id: a.id,
                    state: a.state === "active" ? "disabled" : "active",
                  },
            ),
          () => this.changed(),
        ).then((ok) => {
          if (ok) void this.selectAddressAccount(a.account, 0, true);
        });
      },
    );
  }
  dependencies(id: string) {
    return (this.store.getSnapshot().profiles ?? [])
      .filter((p) => p.incoming.provider === id || p.outgoing?.provider === id)
      .map((p) => `${accountLabel(p)} (${p.address})`);
  }
  async pickPackage(kind: PackageKind) {
    let selected: PackageChoice | null = null;
    const accepted = await this.write(
      async () => {
        selected = await this.bridge.pickPackage(kind);
        return selected;
      },
      (choice) => this.store.patch({ choice }),
    );
    if (!accepted && selected)
      await this.bridge
        .cancelPackage((selected as PackageChoice).token)
        .catch(() => {});
  }
  async reconcilePackages() {
    const ok = await this.write(
      () => this.bridge.packageAction({ action: "refresh" }),
      () => this.store.patch({ notice: m.extensions_rescanned() }),
    );
    if (ok) await this.load();
  }
  async install() {
    const s = this.store.getSnapshot();
    if (!s.choice) return;
    const choice = s.choice;
    // The native token is consumed even if the transport fails. A second click must select again.
    const ok = await this.write(
      () => this.bridge.installPackage(choice.token),
      (r) =>
        this.store.patch({
          notice:
            r.action === "upgraded"
              ? m.extensions_upgraded({ id: r.id, version: r.version })
              : m.extensions_installed({ id: r.id, version: r.version }),
        }),
    );
    if (this.store.getSnapshot().choice === choice)
      this.store.patch({ choice: null });
    if (ok) {
      this.changed();
      await this.loadKeepingNotice();
    }
  }
  cancelPackage() {
    const c = this.store.getSnapshot().choice;
    this.store.patch({ choice: null, rollback: null });
    if (c) void this.bridge.cancelPackage(c.token).catch(() => {});
  }
  async previewRollback(kind: PackageKind, id: string) {
    await this.write(
      () => this.bridge.rollbackPreview(kind, id),
      (rollback) => this.store.patch({ rollback }),
    );
  }
  async rollback() {
    const p = this.store.getSnapshot().rollback;
    if (!p) return;
    const ok = await this.write(
      () => this.bridge.rollback(copy(p)),
      () =>
        this.store.patch({
          rollback: null,
          notice: m.extensions_rolled_back(),
        }),
    );
    if (ok) {
      this.changed();
      await this.loadKeepingNotice();
    }
  }
  confirmPackage(
    kind: PackageKind,
    id: string,
    action: "remove" | "enable" | "disable" | "default",
  ) {
    const dependencies = this.dependencies(id);
    const independent =
      kind === "plugin" &&
      this.store.getSnapshot().packages?.plugins.find((p) => p.id === id)
        ?.lifecycle === "independent";
    const effect =
      action === "remove"
        ? m.extensions_confirm_remove_body()
        : action === "default"
          ? m.extensions_confirm_default_body()
          : action === "disable"
            ? independent
              ? m.extensions_confirm_disable_independent_body()
              : m.extensions_confirm_disable_body()
            : independent
              ? m.extensions_confirm_enable_independent_body()
              : m.extensions_confirm_enable_body();
    const body = `${id}\n${this.store.getSnapshot().profiles === null ? `${m.extensions_confirm_profiles_unknown()}\n` : ""}${dependencies.length ? `${m.extensions_confirm_dependencies({ accounts: dependencies.join(m.extensions_list_separator()) })}\n` : ""}${effect}`;
    this.ask(
      action === "default"
        ? m.extensions_confirm_default_title()
        : action === "remove"
          ? m.extensions_confirm_remove_title()
          : action === "disable"
            ? m.extensions_confirm_disable_title()
            : m.extensions_confirm_enable_title(),
      body,
      action === "default"
        ? m.extensions_action_make_default()
        : action === "remove"
          ? m.extensions_action_remove()
          : action === "disable"
            ? m.extensions_action_disable()
            : m.extensions_action_enable(),
      action === "remove" || action === "disable",
      () => {
        void this.write(
          () =>
            this.bridge.packageAction(
              action === "remove"
                ? { action, kind, id }
                : action === "default"
                  ? { action, id }
                  : { action: "enable", id, enabled: action === "enable" },
            ),
          () => this.store.patch({ notice: m.extensions_settings_updated() }),
        ).then((ok) => {
          if (ok) {
            this.changed();
            void this.loadKeepingNotice();
          }
        });
      },
    );
  }
  async openProvider(id: string) {
    await this.write(
      () => this.bridge.packageAction({ action: "open", id }),
      () =>
        this.store.patch({ notice: m.extensions_provider_open_requested() }),
    );
  }
  ask(
    title: string,
    body: string,
    label: string,
    danger: boolean,
    run: () => void,
    onCancel: () => void = () => {},
    forClose = false,
  ) {
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closeLocked) {
      onCancel();
      return;
    }
    this.cancelConfirmation();
    this.confirmation = run;
    this.confirmationCancel = onCancel;
    this.confirmationIsClose = forClose;
    this.store.patch({ confirm: { title, body, label, danger } });
  }
  confirm() {
    const run = this.confirmation;
    this.confirmation = null;
    this.confirmationCancel = null;
    this.confirmationIsClose = false;
    this.store.patch({ confirm: null });
    run?.();
  }
  cancelConfirmation() {
    const cancel = this.confirmationCancel;
    this.confirmation = null;
    this.confirmationCancel = null;
    this.confirmationIsClose = false;
    this.store.patch({ confirm: null });
    cancel?.();
  }
  requestLeave(
    next: () => void,
    onCancel: () => void = () => {},
    forClose = false,
  ) {
    if (this.confirmationIsClose && !forClose) {
      onCancel();
      return false;
    }
    if (this.store.getSnapshot().busy || this.store.getSnapshot().closeLocked) {
      this.store.patch({ error: m.settings_leave_error_busy() });
      onCancel();
      return false;
    }
    if (!this.dirty) {
      this.discard();
      if (forClose) this.store.patch({ closeLocked: true });
      return true;
    }
    this.ask(
      m.settings_leave_confirm_title(),
      m.settings_leave_confirm_body(),
      m.settings_leave_confirm_action(),
      true,
      () => {
        this.discard();
        if (forClose) this.store.patch({ closeLocked: true });
        next();
      },
      onCancel,
      forClose,
    );
    return false;
  }
  beforeClose(): Promise<boolean> {
    return new Promise((resolve) => {
      if (
        this.requestLeave(
          () => resolve(true),
          () => resolve(false),
          true,
        )
      )
        resolve(true);
    });
  }
  releaseClose() {
    if (this.store.getSnapshot().closeLocked)
      this.store.patch({ closeLocked: false });
  }
  discard() {
    this.cancelPackage();
    const s = this.store.getSnapshot();
    this.store.patch({
      editor: null,
      probe: null,
      wizard: blankWizard(),
      addressForm: null,
      ...(s.rulesBaseline
        ? { rules: JSON.parse(s.rulesBaseline) as Rules }
        : {}),
      error: null,
    });
  }
  private async loadKeepingNotice() {
    await this.load();
  }
  dispose() {
    this.generation++;
    this.query++;
    this.cancelConfirmation();
    this.discard();
  }
}
