import { m } from '@featherpost/i18n/messages';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { BridgeError, errorCode } from './errors';
import { followDaemonLocale, startupLocale } from './locale';
import type { LocaleBridge, MailBridge, OpenIntent, UiError, UiLink, UiLocale, UiLocalePreference, UiSnapshot } from './types';
import { isUiError } from './types';
import { BRAND } from '../brand';

const STATE_EVENT = 'maild://state';
const INTENT_EVENT = 'maild://intent';
const LINK_EVENT = 'maild://link';
const LOCALE_EVENT = 'maild://locale';

// 每次调用时成文，跟随当前界面语言。
const notInTauri = (): UiError => ({
  code: 'offline',
  message: m.common_bridge_error_not_in_tauri({ product: BRAND.productName }),
});

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new BridgeError(notInTauri());
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    // Rust 侧已把原因投影成四类固定文案；其他形状（IPC 本身失败）一律当离线。
    throw new BridgeError(isUiError(error) ? error : { code: 'failed', message: m.common_bridge_error_failed() });
  }
}

/** 服务商帮助页等固定 https 链接；邮件正文里的链接仍只走 ui_open_link 令牌通道。 */
function openExternalUrl(url: string): Promise<void> {
  if (!/^https:\/\/[^\s]+$/.test(url)) return Promise.reject(new BridgeError({ code: 'invalid', message: m.common_bridge_error_invalid_link() }));
  return call('ui_open_external_url', { url });
}

// ADR-0048：语言偏好由 daemon 拥有，壳转发；协商与切换在 @featherpost/i18n。
const tauriLocale: LocaleBridge = {
  current: () => call<UiLocale>('ui_locale'),
  subscribe: async (handler) => {
    if (!isTauri()) return () => {};
    return listen<UiLocale>(LOCALE_EVENT, (event) => handler(event.payload));
  },
  preference: () => call<UiLocalePreference>('ui_locale_preference'),
  setPreference: (update) => call<UiLocalePreference>('ui_set_locale_preference', { update }),
};

