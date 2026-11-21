import { useEffect, useState } from "react";
import { SettingsController } from "../../../maild-ui/src/settings/controller";
import { Accounts } from "./Accounts";
import { Addresses } from "../../../maild-ui/src/settings/Addresses";
import { Notifications } from "../../../maild-ui/src/settings/Notifications";
import { Extensions as ManagedExtensions } from "../../../maild-ui/src/settings/Extensions";
import { Dialog } from "../../../maild-ui/src/mailbox/Dialog";

import type { Host } from "../../../maild-ui/src/host/host";
import { useExternal, useStore } from "../../../maild-ui/src/host/hooks";
import type { Bundle, ViewProps } from "../../../maild-ui/src/host/types";
import { Appearance } from "./Appearance";
import { Icon } from "../../../maild-ui/src/app/Icon";
import { BRAND } from "../../../maild-ui/src/brand";

export const SETTINGS_VIEW = "maild.settings.page";

const PERMISSION_LABELS: Record<string, string> = {
  messages_read: "读取邮件与账户状态",
  messages_send: "发送邮件",
  settings_read: "读取应用设置",
  settings_write: "修改应用设置",
  frontend_open: "接收导航意图、读取前端清单",
  mailbox_write: "已读 / 星标 / 归档 / 删除与离线队列裁决",
  accounts_manage: "账户中心",
  addresses_manage: "别名与一次性地址",
  plugins_manage: "安装、升级、回滚、启停受管包",
  notifications_read: "读取系统通知授权状态",
};

function Row({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="setting-row">
      <div className="setting-copy">
        <strong>{title}</strong>
        {description && <p>{description}</p>}
      </div>
      {children}
    </div>
  );
}

