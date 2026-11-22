import { applyEffectiveLocales } from '@featherpost/i18n';

import type { LocaleBridge } from './types';

/**
 * 界面语言跟随 daemon 的语言偏好（ADR-0048 决策 8）：先订阅 `maild://locale`，再取壳手里的
 * 当前值。推送先到就不再用启动值——推送更新。`null`（尚未取到、老后台）按空列表协商，
 * 即基础语言 zh-CN。返回取消订阅。
 */
export async function followDaemonLocale(locale: LocaleBridge): Promise<() => void> {
  let pushed = false;
  const stop = await locale.subscribe((next) => {
    pushed = true;
    applyEffectiveLocales(next.effective ?? []);
  });
  try {
    const current = await locale.current();
    if (!pushed) applyEffectiveLocales(current.effective ?? []);
  } catch {
    // 拿不到启动值就停在基础语言，等推送。
  }
  return stop;
}
