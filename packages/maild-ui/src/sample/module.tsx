import { Icon } from '../app/Icon';
import type { Host } from '../host/host';
import { useStore } from '../host/hooks';
import { Store } from '../host/store';
import type { Bundle, ViewProps } from '../host/types';

interface Excerpt {
  account: string;
  folder: string;
  uid: string;
  subject: string;
  at: number;
}

const VIEW = 'maild.sample-excerpt.collection';

/**
 * 示例扩展「邮件摘录」：证明同一命令能同时出现在工具栏与溢出菜单、停用时按钮/菜单/工作页/
 * 设置分区一起消失。它是可信的编译期模块，不代表第三方隔离（E02）已经存在。
 */
export function sampleExcerptModule(host: Host): Bundle {
  const excerpts = new Store<readonly Excerpt[]>([]);

  function Collection(_props: ViewProps) {
    const items = useStore(excerpts);
    return (
      <div className="page">
        <div className="page-toolbar">
          <button type="button" className="text-button" onClick={() => host.openRole('mailbox')}>
            <Icon name="back" /> 返回邮件
          </button>
          <h1>摘录集</h1>
          <small>邮件摘录（示例） · 本次会话内存</small>
        </div>
        <div className="page-scroll">
          <div className="page-column">
            <h2>摘录集</h2>
            <p className="page-intro">在阅读工具栏或「更多操作」里点「加入摘录集」，当前邮件的主题会记在这里。关闭窗口即清空——示例不做持久化。</p>
            {items.length === 0 && <p className="group-note">还没有摘录。</p>}
            {items.map((item) => (
              <div key={`${item.account}/${item.folder}/${item.uid}`} className="setting-row">
                <div className="setting-copy">
                  <strong>{item.subject || '（无主题）'}</strong>
                  <p>{item.account} · {item.folder} · UID {item.uid}</p>
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
    );
  }

  function Section(_props: ViewProps) {
    const items = useStore(excerpts);
    return (
      <>
        <h2>邮件摘录（示例）</h2>
        <p className="page-intro">这是一个随包附带的示例扩展，用来演示界面贡献点。它贡献了一个工具栏命令、一个菜单命令、一个工作页和这个设置分区。</p>
        <div className="setting-row">
          <div className="setting-copy">
            <strong>已摘录</strong>
            <p>{items.length} 封</p>
          </div>
          <button type="button" className="secondary" onClick={() => host.openView(VIEW)}>打开摘录集</button>
        </div>
      </>
    );
  }

  return {
    moduleId: 'maild.sample-excerpt',
    apiVersion: 1,
    title: '邮件摘录（示例）',
    commands: [
      {
        id: 'maild.sample-excerpt.clip',
        title: '加入摘录集',
        icon: 'clip',
        locations: ['toolbar', 'menu'],
        order: 500,
        enabledWhen: 'message-selected',
        execute: (context) => {
          if (!context.selection) return;
          const { account, folder, uid } = context.selection;
          const current = excerpts.getSnapshot();
          if (current.some((item) => item.account === account && item.folder === folder && item.uid === uid)) {
            context.notify('这封邮件已在摘录集里。');
            return;
          }
          excerpts.set([{ account, folder, uid, subject: context.selectionSubject ?? '', at: Date.now() }, ...current].slice(0, 200));
          context.notify('已加入摘录集。');
        },
      },
      { id: 'maild.sample-excerpt.open', title: '打开摘录集', icon: 'clip', locations: ['menu'], order: 510, execute: (context) => context.openView(VIEW) },
    ],
    views: [{ id: VIEW, title: '摘录集', icon: 'clip', order: 100, render: Collection }],
    settings: [{ id: 'maild.sample-excerpt.settings', title: '邮件摘录（示例）', order: 100, render: Section }],
  };
}
