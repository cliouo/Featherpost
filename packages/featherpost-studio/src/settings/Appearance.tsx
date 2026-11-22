import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "@featherpost/i18n/react";
import type { Host } from "../../../maild-ui/src/host/host";
import { useStore } from "../../../maild-ui/src/host/hooks";
import type { Preferences } from "../../../maild-ui/src/preferences";

// 存消息函数而非文案：每次渲染按当前界面语言取值。
const themes = [
  { value: "light", label: m.studio_appearance_theme_light },
  { value: "dark", label: m.studio_appearance_theme_dark },
  { value: "system", label: m.studio_appearance_theme_system },
] as const;

export function Appearance({ host }: { host: Host }) {
  useUiLocale();
  const { preferences } = useStore(host.state);
  const update = (patch: Partial<Preferences>) =>
    host.setPreferences({ ...preferences, ...patch });
  return (
    <section className="studio-appearance">
      <div className="studio-appearance-label">
        <h3 id="studio-theme-label">{m.studio_appearance_theme_heading()}</h3>
        <span>{m.studio_appearance_theme_current({ theme: themes.find(theme => theme.value === preferences.theme)?.label() ?? "" })}</span>
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
            <span className="studio-theme-label">{theme.label()}</span>
          </label>
        ))}
      </div>
      <div className="studio-appearance-label density-label">
        <h3 id="studio-density-label">{m.studio_appearance_density_heading()}</h3><span>{m.studio_appearance_density_hint()}</span>
      </div>
      <div className="studio-density-options" role="group" aria-labelledby="studio-density-label">
        {([
          { value: "compact", label: m.studio_appearance_density_compact(), copy: m.studio_appearance_density_compact_hint() },
          { value: "comfortable", label: m.studio_appearance_density_comfortable(), copy: m.studio_appearance_density_comfortable_hint() },
        ] as const).map(density => (
          <label key={density.value} className={`studio-density-option ${density.value}${preferences.density === density.value ? " selected" : ""}`}>
            <span className="studio-density-copy">
              <span className="studio-density-heading">
                <input type="radio" name="studio-density" value={density.value} checked={preferences.density === density.value}
                  onChange={() => update({ density: density.value })} />
                <strong>{density.label}</strong>
              </span>
              <small>{density.copy}</small>
            </span>
            <span className="studio-density-lines" aria-hidden="true">
              {(density.value === "compact" ? [0,1,2] : [0,1]).map(n => <i key={n} />)}
            </span>
          </label>
        ))}
      </div>
      <p className="studio-appearance-note">✓ {m.studio_appearance_persist_note()}</p>
    </section>
  );
}
