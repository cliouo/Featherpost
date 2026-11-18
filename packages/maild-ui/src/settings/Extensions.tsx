import { useState } from "react";
import { useStore } from "../host/hooks";
import type { SettingsController } from "./controller";
import type { PackageKind, PackagePreview } from "./types";
import { BRAND } from "../brand";
export const permissions: Record<string, string> = {
  messages_read: "读取邮件",
  messages_send: "发送邮件",
  drafts_read: "读取草稿",
  drafts_write: "编辑草稿",
  mailbox_write: "修改邮件与同步队列",
  accounts_manage: "管理账户与凭据",
  addresses_manage: "管理发件身份",
  plugins_manage: "安装和管理其他扩展",
  settings_read: "读取设置",
  settings_write: "修改设置",
  notifications_read: "读取通知授权",
  frontend_open: "激活前端",
  transport_provider: "提供邮件收发服务",
  events_stream: "订阅事件",
  inbox: "收件箱（读取邮件）",
  message: "阅读邮件（读取邮件）",
  compose: "写信（发送邮件）",
  settings: "设置（读写设置）",
  background: "后台入口",
};
export function PermissionList({ values }: { values: string[] }) {
  return (
    <ul className="permission-list">
      {values.length ? (
        values.map((v) => (
          <li key={v}>
            <strong>{permissions[v] ?? v}</strong>
            <code>{v}</code>
          </li>
        ))
      ) : (
        <li>未声明邮件 API 权限</li>
      )}
    </ul>
  );
}
function Preview({ p }: { p: PackagePreview }) {
  return (
    <>
      <h3>{p.name}</h3>
      <p className="group-note">
        {p.id} · {p.version} · {p.kind === "frontend" ? "前端" : "服务扩展"} ·{" "}
        {p.lifecycle === "supervised"
          ? "由后台托管运行"
          : p.lifecycle === "on_demand"
            ? "按需启动"
            : "独立运行"}
      </p>
      <h4>将获得的权限</h4>
      <PermissionList
        values={[...new Set([...p.capabilities, ...p.permissions])]}
      />
      <p className="group-note">
        这是本机可执行包，请只安装信任来源。上面的权限约束邮件
        API，并不等同于操作系统沙箱。
      </p>
    </>
  );
}
export function Extensions({
  controller,
  children,
}: {
  controller: SettingsController;
  children?: React.ReactNode;
}) {
  const s = useStore(controller.store),
    [kind, setKind] = useState<PackageKind>("plugin"),
    catalog = s.packages;
  return (
    <>
      <h2>扩展</h2>
      <p className="page-intro">需要时扩展，平时专注邮件。</p>
      {s.choice ? (
        <section className="package-preview">
          <div className="section-label">安装确认</div>
          <Preview p={s.choice.preview} />
          <p className="package-source">来源：{s.choice.source}</p>
          {controller.dependencies(s.choice.preview.id).length > 0 && (
            <p className="group-note">
              关联账户：
              {controller.dependencies(s.choice.preview.id).join("、")}
            </p>
          )}
          <div className="settings-actions">
            <button
              className="primary"
              disabled={s.busy || !s.online}
              onClick={() => void controller.install()}
            >
              确认权限并安装
            </button>
            <button
              className="secondary"
              disabled={s.busy}
              onClick={() => controller.cancelPackage()}
            >
              取消
            </button>
          </div>
        </section>
      ) : s.rollback ? (
        <section className="package-preview">
          <div className="section-label">回滚确认</div>
          <Preview p={s.rollback} />
          <p>当前版本与回滚版本将互换。再次回滚可以换回本次版本。</p>
          <div className="settings-actions">
            <button
              className="primary"
              disabled={s.busy || !s.online}
              onClick={() => void controller.rollback()}
            >
              确认权限并回滚
            </button>
            <button
              className="secondary"
              disabled={s.busy}
              onClick={() => controller.cancelPackage()}
            >
              取消
            </button>
          </div>
        </section>
      ) : (
        <>
          <h3>前端</h3>
          {catalog?.frontends.map((p) => (
            <article className="managed-package" key={p.id}>
              <div className="setting-row">
                <div className="setting-copy">
                  <strong>{p.name}</strong>
                  <p>
                    {p.id} · {p.version}
                  </p>
                </div>
                {catalog.default === p.id && <span className="tag">默认</span>}
                {p.id === BRAND.bundledFrontendId && (
                  <span className="tag">当前窗口</span>
                )}
              </div>
              <details>
                <summary>权限与操作</summary>
                <PermissionList
                  values={[...new Set([...p.capabilities, ...p.permissions])]}
                />
                <div className="settings-actions">
                  <button
                    className="secondary"
                    disabled={s.busy || !s.online || catalog.default === p.id}
                    onClick={() =>
                      controller.confirmPackage("frontend", p.id, "default")
                    }
                  >
                    设为默认
                  </button>
                  {!p.builtin && (
                    <>
                      <button
                        className="secondary"
                        disabled={s.busy || !s.online}
                        onClick={() =>
                          void controller.previewRollback("frontend", p.id)
                        }
                      >
                        查看回滚版本
                      </button>
                      <button
                        className="text-button danger"
                        disabled={
                          s.busy ||
                          !s.online ||
                          catalog.default === p.id ||
                          p.id === BRAND.bundledFrontendId
                        }
                        onClick={() =>
                          controller.confirmPackage("frontend", p.id, "remove")
                        }
                      >
                        移除
                      </button>
                    </>
                  )}
                </div>
              </details>
            </article>
          ))}
          <h3>邮箱服务与后台扩展</h3>
          {catalog?.plugins.length === 0 && (
            <p className="group-note">
              尚未安装服务扩展。普通 IMAP / SMTP 账户无需扩展。
            </p>
          )}
          {catalog?.plugins.map((p) => (
            <article className="managed-package" key={p.id}>
              <div className="setting-row">
                <div className="setting-copy">
                  <strong>{p.name}</strong>
                  <p>
                    {p.id} · {p.version} · {pluginState(p.state)}
                  </p>
                  {controller.dependencies(p.id).length > 0 && (
                    <p>关联账户：{controller.dependencies(p.id).join("、")}</p>
                  )}
                </div>
                <button
                  className="toggle"
                  type="button"
                  role="switch"
                  aria-checked={p.enabled}
                  aria-label={`启用 ${p.name}`}
                  disabled={s.busy || !s.online}
                  onClick={() =>
                    controller.confirmPackage(
                      "plugin",
                      p.id,
                      p.enabled ? "disable" : "enable",
                    )
                  }
                >
                  <span className="toggle-track" />
                </button>
              </div>
              <details>
                <summary>权限与操作</summary>
                <PermissionList values={p.capabilities} />
                <div className="settings-actions">
                  {p.has_ui && (
                    <button
                      className="secondary"
                      disabled={s.busy || !s.online}
                      onClick={() => void controller.openProvider(p.id)}
                    >
                      打开扩展管理
                    </button>
                  )}
                  <button
                    className="secondary"
                    disabled={s.busy || !s.online}
                    onClick={() =>
                      void controller.previewRollback("plugin", p.id)
                    }
                  >
                    查看回滚版本
                  </button>
                  <button
                    className="text-button danger"
                    disabled={s.busy || !s.online}
                    onClick={() =>
                      controller.confirmPackage("plugin", p.id, "remove")
                    }
                  >
                    移除
                  </button>
                </div>
              </details>
            </article>
          ))}
          <div className="settings-actions">
            <select
              aria-label="安装包类型"
              value={kind}
              onChange={(e) => setKind(e.target.value as PackageKind)}
              disabled={s.busy}
            >
              <option value="plugin">服务扩展包</option>
              <option value="frontend">前端包</option>
            </select>
            <button
              className="secondary"
              disabled={!s.online || s.busy || s.loading || !catalog}
              onClick={() => void controller.pickPackage(kind)}
            >
              从本地目录安装 / 升级
            </button>
          </div>
          <p className="group-note">
            选择包含 plugin.toml 或 frontend.toml
            的完整目录。安装前先校验并显示授权；升级使用同一个入口。
          </p>
          <div className="settings-divider" />
          <h3>当前窗口的界面模块</h3>
          {children}
          <p className="group-note">
            内置模块经统一贡献注册表装配。外部包的界面面板与独立窗口将在 E02/E03
            接入。
          </p>
        </>
      )}
    </>
  );
}
function pluginState(state: string) {
  return (
    (
      {
        running: "运行中",
        disabled: "已停用",
        stopped: "已停止",
        independent: "独立运行",
        blocked: "启动受阻",
        backoff: "等待重试",
        starting: "启动中",
      } as Record<string, string>
    )[state] ?? state
  );
}
