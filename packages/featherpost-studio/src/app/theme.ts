import { useEffect, useState } from 'react';

import type { Preferences } from '../preferences';

/** 主题落在 <html> 的 data 属性上；CSS 只认 token，组件不知道颜色。 */
export function applyTheme(preferences: Preferences, root: HTMLElement = document.documentElement) {
  if (preferences.theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = preferences.theme;
  root.dataset.density = preferences.density;
}

const DARK_QUERY = '(prefers-color-scheme: dark)';

function systemDark(): boolean {
  try {
    return typeof matchMedia === 'function' && matchMedia(DARK_QUERY).matches;
  } catch {
    return false;
  }
}

/** 外壳当前是否为深色：显式深色，或跟随系统且系统为深色。 */
export function useDarkShell(theme: Preferences['theme']): boolean {
  const [dark, setDark] = useState(systemDark);
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const media = matchMedia(DARK_QUERY);
    const update = () => setDark(media.matches);
    update();
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);
  return theme === 'dark' || (theme === 'system' && dark);
}
