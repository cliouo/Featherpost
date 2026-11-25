import { m } from "@featherpost/i18n/messages";

import { accountReason, type AccountReasonSource } from "../i18n";

/**
 * 账户离线原因（ADR-0048 L6）：按 daemon 的稳定 code 用界面语言成文，服务器原文作为次要信息
 * 原样显示在下面。两个默认前端的账户页共用。
 */
export function AccountReason({ source }: { source: AccountReasonSource }) {
  const reason = accountReason(source);
  if (!reason) return null;
  return (
    <>
      <span className="account-reason">{reason.text}</span>
      {reason.server && <code aria-label={m.common_account_detail_server_label()}>{reason.server}</code>}
    </>
  );
}
