import { lifecycleLabel, permissionLabel } from "../i18n";
import { useEffect, useState } from "react";
import { SettingsController } from "./controller";
import { Accounts } from "./Accounts";
import { Addresses } from "./Addresses";
import { Notifications } from "./Notifications";
import { Extensions as ManagedExtensions } from "./Extensions";
import { Dialog } from "../mailbox/Dialog";

import type { Host } from "../host/host";
import { useExternal, useStore } from "../host/hooks";
import type { Bundle, ViewProps } from "../host/types";
import type { Preferences } from "../preferences";
import { BRAND } from "../brand";
import { Choice } from "./Choice";

export const SETTINGS_VIEW = "maild.settings.page";


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
  function Appearance(_props: ViewProps) {
    const { preferences } = useStore(host.state);
    const update = (patch: Partial<Preferences>) =>
      host.setPreferences({ ...preferences, ...patch });
    return (
      <>
        <h2>外观</h2>
        <p className="page-intro">
          只影响这个窗口；邮件原文保留发件人自己的版式与颜色。
        </p>
        <Row
          title="主题"
          description="跟随系统，或固定浅色 / 深色。深色外壳下邮件仍保持原色。"
        >
          <Choice
            aria-label="主题"
            value={preferences.theme}
            onValueChange={(value) =>
              update({ theme: value as Preferences["theme"] })
            }
          >
            <option value="system">跟随系统</option>
            <option value="light">浅色</option>
            <option value="dark">深色</option>
          </Choice>
        </Row>
        <Row
          title="列表密度"
          description="舒适 90px 一行，紧凑 68px 并隐藏摘要。"
        >
          <Choice
            aria-label="列表密度"
            value={preferences.density}
            onValueChange={(value) =>
              update({ density: value as Preferences["density"] })
            }
          >
            <option value="comfortable">舒适</option>
            <option value="compact">紧凑</option>
          </Choice>
        </Row>
      </>
    );
  }

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
              description={`${frontendInfo.id} · ${frontendInfo.version} · ${lifecycleLabel(frontendInfo.lifecycle)}`}
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
                  title={permissionLabel(word)}
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
      void controller.load();
    }, []);
    const navigate = (id: string) => {
      const next = () => controller.activate(id);
      if (controller.requestLeave(next)) next();
    };
    const Section = active?.render;
    return (
      <div className="page settings-page">
        <div className="page-toolbar" data-tauri-drag-region="deep">
          <button
            type="button"
            className="text-button"
            onClick={() => host.openRole("mailbox")}
          >
            返回邮件
          </button>
          <h1>设置</h1>
          <button
            type="button"
            className="text-button"
            disabled={
              state.busy ||
              state.loading ||
              state.closeLocked ||
              !state.online ||
              controller.dirty
            }
            onClick={() =>
              void (state.section === "maild.settings.extensions"
                ? controller.reconcilePackages()
                : controller.load())
            }
          >
            刷新
          </button>
          <small>⌘, 直达</small>
        </div>
        <div className="settings-layout">
          <nav className="settings-nav" aria-label="设置分区">
            {sections.map((section) => (
              <button
                key={section.id}
                type="button"
                className={section.id === active?.id ? "active" : ""}
                disabled={state.closeLocked}
                aria-current={section.id === active?.id ? "page" : undefined}
                onClick={() => navigate(section.id)}
              >
                {section.title}
              </button>
            ))}
          </nav>
          <div className="settings-content">
            <div className="page-column">
              {!state.online && (
                <p className="settings-alert" role="status">
                  后台未连接。已有资料可查看，管理操作暂不可用。
                </p>
              )}
              {state.error && (
                <p className="settings-alert" role="alert">
                  {state.error}
                </p>
              )}
              {state.notice && (
                <p className="settings-alert settings-notice" role="status">
                  {state.notice}
                </p>
              )}
              {(state.loading || state.busy) && (
                <p className="settings-progress" role="status">
                  {state.busy ? "操作进行中…" : "正在读取…"}
                </p>
              )}
              <fieldset disabled={state.closeLocked}>
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
        render: Appearance,
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