export function settingsModule(
  host: Host,
  controller = new SettingsController(host.bridge.settings),
): Bundle {
  function Connection(_props: ViewProps) {
    const { snapshot, frontendInfo, frontendInfoError } = useStore(host.state);
    const [opening, setOpening] = useState(false);
    const [recoveryError, setRecoveryError] = useState<string | null>(null);
    async function openRecovery() {
      if (opening || snapshot.phase !== "online") return;
      setOpening(true);
      setRecoveryError(null);
      try { await host.bridge.settings.packageAction({ action: "recovery" }); }
      catch { setRecoveryError(`修复入口未能打开，请从完整 ${BRAND.appBundleName} 打开修复与后台管理。`); }
      finally { setOpening(false); }
    }
    return (
      <>
        <h2>后台与存储</h2>
        <p className="page-intro">
          关闭窗口后，独立后台可继续收信。账户与邮件保存在本机，凭据由系统凭据库管理。
        </p>
        <p className="group-note">
          备份与恢复需要独立维护流程：停止相关进程、验证候选库并保留回滚记录。此页不提供会直接覆盖邮件库的开关。
        </p>
        <p className="page-intro">
          这个窗口是一个可替换的前端包，以自己的身份连接后台；它能做什么由下面的授权决定，第三方前端走同一张表。
        </p>
        <Row
          title="后台"
          description={
            snapshot.version ? `maild ${snapshot.version}` : "尚未取得版本"
          }
        >
          <span className={`tag${snapshot.phase === "online" ? "" : " warn"}`}>
            {phaseLabel(snapshot.phase)}
          </span>
        </Row>
        <Row
          title="会话主体"
          description={
            snapshot.principal ?? snapshot.identity_failure ?? "未绑定"
          }
        >
          <span className={`tag${snapshot.principal ? "" : " warn"}`}>
            {snapshot.principal ? "前端主体" : "未绑定"}
          </span>
        </Row>
        <Row title="修复与后台管理" description={snapshot.phase === "online"
          ? "在独立原生窗口恢复内置前端、启动或停止后台。"
          : `后台未连接，请打开 ${BRAND.appBundleName} 的修复入口。`}>
          <button type="button" disabled={opening || snapshot.phase !== "online"} onClick={() => void openRecovery()}>
            {opening ? "正在打开…" : "打开修复窗口"}
          </button>
        </Row>
        {recoveryError && <p className="group-note" role="alert">{recoveryError}</p>}
        {frontendInfo && (
          <>
            <Row
              title={frontendInfo.name}
              description={`${frontendInfo.id} · ${frontendInfo.version} · ${frontendInfo.lifecycle}`}
            >
              <span className="tag">
                {frontendInfo.is_default
                  ? "默认前端"
                  : `默认：${frontendInfo.default_frontend}`}
              </span>
            </Row>
            <div className="section-label">已授权的功能</div>
            {[
              ...frontendInfo.permissions,
              ...frontendInfo.capabilities.flatMap(routeToWords),
            ]
              .filter((word, index, all) => all.indexOf(word) === index)
              .sort()
              .map((word) => (
                <Row
                  key={word}
                  title={PERMISSION_LABELS[word] ?? word}
                  description={word}
                />
              ))}
          </>
        )}
        {frontendInfoError && <p className="group-note">{frontendInfoError}</p>}
      </>
    );
  }

  function Extensions(_props: ViewProps) {
    const registry = useExternal(
      host.registry.subscribe,
      host.registry.getSnapshot,
    );
    const { diagnostics } = useStore(host.state);
    // 已注册的模块从注册表来（含贡献数）；停用的可选模块从宿主来（只有名字）。
    const rows = [
      ...registry.modules,
      ...host
        .optionalModules()
        .filter((module) => !module.enabled)
        .map((module) => ({
          moduleId: module.moduleId,
          title: module.title,
          builtin: false,
          contributions: 0,
        })),
    ].sort(
      (a, b) =>
        Number(b.builtin) - Number(a.builtin) ||
        a.moduleId.localeCompare(b.moduleId),
    );
    return (
      <>
        <p className="group-note">
          停用示例模块会撤走它的按钮、工作页与设置分区，不影响后台服务包。
        </p>
        {rows.map((module) => {
          const enabled =
            host.isModuleEnabled(module.moduleId) || module.builtin;
          return (
            <Row
              key={module.moduleId}
              title={module.title}
              description={`${module.moduleId}${enabled ? ` · ${module.contributions} 项贡献` : ""}`}
            >
              {module.builtin ? (
                <span className="tag">内置</span>
              ) : (
                <button
                  type="button"
                  role="switch"
                  aria-checked={enabled}
                  aria-label={`启用 ${module.title}`}
                  className="toggle"
                  onClick={() =>
                    host.setModuleEnabled(module.moduleId, !enabled)
                  }
                >
                  <span className="toggle-track" />
                </button>
              )}
            </Row>
          );
        })}
        {diagnostics.length > 0 && (
          <>
            <div className="section-label">诊断</div>
            {diagnostics.map((line, index) => (
              <p key={index} className="group-note">
                {line}
              </p>
            ))}
          </>
        )}
      </>
    );
  }

  function Page(_props: ViewProps) {
    const registry = useExternal(
      host.registry.subscribe,
      host.registry.getSnapshot,
    );
    const sections = registry.settings.map(({ value }) => value);
    const state = useStore(controller.store);
    const active =
      sections.find((section) => section.id === state.section) ?? sections[0];
    useEffect(() => {
      document.body.dataset.studioSettings = "true";
      void controller.load();
      return () => { delete document.body.dataset.studioSettings; };
    }, []);
    const navigate = (id: string) => {
      const next = () => controller.activate(id);
      if (controller.requestLeave(next)) next();
    };
    const Section = active?.render;
    const accounts = active?.id === "maild.settings.accounts";
    const refreshDisabled = state.busy || state.loading || state.closeLocked || !state.online || controller.dirty;
    const refresh = () => void (state.section === "maild.settings.extensions" ? controller.reconcilePackages() : controller.load());
    const descriptions: Record<string, string> = {
      "maild.settings.accounts": "管理账户资料、认证信息与收发连接",
      "maild.settings.appearance": "主题同时应用于界面与邮件阅读区",
      "maild.settings.notifications": "决定何时提醒，以及通知中显示多少内容",
      "maild.settings.addresses": "管理各个邮箱的本地发件身份",
      "maild.settings.extensions": "管理服务扩展、前端与当前窗口的模块",
      "maild.settings.connection": "查看后台连接、本机存储与前端授权",
    };
    return (
      <div className="page settings-page studio-settings">
        <header className="studio-settings-topbar" data-tauri-drag-region>
          <div className="studio-brand" data-tauri-drag-region>
            <svg className="brand-feather" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <path d="M20 3C11 1 4 8 5 17l-2 4 5-2C17 20 22 12 20 3ZM5 19 16 7M9 15v-5M9 15h5" />
            </svg>
            <strong data-tauri-drag-region>Featherpost</strong>
          </div>
          <span className="studio-settings-location" data-tauri-drag-region>设置</span>
          <button type="button" className="secondary studio-return" onClick={() => host.openRole("mailbox")}>
            <Icon name="back" />返回邮件
          </button>
        </header>
        <div className="settings-layout">
          <nav className="settings-nav" aria-label="设置分区">
            <h1>设置</h1>
            {[
              { label: "邮箱与偏好", rows: sections.filter(section => (section.order ?? 0) < 70) },
              { label: "应用", rows: sections.filter(section => (section.order ?? 0) >= 70) },
            ].map(group => group.rows.length > 0 && (
              <div className="studio-settings-nav-group" key={group.label}>
                <p>{group.label}</p>
                {group.rows.map(section => (
                  <button key={section.id} type="button" className={section.id === active?.id ? "active" : ""}
                    disabled={state.closeLocked} aria-current={section.id === active?.id ? "page" : undefined}
                    onClick={() => navigate(section.id)}>
                    <span>{section.title}</span>
                    {section.id === "maild.settings.accounts" && state.profiles !== null && <small>{state.profiles.length}</small>}
                  </button>
                ))}
              </div>
            ))}
            <div className="studio-settings-nav-footer">
              <strong>Featherpost Studio</strong>
              <span><i className={state.online ? "status-dot" : "status-dot warn"} />后台{state.online ? "已连接" : "未连接"}</span>
            </div>
          </nav>
          <div className="settings-content">
            <div className="page-column">
              <div className="studio-settings-heading">
                <div>
                  <div className="studio-settings-title"><h2>{active?.title ?? "设置"}</h2>
                    {accounts && state.profiles !== null && <span>{state.profiles.length} 个邮箱</span>}
                  </div>
                  {active && descriptions[active.id] && <p>{descriptions[active.id]}</p>}
                </div>
                {accounts ? (
                  <button type="button" className="primary" disabled={!state.online || state.busy || state.loading || state.closeLocked || state.profiles === null}
                    onClick={() => { const next = () => controller.beginAccount(); if (controller.requestLeave(next)) next(); }}>
                    <span aria-hidden="true">＋</span> 添加邮箱账户
                  </button>
                ) : active?.id === "maild.settings.appearance" ? (
                  <div className="studio-heading-actions"><span className="studio-instant">✓ 更改即时生效</span>
                    <button type="button" className="text-button" aria-label="刷新设置" title="刷新设置" disabled={refreshDisabled} onClick={refresh}><Icon name="refresh" /></button>
                  </div>
                ) : (
                  <button type="button" className="text-button" disabled={refreshDisabled} onClick={refresh}><Icon name="refresh" />刷新</button>
                )}
              </div>
              {!state.online && <p className="settings-alert" role="status">后台未连接。已有资料可查看，管理操作暂不可用。</p>}
              {state.error && <p className="settings-alert" role="alert">{state.error}</p>}
              {state.notice && <p className="settings-alert settings-notice" role="status">{state.notice}</p>}
              {(state.loading || state.busy) && <p className="settings-progress" role="status">{state.busy ? "操作进行中…" : "正在读取…"}</p>}
              <fieldset disabled={state.closeLocked} className={accounts || active?.id === "maild.settings.appearance" ? "studio-section-bare" : "studio-settings-section"}>
                {Section && <Section active />}
              </fieldset>
            </div>
          </div>
        </div>
        {state.confirm && (
          <Dialog
            title={state.confirm.title}
            onClose={() => controller.cancelConfirmation()}
          >
            <p className="settings-confirm-copy">{state.confirm.body}</p>
            <div className="settings-actions">
              <button
                className="secondary"
                onClick={() => controller.cancelConfirmation()}
              >
                取消
              </button>
              <button
                className={
                  state.confirm.danger ? "secondary danger" : "primary"
                }
                onClick={() => controller.confirm()}
              >
                {state.confirm.label}
              </button>
            </div>
          </Dialog>
        )}
      </div>
    );
  }

  const stopState = host.state.subscribe(() =>
    controller.connection(host.state.getSnapshot().snapshot),
  );
  controller.connection(host.state.getSnapshot().snapshot);
  const stopGuard = host.beforeNavigate((_id, next, cancel) =>
    host.state.getSnapshot().activeViewId === SETTINGS_VIEW
      ? controller.requestLeave(next, cancel)
      : true,
  );
  const stopIntent = host.onIntent((intent) => {
    if (intent.route === "settings") host.openView(SETTINGS_VIEW);
  });

  return {
    moduleId: "maild.settings",
    apiVersion: 1,
    title: "设置",
    views: [
      {
        id: SETTINGS_VIEW,
        title: "设置",
        icon: "settings",
        role: "settings",
        order: 1000,
        render: Page,
      },
    ],
    settings: [
      {
        id: "maild.settings.accounts",
        title: "账户",
        order: 0,
        render: () => <Accounts controller={controller} />,
      },
      {
        id: "maild.settings.notifications",
        title: "通知与规则",
        order: 50,
        render: () => <Notifications controller={controller} />,
      },
      {
        id: "maild.settings.addresses",
        title: "地址与别名",
        order: 60,
        render: () => <Addresses controller={controller} />,
      },
      {
        id: "maild.settings.appearance",
        title: "外观与操作",
        order: 10,
        render: () => <Appearance host={host} />,
      },
      {
        id: "maild.settings.connection",
        title: "后台与存储",
        order: 90,
        render: Connection,
      },
      {
        id: "maild.settings.extensions",
        title: "扩展",
        order: 70,
        render: () => (
          <ManagedExtensions controller={controller}>
            <Extensions active />
          </ManagedExtensions>
        ),
      },
    ],
    dispose: () => {
      stopIntent();
      stopGuard();
      stopState();
      controller.dispose();
    },
  };
}

function routeToWords(route: string): string[] {
  switch (route) {
    case "inbox":
    case "message":
      return ["messages_read"];
    case "compose":
      return ["messages_send"];
    case "settings":
      return ["settings_read", "settings_write"];
    default:
      return [];
  }
}

export function phaseLabel(phase: string): string {
  switch (phase) {
    case "online":
      return "已连接";
    case "connecting":
      return "连接中";
    case "synchronizing":
      return "同步中";
    case "retry_waiting":
      return "等待重连";
    case "stopped":
      return "已停止";
    default:
      return "未连接";
  }
}
