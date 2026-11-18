/**
 * 唯一进浏览器存储的东西：三个非敏感外观值（ADR-0031 决策 7）。
 * `readerAppearance` 只决定深色外壳下 HTML 邮件是否施加柔和深色滤镜，不改 CSP 或 sandbox。
 */
export type ReaderAppearance = 'paper' | 'dark-adapted';
export interface Preferences {
  theme: 'system' | 'light' | 'dark';
  density: 'comfortable' | 'compact';
  readerAppearance?: ReaderAppearance;
}
export const defaultPreferences: Preferences = { theme: 'system', density: 'comfortable', readerAppearance: 'paper' };
export const PREFERENCE_KEY = 'maild.ui.preferences.v1';
type StorageReader = Pick<Storage, 'getItem'>;
type StorageWriter = Pick<Storage, 'setItem'>;

export function readPreferences(storage?: StorageReader): Preferences {
  try {
    const data: unknown = JSON.parse(storage?.getItem(PREFERENCE_KEY) ?? 'null');
    if (!data || typeof data !== 'object') return { ...defaultPreferences };
    const candidate = data as Partial<Preferences>;
    return {
      theme: candidate.theme === 'light' || candidate.theme === 'dark' ? candidate.theme : 'system',
      density: candidate.density === 'compact' ? 'compact' : 'comfortable',
      readerAppearance: candidate.readerAppearance === 'dark-adapted' ? 'dark-adapted' : 'paper',
    };
  } catch {
    return { ...defaultPreferences };
  }
}

export function savePreferences(preferences: Preferences, storage?: StorageWriter): boolean {
  try {
    if (!storage) return false;
    // 只投影这三个白名单键。账户、文件夹、邮件身份、草稿、token、扩展状态都不进 localStorage。
    storage.setItem(
      PREFERENCE_KEY,
      JSON.stringify({
        theme: preferences.theme,
        density: preferences.density,
        readerAppearance: preferences.readerAppearance === 'dark-adapted' ? 'dark-adapted' : 'paper',
      }),
    );
    return true;
  } catch {
    return false;
  }
}

export function preferenceStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}
