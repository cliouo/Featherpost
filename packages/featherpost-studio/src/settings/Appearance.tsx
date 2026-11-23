import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "@featherpost/i18n/react";
import { useEffect } from "react";
import type { Host } from "../../../maild-ui/src/host/host";
import { useStore } from "../../../maild-ui/src/host/hooks";
import type { Preferences } from "../../../maild-ui/src/preferences";
import { Choice } from "../../../maild-ui/src/settings/Choice";
import {
  LANGUAGE_OPTIONS,
  languageChoice,
  type LanguageChoice,
  type LanguageController,
} from "../../../maild-ui/src/settings/language";

// 存消息函数而非文案：每次渲染按当前界面语言取值。
const themes = [
  { value: "light", label: m.studio_appearance_theme_light },
  { value: "dark", label: m.studio_appearance_theme_dark },
  { value: "system", label: m.studio_appearance_theme_system },
] as const;

/** ADR-0048：界面语言。偏好归 daemon，所有窗口共用；语言名用其自身语言书写。 */
function Language({ host, language }: { host: Host; language: LanguageController }) {
  useUiLocale();
  const state = useStore(language.store);
  const online = useStore(host.state).snapshot.phase === "online";
  useEffect(() => {
    if (online) void language.load();
  }, [language, online]);
  if (!language.available) return null;
  const choice = languageChoice(state.preference);
  const locked = state.unsupported || state.preference?.persisted === false;
  return (
    <>
      <div className="studio-appearance-label language-label">
        <h3 id="studio-language-label">{m.studio_appearance_language_heading()}</h3>
        <span>
          {state.unsupported
            ? m.studio_appearance_language_unavailable()
            : state.preference?.persisted === false
              ? m.studio_appearance_language_not_persisted()
              : m.studio_appearance_language_hint()}
        </span>
      </div>
      <div className="studio-language">
        <Choice
          aria-labelledby="studio-language-label"
          value={choice ?? ""}
          disabled={state.busy || locked || !online || choice === null}
          onValueChange={(value) => {
            if (value === "system" || LANGUAGE_OPTIONS.some((option) => option.value === value))
              void language.choose(value as LanguageChoice);
          }}
        >
          <option value="system">{m.studio_appearance_language_system()}</option>
          {LANGUAGE_OPTIONS.map((option) => (
            <option key={option.value} value={option.value} lang={option.value}>
              {option.name()}
            </option>
          ))}
          {choice === "custom" && (
            <option value="custom" disabled>
              {m.studio_appearance_language_custom({ languages: state.preference!.languages.join(", ") })}
            </option>
          )}
        </Choice>
        {state.error && <p className="studio-appearance-note" role="alert">{state.error}</p>}
      </div>
    </>
  );
}

export function Appearance({ host, language }: { host: Host; language?: LanguageController }) {
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
      {language && <Language host={host} language={language} />}
      <p className="studio-appearance-note">✓ {m.studio_appearance_persist_note()}</p>
    </section>
  );
}
