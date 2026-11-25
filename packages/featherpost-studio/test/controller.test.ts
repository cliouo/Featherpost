import { describe, expect, it } from 'vitest';

import { MailboxController } from '../src/mailbox/controller';
import { FakeBridge, flush, folder, message, offlineError, online, summary } from './fakeBridge';

async function ready() {
  const bridge = new FakeBridge();
  const selections: unknown[] = [];
  const controller = new MailboxController(bridge, (selection) => selections.push(selection));
  controller.acceptSnapshot(online);
  // 上线 → 选账户 → 拉文件夹。
  expect(bridge.folderRequests).toHaveLength(1);
  bridge.folderRequests[0].d.resolve([folder('INBOX', 'inbox', 2), folder('Archive', 'archive')]);
  await flush();
  controller.selectAccount('demo');
  expect(controller.state.folder?.name).toBe('INBOX');
  return { bridge, controller, selections };
}

describe('MailboxController：旧响应不覆盖新选择（UX-SPEC §4）', () => {
  it('迟到的旧文件夹分页被丢弃', async () => {
    const { bridge, controller } = await ready();
    const inbox = bridge.pageRequests.at(-1)!;
    expect(inbox.folder.folder).toBe('INBOX');
    controller.selectFolder(controller.state.folders[1]); // Archive
    const archive = bridge.pageRequests.at(-1)!;
    expect(archive.folder.folder).toBe('Archive');
    // INBOX 的应答后到：不能进 Archive 的列表。
    inbox.d.resolve({ messages: [summary('INBOX', '1', '旧')], total: 1 });
    await flush();
    expect(controller.state.rows).toEqual([]);
    archive.d.resolve({ messages: [summary('Archive', '9', '新')], total: 1 });
    await flush();
    expect(controller.state.rows.map((r) => r.subject)).toEqual(['新']);
    expect(controller.state.folder?.name).toBe('Archive');
  });

  it('切换邮件后，前一封的正文应答不覆盖当前选中', async () => {
    const { bridge, controller, selections } = await ready();
    bridge.pageRequests.at(-1)!.d.resolve({ messages: [summary('INBOX', '1'), summary('INBOX', '2')], total: 2 });
    await flush();
    controller.selectMessage({ account: 'demo', folder: 'INBOX', uid: '1' });
    const first = bridge.detailRequests.at(-1)!;
    controller.selectMessage({ account: 'demo', folder: 'INBOX', uid: '2' });
    const second = bridge.detailRequests.at(-1)!;
    first.d.resolve(message('INBOX', '1'));
    await flush();
    expect(controller.state.detail).toBeNull();
    second.d.resolve(message('INBOX', '2'));
    await flush();
    expect(controller.state.detail?.body).toBe('body 2');
    // 选择回调给宿主（命令的受限上下文）：最后一次是 uid 2。
    expect(selections.at(-1)).toEqual({ identity: { account: 'demo', folder: 'INBOX', uid: '2' }, subject: '2' });
  });

  it('连接纪元变化让在途请求作废，并重新对齐', async () => {
    const { bridge, controller } = await ready();
    const pending = bridge.pageRequests.at(-1)!;
    controller.acceptSnapshot({ ...online, revision: 5, connection_epoch: 2 });
    pending.d.resolve({ messages: [summary('INBOX', '1', '旧纪元')], total: 1 });
    await flush();
    expect(controller.state.rows).toEqual([]);
    // 新纪元重新拉了文件夹。
    expect(bridge.folderRequests.length).toBeGreaterThanOrEqual(2);
  });

  it('旧 revision 的快照被忽略', async () => {
    const { bridge, controller } = await ready();
    const before = bridge.folderRequests.length;
    controller.acceptSnapshot({ ...online, revision: 0 });
    expect(bridge.folderRequests.length).toBe(before);
  });

  it('离线只读：无缓存给说明，有缓存照常显示，错误不带路径', async () => {
    const { bridge, controller } = await ready();
    bridge.pageRequests.at(-1)!.d.resolve({ messages: [summary('INBOX', '1'), summary('INBOX', '2')], total: 2 });
    await flush();
    controller.selectMessage({ account: 'demo', folder: 'INBOX', uid: '1' });
    bridge.detailRequests.at(-1)!.d.resolve(message('INBOX', '1'));
    await flush();
    controller.acceptSnapshot({ ...online, revision: 9, phase: 'retry_waiting', retry_in_ms: 800 });
    controller.selectMessage({ account: 'demo', folder: 'INBOX', uid: '1' });
    expect(controller.state.detail?.body).toBe('body 1');
    expect(controller.state.detailError).toBeNull();
    controller.selectMessage({ account: 'demo', folder: 'INBOX', uid: '2' });
    expect(controller.state.detail).toBeNull();
    expect(controller.state.detailError).toMatch(/连接恢复/);
    // 在线但桥报离线：文案是固定的用户文案。
    controller.acceptSnapshot({ ...online, revision: 10 });
    bridge.folderRequests.at(-1)!.d.reject(offlineError());
    await flush();
    expect(controller.state.folderError).toBe('后台连接不可用，已读取的内容仍可查看。');
  });

  it('跨账户的文件夹应答被当作范围错误丢弃', async () => {
    const { bridge, controller } = await ready();
    void controller.refresh();
    bridge.folderRequests.at(-1)!.d.resolve([{ ...folder('INBOX', 'inbox'), account: 'other' }]);
    await flush();
    expect(controller.state.folderError).toMatch(/文件夹/);
    expect(controller.state.folders.map((f) => f.name)).toEqual(['INBOX', 'Archive']);
  });
});
