import { applyEffectiveLocales } from '@featherpost/i18n';

import type { LocaleBridge } from './types';

export interface LocaleFollower {
  /** 第一次拿到 daemon 的偏好列表（启动值或推送）并已切换语言时完成；拿不到就一直挂着。 */
  ready: Promise<void>;
  /** 取消订阅。 */
  stop(): void;
}

/**
 * 界面语言跟随 daemon 的语言偏好（ADR-0048 决策 8）：先订阅 `maild://locale`，再取壳手里的
 * 当前值。推送先到就不再用启动值——推送更新。`null`（尚未取到、老后台）按空列表协商，
 * 即基础语言 zh-CN；此时 `ready` 等第一次推送。
 */
export function followDaemonLocale(locale: LocaleBridge): LocaleFollower {
  let pushed = false;
  let stopped = false;
  let unsubscribe: (() => void) | null = null;
  let markReady!: () => void;
  const ready = new Promise<void>((resolve) => {
    markReady = resolve;
  });
  const apply = (effective: string[] | null) => {
    applyEffectiveLocales(effective ?? []);
    if (effective !== null) markReady();
  };
  void (async () => {
    const stop = await locale.subscribe((next) => {
      pushed = true;
      apply(next.effective);
    });
    if (stopped) return stop();
    unsubscribe = stop;
    try {
      const current = await locale.current();
      if (!pushed) apply(current.effective);
    } catch {
      // 拿不到启动值就停在基础语言，等推送。
    }
  })().catch(() => {});
  return {
    ready,
    stop() {
      stopped = true;
      unsubscribe?.();
      unsubscribe = null;
    },
  };
}

/** 首帧前最多等这么久：本地 IPC 读缓存只要几毫秒；后台还没连上时不让窗口一直空白。 */
export const STARTUP_LOCALE_TIMEOUT_MS = 300;

/** 等启动语言就绪或超时，二者先到为准；用于在首帧前切好语言，避免英文用户先看到一帧中文。 */
export function startupLocale(follower: LocaleFollower | null, timeoutMs = STARTUP_LOCALE_TIMEOUT_MS): Promise<void> {
  if (!follower) return Promise.resolve();
  return Promise.race([follower.ready, new Promise<void>((resolve) => setTimeout(resolve, timeoutMs))]);
}
