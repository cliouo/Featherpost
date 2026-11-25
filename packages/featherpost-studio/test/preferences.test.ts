import { describe, expect, it } from 'vitest';

import { PREFERENCE_KEY, readPreferences, savePreferences } from '../src/preferences';

describe('preferences：只存三个非敏感键（ADR-0031 决策 7）', () => {
  it('坏数据回默认', () => {
    const defaults = { theme: 'system', density: 'comfortable', readerAppearance: 'paper' };
    expect(readPreferences({ getItem: () => 'not json' })).toStrictEqual(defaults);
    expect(readPreferences({ getItem: () => JSON.stringify({ theme: 'neon', density: 'compact', readerAppearance: 'sepia' }) })).toStrictEqual({
      ...defaults,
      density: 'compact',
    });
    expect(readPreferences(undefined)).toStrictEqual(defaults);
  });

  it('旧的两键数据照常读取，阅读外观回到原色信纸', () => {
    expect(readPreferences({ getItem: () => JSON.stringify({ theme: 'dark', density: 'compact' }) })).toStrictEqual({
      theme: 'dark',
      density: 'compact',
      readerAppearance: 'paper',
    });
    expect(readPreferences({ getItem: () => JSON.stringify({ theme: 'dark', readerAppearance: 'dark-adapted' }) }).readerAppearance).toBe('dark-adapted');
  });

  it('写入只包含 theme、density 与 readerAppearance 白名单', () => {
    const written: Record<string, string> = {};
    const ok = savePreferences(
      { theme: 'dark', density: 'compact', readerAppearance: 'dark-adapted', ...({ token: 'secret', account: 'x' } as object) } as never,
      { setItem: (k, v) => void (written[k] = v) },
    );
    expect(ok).toBe(true);
    expect(JSON.parse(written[PREFERENCE_KEY])).toStrictEqual({ theme: 'dark', density: 'compact', readerAppearance: 'dark-adapted' });
    expect(Object.keys(JSON.parse(written[PREFERENCE_KEY])).sort()).toEqual(['density', 'readerAppearance', 'theme']);
    savePreferences({ theme: 'light', density: 'comfortable', readerAppearance: 'evil' as never }, { setItem: (k, v) => void (written[k] = v) });
    expect(JSON.parse(written[PREFERENCE_KEY]).readerAppearance).toBe('paper');
    expect(savePreferences({ theme: 'dark', density: 'compact' }, undefined)).toBe(false);
  });
});