export const tauriBridge: MailBridge = {
  settings: {
    profiles:()=>call('ui_settings_profiles'),probe:request=>call('ui_settings_probe',{request}),
    saveAccount:request=>call('ui_settings_save_account',{request}),accountAction:request=>call('ui_settings_account_action',{request}),
    rules:()=>call('ui_settings_rules'),saveRules:patch=>call('ui_settings_rules',{patch}),
    notification:(request=false)=>call('ui_settings_notification',{request}),
    addresses:(account,offset)=>call('ui_settings_addresses',{account,offset}),addressAction:request=>call('ui_settings_address_action',{request}),
    packages:()=>call('ui_settings_packages'),packageAction:request=>call('ui_settings_package_action',{request}),
    pickPackage:kind=>call('ui_settings_pick_package',{kind}),cancelPackage:token=>call('ui_settings_cancel_package',{token}),
    installPackage:token=>call('ui_settings_install',{token}),rollbackPreview:(kind,id)=>call('ui_settings_rollback_preview',{kind,id}),
    rollback:preview=>call('ui_settings_rollback',{preview}),
    // 后端发现未注册或失败时返回 null，界面按 imap.<domain> / smtp.<domain> 预填。
    discoverProvider:async address=>{ try { return await call('ui_settings_discover_provider',{address}); } catch { return null; } },
    openExternalUrl:url=>openExternalUrl(url),
  },
  mailbox: {
    profiles:()=>call('ui_mailbox_profiles'), query:(query,offset)=>call('ui_mailbox_query',{query,offset}),
    thread:(identity,offset)=>call('ui_mailbox_thread',{...identity,offset}),locate:identity=>call('ui_mailbox_locate',{...identity}),
    index:()=>call('ui_mailbox_index'),write:request=>call('ui_mailbox_write',{request}),
    activity:(account,before)=>call('ui_mailbox_activity',{account:account??null,before_id:before??null}),
    resolve:(id,resolution)=>call('ui_mailbox_resolve',{id,resolution}),
    // —— T4 新增，可选能力：后台未注册命令时的降级见各自注释 ——
    syncNow:async(account)=>{
      try { await call<void>('ui_sync_mailbox',{account:account??null}); }
      catch (error) {
        // 未注册命令、旧后台都投影成 failed：静默降级，调用方随后照常刷新列表。
        // 离线与无授权如实上抛。
        const code = errorCode(error);
        if (code === 'offline' || code === 'denied') throw error;
      }
    },
    serverSearch:(account,folder,query,options)=>call('ui_server_search',{account,folder,query,since:options?.since??null,until:options?.until??null}),
    fetchServerMessage:(account,folder,uid)=>call('ui_fetch_server_message',{account,folder,uid}),
  },
  compose: {
    senders:()=>call('ui_senders'), drafts:(offset=0)=>call('ui_drafts',{offset}), draft:id=>call('ui_draft',{id}),
    create:(account,patch)=>call('ui_new_draft',{account,patch}), save:(id,version,patch)=>call('ui_save_draft',{id,version,patch}),
    reply:(identity,mode,locale)=>call('ui_reply_draft',{...identity,mode,locale:locale??null}), send:(id,version)=>call('ui_send_draft',{id,version}),
    resolve:(id,version)=>call('ui_resolve_send',{id,version}), discard:(id,version)=>call('ui_discard_draft',{id,version}),
    pick:()=>call('ui_pick_files'), upload:(draftId,token)=>call('ui_upload_file',{draft_id:draftId,token}),
    release:token=>call('ui_release_file',{token}), finish:(id,version)=>call('ui_finish_upload',{id,version}), cancel:id=>call('ui_cancel_upload',{id}),
    onProgress:async fn=>isTauri()?listen('maild://file-progress',e=>fn(e.payload as import('../compose/types').Progress)):()=>{},
    onPicked:async fn=>isTauri()?listen('maild://files-picked',e=>fn(e.payload as import('../compose/types').PickResult[])):()=>{},
    onClose:async fn=>isTauri()?listen('maild://close-request',()=>fn()):()=>{}, close:()=>call('ui_finish_close'), ready:()=>call('ui_compose_ready'),
  },
  available: isTauri(),
  snapshot: () => call<UiSnapshot>('ui_snapshot'),
  subscribe: async (handler) => {
    if (!isTauri()) return () => {};
    return listen<UiSnapshot>(STATE_EVENT, (event) => handler(event.payload));
  },
  onIntent: async (handler) => {
    if (!isTauri()) return () => {};
    return listen<OpenIntent>(INTENT_EVENT, (event) => handler(event.payload));
  },
  folders: (account) => call('ui_list_folders', { account }),
  messages: ({ account, folder }, offset, limit) => call('ui_list_messages', { account, folder, offset, limit }),
  message: ({ account, folder, uid }) => call('ui_get_message', { account, folder, uid }),
  refresh: () => call('ui_refresh'),
  frontendInfo: () => call('ui_frontend_info'),
  prepareHtml: ({ account, folder, uid }, allowRemote) =>
    call('ui_prepare_html', { account, folder, uid, allow_remote: allowRemote }),
  releaseHtml: (token) => call('ui_release_html', { token }),
  onLink: async (handler) => {
    if (!isTauri()) return () => {};
    return listen<UiLink>(LINK_EVENT, (event) => handler(event.payload));
  },
  openLink: (token, index) => call('ui_open_link', { token, index }),
  saveAttachment: ({ account, folder, uid }, partIndex) =>
    call('ui_save_attachment', { account, folder, uid, part_index: partIndex }),
  readingPolicy: () => call('ui_reading_policy'),
  setReadingPolicy: (patch) => call('ui_set_reading_policy', { patch }),
  openExternalUrl,
  locale: tauriLocale,
};

// 默认前端与 Studio 都从这里取桥：导入即开始跟随 daemon 的语言偏好（启动值 + `maild://locale`）。
const daemonLocale = isTauri() ? followDaemonLocale(tauriLocale) : null;

/** 两个前端的 `main.tsx` 在首帧前等它：启动语言就绪或短暂超时后再渲染（ADR-0048 决策 8）。 */
export function startupLocaleReady(): Promise<void> {
  return startupLocale(daemonLocale);
}

export { BridgeError, errorCode } from './errors';
