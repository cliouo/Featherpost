import { Icon } from "../app/Icon";
import { CUSTOM_PROVIDER, QUICK_PROVIDERS, providerById } from "../settings/providers";

/** 零账户首次进入：主区域的「连接你的第一个邮箱」引导卡。 */
export function Welcome({ onSetup }: { onSetup: (provider?: string) => void }) {
  return (
    <section className="welcome" aria-labelledby="welcome-title">
      <div className="welcome-card">
        <Icon name="mail" />
        <h1 id="welcome-title">连接你的第一个邮箱</h1>
        <p className="welcome-lead">
          选择你的邮箱服务商，向导会自动填好服务器与端口；粘贴授权码或应用专用密码，通常十秒内即可完成。
        </p>
        <div className="welcome-providers" role="group" aria-label="选择邮箱服务商">
          {QUICK_PROVIDERS.filter((id) => id !== CUSTOM_PROVIDER).map((id) => {
            const preset = providerById(id)!;
            return (
              <button key={id} type="button" className="welcome-provider" onClick={() => onSetup(id)}>
                <strong>{preset.short}</strong>
                <small>{preset.domains.slice(0, 2).join(" · ")}</small>
              </button>
            );
          })}
          <button type="button" className="welcome-provider" onClick={() => onSetup(CUSTOM_PROVIDER)}>
            <strong>其他邮箱</strong>
            <small>自定义 IMAP / SMTP</small>
          </button>
        </div>
        <ul className="welcome-facts">
          <li>凭据仅保存在系统钥匙串，不经过任何第三方服务器</li>
          <li>关闭窗口后，后台以极低内存静默收信</li>
        </ul>
      </div>
    </section>
  );
}
