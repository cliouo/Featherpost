/* Trusted design fixture only. This is not the proposed third-party sandbox/SDK. */
'use strict';
const extensionDemoChild = new URLSearchParams(location.search).get('surface') === 'maild-notes-demo';
const extensionDemo = { enabled: false, placement: 'closed', context: 1, notes: new Map(), grants: new Set(), child: null, page: false };
let childView = null;
let extensionHandoffTimer;
const originalRender = render;
const surfaceMessageOrigin = location.protocol === 'file:' ? '*' : location.origin;
const receivedMessageOrigin = location.protocol === 'file:' ? 'null' : location.origin;

function extensionButton(action, label, svg = '', classes = 'secondary') {
  return `<button class="${classes}" data-ui-action="${action}">${svg ? icon(svg) : ''}${label}</button>`;
}
function extensionCurrentMessage() { return samples.find(m => m.id === extensionDemo.context) || samples[0]; }
function extensionNote() { return extensionDemo.notes.get(extensionDemo.context) || ''; }
function extensionPanelContent(mode = 'panel') {
  const m = extensionCurrentMessage(), allowed = extensionDemo.grants.has(m.id);
  const title = allowed ? esc(m.subject) : '尚未授权读取这封邮件';
  return `<header class="extension-panel-head"><span class="extension-symbol">${icon('book')}</span><div><strong>邮件摘录</strong><small>示例插件 · 在此窗口内</small></div><span class="spacer"></span>${extensionButton('close-panel', '关闭', 'close', 'icon-button')}</header>
    <div class="extension-body"><div class="extension-context"><span class="extension-eyebrow">绑定的邮件</span><strong>${title}</strong>${allowed?`<small>${esc(m.sender)} · ${accountNames[m.account]}</small>`:''}</div>
    ${!allowed?`<div class="extension-permission">${icon('shield')}<p>这个扩展希望读取你选中的一封邮件，用于整理摘录。</p>${extensionButton('grant', '允许读取这封邮件', '', 'primary')}<small>仅演示本封授权，不授权其他邮件。</small></div>`:`<div class="extension-excerpt"><span class="extension-eyebrow">邮件摘要</span><p>${esc(m.snippet)}</p><span class="extension-scope">${icon('lock')}仅这封邮件</span></div>`}
    ${state.selected && state.selected !== m.id?`<div class="extension-binding-note">你正在阅读另一封邮件，摘录仍固定在这里。${extensionButton('follow-current','切换到当前邮件','','text-button')}</div>`:''}
    <label class="extension-note-label" for="extension-note">我的摘录</label><textarea id="extension-note" maxlength="16000" placeholder="留下要记住的一句话…">${esc(extensionNote())}</textarea><span class="extension-note-status">${extensionNote()?'已暂存 · 本次演示':'内容仅保留在本次演示中'}</span>
    <div class="extension-panel-links">${extensionButton('detach','在独立窗口打开','monitor','text-button')}${mode==='panel'?extensionButton('workspace','查看摘录集','arrow','text-button'):extensionButton('back-mail','返回邮件','back','text-button')}</div>
    <p class="extension-footnote">扩展有自己的视图。邮件原文保持原样。</p></div>`;
}
function extensionWorkspace() {
  const saved = [...extensionDemo.notes].filter(([,text])=>text.trim());
  return `<section class="page extension-workspace"><header class="page-toolbar">${extensionButton('back-mail','返回邮件','back','text-button')}<h1>摘录集</h1><span class="spacer"></span><span class="tag">来自：邮件摘录 · 示例插件</span></header><div class="page-scroll"><div class="page-column"><span class="extension-eyebrow">YOUR MARGINALIA</span><h2>在邮件之间，留下自己的想法。</h2><p class="page-intro">这是插件贡献的工作页，收件箱仍在原来的位置。</p>${saved.length?saved.map(([id,text])=>`<button class="extension-note-row" data-ui-note="${id}"><span>${icon('book')}</span><span><strong>${esc(text.slice(0,110))}</strong><small>${esc(samples.find(m=>m.id===id)?.subject||'邮件摘录')}</small></span><span class="spacer"></span>${icon('chevron')}</button>`).join(''):`<div class="empty">${icon('book')}<h2>还没有摘录</h2><p>从任意邮件的“摘录”入口开始，写下想保留的内容。</p>${extensionButton('back-mail','回到邮件','','secondary')}</div>`}<p class="group-note">原型展示可注册的独立工作页。没有创建真实笔记数据库。</p></div></div></section>`;
}
function extensionWindowPlaceholder() {
  return `<aside class="extension-panel" aria-label="已分离的扩展视图"><header class="extension-panel-head"><strong>邮件摘录</strong><span class="spacer"></span>${extensionButton('close-panel','关闭占位','close','icon-button')}</header><div class="extension-detached-placeholder">${icon('monitor')}<h2>已在独立窗口打开</h2><p>同一份摘录只有一个编辑器。你可以把它放在另一块屏幕上。</p>${extensionButton('focus-window','聚焦窗口','','primary')}${extensionButton('dock','收回主窗口','back','secondary')}</div></aside>`;
}
function sendExtensionState() {
  if (!extensionDemo.child || extensionDemo.child.closed) return;
  const m=extensionCurrentMessage(),allowed=extensionDemo.grants.has(m.id);
  extensionDemo.child.postMessage({kind:'maild-notes:state',data:{allowed,title:allowed?m.subject:'尚未授权读取这封邮件',excerpt:allowed?m.snippet:'',note:extensionNote(),theme:state.theme}},surfaceMessageOrigin);
}
function mountExtensionDemo() {
  if (extensionDemoChild) { renderExtensionChild(); return; }
  if (extensionDemo.child?.closed) { extensionDemo.child=null;if(extensionDemo.placement==='window')extensionDemo.placement='closed'; }
  if (extensionDemo.page) {
    const work=document.querySelector('#workspace');if(work)work.innerHTML=extensionWorkspace();
  }
  const controls=document.querySelector('.reviewbar nav');
  if(controls&&!controls.querySelector('[data-ui-action="show-demo"]'))controls.insertAdjacentHTML('beforeend',extensionButton('show-demo','扩展示例','','review-extension'));
  const settingsContent=state.route==='settings'&&state.setting==='extensions'?document.querySelector('.settings-content .page-column'):null;
  if(settingsContent){settingsContent.insertAdjacentHTML('beforeend',`<p class="section-label">界面扩展 · 设计示例</p><div class="setting-row"><div class="setting-copy"><strong>邮件摘录</strong><p>工具栏动作、阅读侧栏、摘录工作页、可分离窗口。<br>读取邮件由用户逐封确认。</p></div><button class="toggle" role="switch" aria-label="启用邮件摘录示例" aria-checked="${extensionDemo.enabled}" data-ui-action="toggle-enabled"><span class="toggle-track"></span></button></div><p class="group-note">停用会一起移除该扩展的入口与窗口。默认功能也使用同一套贡献注册约定。</p>`);}
  if (!extensionDemo.enabled) return;
  const nav=document.querySelector('.nav-scroll');
  if(nav)nav.insertAdjacentHTML('beforeend',`<div class="nav-heading">扩展</div><button class="nav-item ${extensionDemo.page?'active':''}" data-ui-action="workspace" title="摘录集 · 邮件摘录插件">${icon('book')}<span class="nav-text">摘录集</span></button>`);
  const toolbar=document.querySelector('.reader-toolbar');
  if(toolbar)toolbar.insertAdjacentHTML('beforeend',extensionButton('open-panel','摘录','book','extension-toolbar-button'));
  const readerEl=document.querySelector('.reader'),scroll=document.querySelector('.reader-scroll');
  if(readerEl&&scroll&&!extensionDemo.page&&['panel','window'].includes(extensionDemo.placement)){
    readerEl.classList.add('extension-docked');
    const layout=document.createElement('div');layout.className='reader-extension-layout';readerEl.insertBefore(layout,scroll);layout.append(scroll);
    if(extensionDemo.placement==='window')layout.insertAdjacentHTML('beforeend',extensionWindowPlaceholder());
    else layout.insertAdjacentHTML('beforeend',`<aside class="extension-panel" aria-label="邮件摘录扩展面板">${extensionPanelContent()}</aside>`);
  }
  sendExtensionState();
}
render = function() { originalRender(); mountExtensionDemo(); };
function rerenderExtension() {
  const scroll=document.querySelector('.reader-scroll'),top=scroll?.scrollTop;
  render();if(top!=null&&document.querySelector('.reader-scroll'))document.querySelector('.reader-scroll').scrollTop=top;
}
function openExtensionPanel() {
  extensionDemo.enabled=true;extensionDemo.page=false;
  extensionDemo.placement=extensionDemo.child&&!extensionDemo.child.closed?'window':'panel';
  state.route='mail';state.mobileReader=true;rerenderExtension();
}
function detachExtension() {
  if(extensionDemo.child&&!extensionDemo.child.closed){extensionDemo.child.focus();return;}
  const url=new URL(location.href);url.search='';url.hash='';url.searchParams.set('surface','maild-notes-demo');
  const child=window.open(url.toString(),'maild-notes-design-window','popup,width=820,height=680');
  if(!child){toast('浏览器阻止了弹窗。请允许本页弹窗后重试，摘录仍保留在这里。');return;}
  extensionDemo.child=child;extensionDemo.placement='window';rerenderExtension();
}
function dockExtension(handedOff = false) {
  if (!handedOff && extensionDemo.child && !extensionDemo.child.closed) {
    clearTimeout(extensionHandoffTimer);
    extensionDemo.child.postMessage({kind:'maild-notes:handoff'},surfaceMessageOrigin);
    extensionHandoffTimer=setTimeout(()=>toast('窗口尚未完成内容交接，原编辑窗口已保留。请稍后再试。'),2000);
    return;
  }
  clearTimeout(extensionHandoffTimer);
  extensionDemo.placement='panel';extensionDemo.page=false;
  const child=extensionDemo.child;extensionDemo.child=null;child?.close();
  state.route='mail';state.mobileReader=true;rerenderExtension();
}
function renderExtensionChild() {
  document.body.classList.add('extension-window');
  const data=childView;
  if(!data){app.innerHTML=`<main id="workspace" class="page"><div class="empty">${icon('monitor')}<h2>邮件摘录 · 示例插件</h2><p>${window.opener?'正在连接主窗口…':'请从主窗口的摘录面板打开这个视图。'}</p></div></main>`;return;}
  document.title='邮件摘录 · 示例插件 · Maild';document.body.classList.toggle('dark',data.theme==='dark');
  const active=document.activeElement?.id==='extension-child-note';
  if(active){const el=document.querySelector('#extension-child-note');if(el.value===data.note)return;}
  app.innerHTML=`<main id="workspace" class="page"><header class="page-toolbar"><span class="extension-symbol">${icon('book')}</span><h1>邮件摘录</h1><span class="spacer"></span><span class="tag">示例插件 · Maild</span>${extensionButton('child-dock','收回主窗口','back','secondary')}</header><div class="page-scroll"><div class="page-column extension-child-column"><span class="extension-eyebrow">独立扩展窗口</span><h2>${esc(data.title)}</h2>${data.allowed?`<div class="extension-excerpt"><span class="extension-eyebrow">邮件摘要</span><p>${esc(data.excerpt)}</p><span class="extension-scope">${icon('lock')}仅这封邮件</span></div>`:`<div class="extension-permission"><p>读取范围仍需你授权。分离窗口不会增加权限。</p>${extensionButton('child-grant','允许读取这封邮件','','primary')}</div>`}<label class="extension-note-label" for="extension-child-note">我的摘录</label><textarea id="extension-child-note" maxlength="16000" placeholder="写下要保留的想法…">${esc(data.note)}</textarea><p class="extension-note-status">已暂存在主页面内存中，收回后继续编辑。</p><p class="extension-footnote">设计演示，不连接真实邮箱。浏览器主页面关闭会结束本次演示。</p></div></div></main>`;
}
document.addEventListener('click',e=>{
  const control=e.target.closest('[data-ui-action],[data-ui-note]');if(!control)return;
  const action=control.dataset.uiAction;
  if(control.dataset.uiNote){if(extensionDemo.child&&!extensionDemo.child.closed){extensionDemo.child.focus();toast('摘录仍在独立窗口中，请先收回再切换。');return;}extensionDemo.context=Number(control.dataset.uiNote);state.selected=extensionDemo.context;openExtensionPanel();return;}
  if(extensionDemoChild){if(action==='child-dock')window.opener?.postMessage({kind:'maild-notes:dock',note:document.querySelector('#extension-child-note')?.value||''},surfaceMessageOrigin);if(action==='child-grant')window.opener?.postMessage({kind:'maild-notes:grant'},surfaceMessageOrigin);return;}
  switch(action){
    case 'show-demo':if(extensionDemo.child&&!extensionDemo.child.closed){extensionDemo.child.focus();return;}extensionDemo.enabled=true;extensionDemo.context=samples.find(m=>m.id===state.selected&&m.folder==='inbox')?.id||samples.find(m=>m.folder==='inbox')?.id||1;state.selected=extensionDemo.context;state.folder='inbox';state.account='all';state.scenario='normal';openExtensionPanel();break;
    case 'open-panel':if(extensionDemo.placement==='closed'&&(!extensionDemo.child||extensionDemo.child.closed))extensionDemo.context=state.selected||1;openExtensionPanel();break;
    case 'close-panel':extensionDemo.placement='closed';rerenderExtension();break;
    case 'grant':extensionDemo.grants.add(extensionDemo.context);rerenderExtension();break;
    case 'follow-current':extensionDemo.context=state.selected;rerenderExtension();break;
    case 'workspace':extensionDemo.enabled=true;extensionDemo.page=true;state.route='mail';rerenderExtension();break;
    case 'back-mail':extensionDemo.page=false;state.route='mail';rerenderExtension();break;
    case 'detach':detachExtension();break;
    case 'focus-window':if(extensionDemo.child&&!extensionDemo.child.closed)extensionDemo.child.focus();else {extensionDemo.placement='panel';rerenderExtension();}break;
    case 'dock':dockExtension();break;
    case 'toggle-enabled':extensionDemo.enabled=!extensionDemo.enabled;if(!extensionDemo.enabled){extensionDemo.placement='closed';extensionDemo.page=false;extensionDemo.grants.clear();extensionDemo.child?.close();extensionDemo.child=null;}rerenderExtension();toast(extensionDemo.enabled?'演示：邮件摘录扩展已启用。':'演示：扩展入口、面板和窗口已停用。');break;
  }
});
// Base navigation exits the contributed workspace; other UI state stays intact.
document.addEventListener('click',e=>{
  const base=e.target.closest('[data-route],[data-folder],[data-account],[data-setting]');
  if(base&&!extensionDemoChild&&extensionDemo.page){extensionDemo.page=false;render();}
});
document.addEventListener('input',e=>{
  if(e.target.id==='extension-note'){extensionDemo.notes.set(extensionDemo.context,e.target.value);const label=document.querySelector('.extension-note-status');if(label)label.textContent='已暂存 · 本次演示';}
  if(e.target.id==='extension-child-note'){if(childView)childView.note=e.target.value;window.opener?.postMessage({kind:'maild-notes:changed',note:e.target.value},surfaceMessageOrigin);}
});
window.addEventListener('message',e=>{
  if(e.origin!==receivedMessageOrigin||!e.data||typeof e.data.kind!=='string')return;
  if(extensionDemoChild){if(e.source!==window.opener)return;if(e.data.kind==='maild-notes:handoff'){window.opener.postMessage({kind:'maild-notes:dock',note:document.querySelector('#extension-child-note')?.value||''},surfaceMessageOrigin);return;}if(e.data.kind!=='maild-notes:state')return;childView=e.data.data;renderExtensionChild();return;}
  if(e.source!==extensionDemo.child||!extensionDemo.enabled)return;
  switch(e.data.kind){
    case 'maild-notes:ready':sendExtensionState();break;
    case 'maild-notes:changed':if(typeof e.data.note==='string'&&e.data.note.length<=16000)extensionDemo.notes.set(extensionDemo.context,e.data.note);break;
    case 'maild-notes:grant':extensionDemo.grants.add(extensionDemo.context);sendExtensionState();break;
    case 'maild-notes:dock':if(typeof e.data.note==='string'&&e.data.note.length<=16000){extensionDemo.notes.set(extensionDemo.context,e.data.note);dockExtension(true);}break;
    case 'maild-notes:closed':extensionDemo.child=null;if(extensionDemo.placement==='window')extensionDemo.placement='closed';rerenderExtension();break;
  }
});
window.addEventListener('beforeunload',()=>{
  if(extensionDemoChild)window.opener?.postMessage({kind:'maild-notes:closed'},surfaceMessageOrigin);
  else extensionDemo.child?.close();
});
if(extensionDemoChild)window.opener?.postMessage({kind:'maild-notes:ready'},surfaceMessageOrigin);
mountExtensionDemo();
