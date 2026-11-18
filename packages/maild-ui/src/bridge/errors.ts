import type { UiError, UiErrorCode } from './types';

/** 桥抛出的唯一错误类型；`detail` 是 Rust 侧投影好的四类固定文案。 */
export class BridgeError extends Error {
  constructor(public readonly detail: UiError) {
    super(detail.message);
    this.name = 'BridgeError';
  }
}

export function errorCode(error: unknown): UiErrorCode {
  return error instanceof BridgeError ? error.detail.code : 'failed';
}
