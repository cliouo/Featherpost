import type { Host } from "../../../maild-ui/src/host/host";
import { useStore } from "../../../maild-ui/src/host/hooks";
import type { Preferences } from "../../../maild-ui/src/preferences";

const themes = [
  { value: "light", label: "浅色" },
  { value: "dark", label: "深色" },
  { value: "system", label: "跟随系统" },
] as const;

export function Appearance({ host }: { host: Host }) {
  const { preferences } = useStore(host.state);
  const update = (patch: Partial<Preferences>) =>
    host.setPreferences({ ...preferences, ...patch });
  return (
    <section className="studio-appearance">
      <div className="studio-appearance-label">
        <h3 id="studio-theme-label">主题</h3>
        <span>当前：{themes.find(theme => theme.value === preferences.theme)?.label}</span>
      </div>
      <div className="studio-themes" role="group" aria-labelledby="studio-theme-label">
        {themes.map(theme => (
          <label key={theme.value} className={`studio-theme-option ${theme.value}`}>
            <input type="radio" name="studio-theme" value={theme.value} checked={preferences.theme === theme.value}
              onChange={() => update({ theme: theme.value })} />
            <span className="studio-theme-preview" aria-hidden="true">
              <span className="studio-mini-nav">{[0,1,2,3,4].map(n => <i key={n} />)}</span>
              <span className="studio-mini-list">{[0,1,2,3].map(n => <i key={n} />)}</span>
              <span className="studio-mini-reader"><b />{[0,1,2,3].map(n => <i key={n} />)}</span>
            </span>
            <span className="studio-theme-label">{theme.label}</span>
          </label>
        ))}
      </div>
      <div className="studio-appearance-label density-label">
        <h3 id="studio-density-label">列表密度</h3><span>预览邮件列表的行间距</span>
      </div>
      <div className="studio-density-options" role="group" aria-labelledby="studio-density-label">
        {([
          { value: "compact", label: "紧凑", copy: "同屏显示更多邮件" },
          { value: "comfortable", label: "舒适", copy: "保留更宽松的行间距" },
        ] as const).map(density => (
          <label key={density.value} className={`studio-density-option ${density.value}${preferences.density === density.value ? " selected" : ""}`}>
            <input type="radio" name="studio-density" value={density.value} checked={preferences.density === density.value}
              onChange={() => update({ density: density.value })} />
            <span><strong>{density.label}</strong><small>{density.copy}</small></span>
            <span className="studio-density-lines" aria-hidden="true">
              {(density.value === "compact" ? [0,1,2] : [0,1]).map(n => <i key={n} />)}
            </span>
          </label>
        ))}
      </div>
      <p className="studio-appearance-note">✓ 主题与密度会保留到下次打开窗口</p>
    </section>
  );
}
