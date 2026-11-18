import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app/App';
import { tauriBridge } from './bridge/tauri';
import { Host } from './host/host';
import { mailboxModule } from './mailbox/module';
import { sampleExcerptModule } from './sample/module';
import { settingsModule } from './settings/module';
import { SettingsController } from './settings/controller';
import './settings/settings.css';
import { ComposeController } from './compose/controller';
import { composeModule } from './compose/module';
import './compose/compose.css';
import './styles/tokens.css';
import './styles/app.css';

// 装配：内置模块与示例扩展走同一个注册底座（ADR-0031 决策 5）。
const host = new Host(tauriBridge);
const composer = new ComposeController(tauriBridge.compose, id=>host.openView(id), text=>host.notify(text));
host.registerBuiltin(composeModule(host, composer));
const settings = new SettingsController(tauriBridge.settings,()=>{void tauriBridge.refresh().catch(()=>{});});
// 邮箱欢迎页与账户提醒直达设置：预选服务商打开向导，或切到账户管理修复。
host.registerBuiltin(mailboxModule(host, composer, ({ provider, repair }) => {
  if (repair !== undefined) settings.openAccounts();
  else settings.openSetup(provider);
  host.openRole('settings');
}));
composer.setBeforeClose(()=>settings.beforeClose());
composer.store.subscribe(()=>{if(!composer.store.getSnapshot().closing)settings.releaseClose();});
host.registerBuiltin(settingsModule(host,settings));
// 示例扩展默认关闭：没有启用可选扩展时三栏界面保持原样，在「设置 → 扩展」里开。
host.registerOptional('maild.sample-excerpt', { title: '邮件摘录（示例）', bundle: () => sampleExcerptModule(host), defaultEnabled: false });
host.finishAssembly();
void host.start();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App host={host} />
  </StrictMode>,
);
