import { m } from '@featherpost/i18n/messages';

import { Icon } from '../app/Icon';
import type { Host } from '../host/host';
import { useStore } from '../host/hooks';
import { Store } from '../host/store';
import type { Bundle, ViewProps } from '../host/types';
import { uiLocale, useUiLocale } from '../i18n';

interface Excerpt {
  account: string;
  folder: string;
  uid: string;
  subject: string;
  at: number;
}

const VIEW = 'maild.sample-excerpt.collection';

function excerptCount(count: number): string {
  return new Intl.PluralRules(uiLocale()).select(count) === 'one'
    ? m.extensions_sample_count_one({ count })
    : m.extensions_sample_count_other({ count });
}

/**
 * 示例扩展「邮件摘录」：证明同一命令能同时出现在工具栏与溢出菜单、停用时按钮/菜单/工作页/
 * 设置分区一起消失。它是可信的编译期模块，不代表第三方隔离（E02）已经存在。
 */
export function sampleExcerptModule(host: Host): Bundle {
  const excerpts = new Store<readonly Excerpt[]>([]);

  function Collection(_props: ViewProps) {
    useUiLocale();
    const items = useStore(excerpts);
    return (
      <div className="page">
        <div className="page-toolbar" data-tauri-drag-region="deep">
          <button type="button" className="text-button" onClick={() => host.openRole('mailbox')}>
            <Icon name="back" /> {m.extensions_sample_back()}
          </button>
          <h1>{m.extensions_sample_collection_title()}</h1>
          <small>{m.extensions_sample_collection_subtitle()}</small>
        </div>
        <div className="page-scroll">
          <div className="page-column">
            <h2>{m.extensions_sample_collection_title()}</h2>
            <p className="page-intro">{m.extensions_sample_collection_intro()}</p>
            {items.length === 0 && <p className="group-note">{m.extensions_sample_collection_empty()}</p>}
            {items.map((item) => (
              <div key={`${item.account}/${item.folder}/${item.uid}`} className="setting-row">
                <div className="setting-copy">
                  <strong>{item.subject || m.extensions_sample_no_subject()}</strong>
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
    useUiLocale();
    const items = useStore(excerpts);
    return (
      <>
        <h2>{m.extensions_sample_title()}</h2>
        <p className="page-intro">{m.extensions_sample_intro()}</p>
        <div className="setting-row">
          <div className="setting-copy">
            <strong>{m.extensions_sample_clipped()}</strong>
            <p>{excerptCount(items.length)}</p>
          </div>
          <button type="button" className="secondary" onClick={() => host.openView(VIEW)}>{m.extensions_sample_open()}</button>
        </div>
      </>
    );
  }

  return {
    moduleId: 'maild.sample-excerpt',
    apiVersion: 1,
    // 标题写成 getter：注册表保留访问器，切换语言后按当前语言重新成文。
    get title() {
      return m.extensions_sample_title();
    },
    commands: [
      {
        id: 'maild.sample-excerpt.clip',
        get title() {
          return m.extensions_sample_clip();
        },
        icon: 'clip',
        locations: ['toolbar', 'menu'],
        order: 500,
        enabledWhen: 'message-selected',
        execute: (context) => {
          if (!context.selection) return;
          const { account, folder, uid } = context.selection;
          const current = excerpts.getSnapshot();
          if (current.some((item) => item.account === account && item.folder === folder && item.uid === uid)) {
            context.notify(m.extensions_sample_clip_duplicate());
            return;
          }
          excerpts.set([{ account, folder, uid, subject: context.selectionSubject ?? '', at: Date.now() }, ...current].slice(0, 200));
          context.notify(m.extensions_sample_clip_done());
        },
      },
      {
        id: 'maild.sample-excerpt.open',
        get title() {
          return m.extensions_sample_open();
        },
        icon: 'clip',
        locations: ['menu'],
        order: 510,
        execute: (context) => context.openView(VIEW),
      },
    ],
    views: [
      {
        id: VIEW,
        get title() {
          return m.extensions_sample_collection_title();
        },
        icon: 'clip',
        order: 100,
        render: Collection,
      },
    ],
    settings: [
      {
        id: 'maild.sample-excerpt.settings',
        get title() {
          return m.extensions_sample_title();
        },
        order: 100,
        render: Section,
      },
    ],
  };
}
