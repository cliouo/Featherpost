import { m } from '@featherpost/i18n/messages';

import { BRIDGE_ERROR_REASONS } from './reasons';
import type { UiError, UiErrorCode } from './types';

/**
 * 桥错误的用户文案（ADR-0048 决策 3）：Rust 侧给稳定 `reason`，按当前界面语言成文；不认识的
 * `reason` 显示通用提示，不把中文兜底原文直接给英文界面。webview 自己构造的错误没有 `reason`，
 * 其 `message` 已是当前语言。
 */
export function bridgeErrorText(detail: UiError): string {
  if (detail.reason === undefined) return detail.message;
  const text = Object.prototype.hasOwnProperty.call(BRIDGE_ERROR_REASONS, detail.reason)
    ? BRIDGE_ERROR_REASONS[detail.reason]
    : undefined;
  return text ? text() : m.common_bridge_error_failed();
}

/** 桥抛出的唯一错误类型；`detail` 是 Rust 侧的投影，`message` 是构造时界面语言的文案。 */
export class BridgeError extends Error {
  constructor(public readonly detail: UiError) {
    super(bridgeErrorText(detail));
    this.name = 'BridgeError';
  }
}

export function errorCode(error: unknown): UiErrorCode {
  return error instanceof BridgeError ? error.detail.code : 'failed';
}
