/* Design-only fixture. No RPC, network, credentials, or production persistence. */
'use strict';
const paths = {
  inbox:'M3 4h18v16H3z M3 13h5l2 3h4l2-3h5', mail:'M3 5h18v14H3z M3 6l9 7 9-7',
  compose:'M13 5l6 6 M4 20l4-1L21 6l-5-5L3 14z', search:'M10 3a7 7 0 1 0 0 14 7 7 0 0 0 0-14 M15 15l6 6',
  star:'M12 3l2.8 5.7 6.2.9-4.5 4.4 1.1 6.2L12 17.3l-5.6 2.9 1.1-6.2L3 9.6l6.2-.9z',
  send:'M3 3l19 9-19 9 4-9z M7 12h15', draft:'M5 3h10l4 4v14H5z M14 3v5h5 M8 12h8 M8 16h5',
  archive:'M4 8h16v13H4z M3 3h18v5H3z M9 12h6', trash:'M3 6h18 M9 6V3h6v3 M5 6l1 15h12l1-15 M10 10v7 M14 10v7',
  folder:'M3 5h7l2 3h9v12H3z', chevron:'M9 5l7 7-7 7', down:'M6 9l6 6 6-6',
  back:'M14 5l-7 7 7 7', arrow:'M5 12h14 M13 6l6 6-6 6', plus:'M12 4v16 M4 12h16',
  settings:'M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1z M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8',
  more:'M5 12h.01 M12 12h.01 M19 12h.01', reply:'M9 5l-6 6 6 6 M3 11h11a7 7 0 0 1 7 7',
  replyAll:'M7 5l-5 6 5 6 M12 5l-5 6 5 6 M7 11h7a7 7 0 0 1 7 7',
  shield:'M12 2l8 4v6c0 5-8 10-8 10S4 17 4 12V6z M8 12l3 3 5-6',
  clip:'M8 12l7-7a4 4 0 0 1 6 6L10 22a6 6 0 0 1-8-8L13 3 M5 17l10-10',
  close:'M6 6l12 12 M18 6L6 18', check:'M4 12l5 5L20 6', cloud:'M6 18a5 5 0 0 1-1-10 7 7 0 0 1 13-1 5.5 5.5 0 0 1 0 11H6',
  offline:'M3 3l18 18 M6 18a5 5 0 0 1-1-10 M8 5a7 7 0 0 1 10 2 5.5 5.5 0 0 1 3 9',
  refresh:'M20 7V3l-3 3a8 8 0 1 0 3 10 M20 7h-5', moon:'M21 13A9 9 0 0 1 11 3a9 9 0 1 0 10 10',
  sun:'M12 3v1 M12 20v1 M3 12h1 M20 12h1 M5 5l1 1 M18 18l1 1 M5 19l1-1 M18 6l1-1 M12 7a5 5 0 1 0 0 10 5 5 0 0 0 0-10',
  filter:'M4 6h16 M7 12h10 M10 18h4', menu:'M4 5h16 M4 12h16 M4 19h16',
  users:'M9 3a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M2 21v-3a7 7 0 0 1 14 0v3 M17 3a4 4 0 0 1 0 8 M19 15a5 5 0 0 1 3 5',
  bell:'M5 17V9a7 7 0 0 1 14 0v8l2 2H3z M9 22h6',
  puzzle:'M3 3h7a3 3 0 1 1 4 0h7v7a3 3 0 1 0 0 4v7h-7a3 3 0 1 0-4 0H3v-7a3 3 0 1 0 0-4z',
  at:'M16 8v6a3 3 0 0 0 6 0v-2a10 10 0 1 0-4 8 M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8',
  monitor:'M3 3h18v13H3z M12 16v5 M8 21h8', help:'M9 8a3 3 0 0 1 6 0c0 3-3 3-3 6 M12 18v.01 M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20',
  alert:'M12 3L2 21h20z M12 9v5 M12 17v.01', download:'M12 3v12 M7 10l5 5 5-5 M4 16v5h16v-5',
  book:'M3 3h7l2 2 2-2h7v17h-7l-2 2-2-2H3z M12 5v17', lock:'M5 10h14v11H5z M8 10V6a4 4 0 0 1 8 0v4'
};
const icon = name => `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="${paths[name] || paths.mail}"/></svg>`;
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const button = (name,label,action,extra='') => `<button class="icon-button ${extra}" data-action="${action}" title="${label}" aria-label="${label}">${icon(name)}</button>`;
const samples = [
  {id:1, sender:'沿海来信', address:'letters@coast.example', subject:'沿海来信 Vol. 018 · 把周末留给海边', snippet:'九月的海风、一本旧书，和一条不赶时间的路。', time:'10:42', account:'personal', folder:'inbox', unread:true, star:false, kind:'newsletter'},
  {id:2, sender:'周予安', address:'yuan@shanhai.example', subject:'秋季刊印刷确认', snippet:'第三版的纸样收到了，我们就按这个方向推进。', time:'10:18', account:'work', folder:'inbox', unread:true, star:true, attach:true, kind:'thread', count:3},
  {id:3, sender:'行间书店', address:'orders@hangjian.example', subject:'订单 HJ-0911 已发出', snippet:'你订的《海边的房间》已经出发，预计周六送达。', time:'09:36', account:'personal', folder:'inbox', unread:true, star:false, kind:'order'},
  {id:4, sender:'山海工作室 · 财务', address:'finance@shanhai.example', subject:'九月印刷费用电子发票', snippet:'本次印刷费用 ¥2,860.00，电子发票见附件。', time:'09:04', account:'work', folder:'inbox', unread:false, star:false, attach:true, kind:'invoice'},
  {id:5, sender:'GitHub', address:'notifications@github.example', subject:'[maild] Folder sync checks passed', snippet:'All checks have passed for your latest changes.', time:'昨天', account:'dev', folder:'inbox', unread:false, star:false, kind:'text'},
  {id:6, sender:'林闻溪', address:'wenxi@studio.example', subject:'周六去看展吗？', snippet:'看了展览的介绍，里面有一组手工装帧很喜欢。', time:'昨天', account:'personal', folder:'inbox', unread:false, star:true, kind:'personal'},
  {id:7, sender:'松间民宿', address:'stay@songjian.example', subject:'你的入住确认 · 9 月 19 日', snippet:'房间已经为你留好。沿山路走到尽头就能看见我们。', time:'周三', account:'personal', folder:'inbox', unread:false, star:false, kind:'text'},
  {id:8, sender:'我 → 周予安', address:'qing@work.example', subject:'Re: 秋季刊印刷确认', snippet:'纸样没有问题，请保留封面原来的自然色。', time:'周三', account:'work', folder:'sent', unread:false, star:false, kind:'thread'},
  {id:9, sender:'城市步行通讯', address:'walk@city.example', subject:'一条值得绕路的街', snippet:'从河岸到旧街区，走过五家开了很久的小店。', time:'周二', account:'personal', folder:'archive', unread:false, star:false, kind:'text'}
];
const accountNames = {all:'全部账户',personal:'个人邮箱',work:'工作邮箱',dev:'开发订阅'};
const folderNames = {inbox:'收件箱',starred:'星标邮件',drafts:'草稿',sent:'已发送',archive:'归档',trash:'废纸篓',spam:'垃圾邮件'};
const state = { route:'mail', folder:'inbox', account:'all', selected:1, checked:new Set(), filter:'all', search:'', scenario:'normal', setting:'accounts', theme:'light', mobileReader:false, compact:false, remote:false, history:false, reply:false, inlineText:'', replyDrafts:{}, cc:false, draft:{to:'',cc:'',bcc:'',subject:'',body:''}, draftOpen:false, draftError:'', sendState:'idle', wizard:0, wizardData:{}, wizardError:'', queue:[], toggles:{remote:false,notification:true,preview:false,background:true}, searchAttachment:false, searchAccount:'all', searchPeriod:'all'};
const app = document.querySelector('#app');
let toastTimer, dialogTrigger;
function toast(message) { const el=document.querySelector('#toast'); el.textContent=message; el.classList.add('show'); clearTimeout(toastTimer); toastTimer=setTimeout(()=>el.classList.remove('show'),3600); }
function navItem(key,name,count,ico) { return `<button class="nav-item ${state.folder===key && state.route==='mail'?'active':''}" data-folder="${key}" title="${name}" ${state.folder===key?'aria-current="page"':''}>${icon(ico)}<span class="nav-text">${name}</span>${count?`<span class="counter ${key==='inbox'?'count-badge':''}">${count}</span>`:''}</button>`; }
function navContent() {
  const unread=samples.filter(m=>m.folder==='inbox'&&m.unread).length;
  return `${navItem('inbox','收件箱',unread,'inbox')}${navItem('starred','星标邮件','','star')}${navItem('drafts','草稿',state.draftOpen?1:'','draft')}${navItem('sent','已发送','','send')}${navItem('archive','归档','','archive')}${navItem('trash','废纸篓','','trash')}
  <div class="nav-heading">邮箱账户<button data-action="add-account" aria-label="添加账户">${icon('plus')}</button></div>
  ${Object.entries(accountNames).filter(([key])=>key!=='all').map(([key,name],i)=>`<button class="nav-item ${state.account===key?'active':''}" data-account="${key}" title="${name}"><span class="account-dot ${['','blue','orange'][i]}"></span><span class="nav-text">${name}</span><span class="counter">${icon('down')}</span></button>${state.account===key?`<div class="account-address">qing@${key==='work'?'work':'mail'}.example</div><button class="nav-item subfolder" data-folder="inbox">${icon('inbox')}收件箱</button><button class="nav-item subfolder" data-folder="spam">${icon('shield')}垃圾邮件</button><button class="nav-item subfolder" data-action="custom-folder">${icon('folder')}项目资料</button>`:''}`).join('')}
  ${state.account!=='all'?'<button class="nav-item" data-account="all">'+icon('users')+'<span class="nav-text">返回全部账户</span></button>':''}`;
}
function sidebar() {
  const warning=['offline','daemon','auth','unknown','incompatible'].includes(state.scenario);
  const status={offline:'工作邮箱离线',daemon:'后台未连接',auth:'工作邮箱需要登录',unknown:'1 项操作待确认',incompatible:'需要更新应用'}[state.scenario] || (state.queue.length?`${state.queue.length} 项操作待同步`:'所有邮箱已同步');
  return `<aside class="sidebar" aria-label="邮箱导航"><div class="brandbar"><span class="traffic" aria-hidden="true"><i></i><i></i><i></i></span><span class="brand">Maild</span></div><button class="primary compose-button" data-route="compose" title="写邮件">${icon('compose')}<span class="nav-text">写邮件</span><kbd>⌘ N</kbd></button><nav class="nav-scroll" aria-label="文件夹与账户">${navContent()}</nav><div class="sidebar-bottom"><button class="status-button" data-action="activity" title="${status}"><span class="status-dot ${warning?'warn':''}"></span><span class="status-text">${status}</span></button><div class="bottom-line"><div class="profile"><span class="avatar small">Q</span>Qing</div>${button('settings','设置','settings')}</div></div></aside>`;
}
function currentMessages() {
  if (state.scenario==='empty'||state.scenario==='noresults') return [];
  let rows=samples.filter(m=> state.route==='search' ? m.folder!=='trash' : state.folder==='starred'?m.star: m.folder===state.folder);
  if (state.account!=='all' && state.route!=='search') rows=rows.filter(m=>m.account===state.account);
  if (state.filter==='unread') rows=rows.filter(m=>m.unread);
  if (state.route==='search') { const q=state.search.trim().toLowerCase(); if(q) rows=rows.filter(m=>`${m.subject} ${m.sender} ${m.snippet}`.toLowerCase().includes(q)); if(state.searchAttachment)rows=rows.filter(m=>m.attach); if(state.searchAccount!=='all')rows=rows.filter(m=>m.account===state.searchAccount); if(state.searchPeriod==='today')rows=rows.filter(m=>m.time.includes(':')); }
  return rows;
}
function messageRow(m) {
  return `<div class="message-row ${state.selected===m.id?'selected':''} ${m.unread?'unread':''}"><label class="select-cell"><input type="checkbox" data-select="${m.id}" ${state.checked.has(m.id)?'checked':''} aria-label="选择 ${esc(m.subject)}"></label><button class="message-hit" data-message="${m.id}" ${state.selected===m.id?'aria-current="true"':''}><span class="message-top">${m.unread?'<span class="unread-dot" aria-label="未读"></span>':''}<span class="sender">${esc(m.sender)}${m.count?` <span class="quiet">${m.count}</span>`:''}</span><time>${m.time}</time></span><span class="subject-line">${m.attach?'↳ ':''}${esc(m.subject)}</span><span class="snippet">${esc(m.snippet)}</span></button><button class="message-star ${m.star?'starred':''}" data-star="${m.id}" aria-label="${m.star?'取消星标':'添加星标'} ${esc(m.subject)}" aria-pressed="${m.star}">${icon('star')}</button></div>`;
}
function listPane() {
  const search=state.route==='search', rows=currentMessages();
  return `<section class="list-pane" aria-label="邮件列表"><div class="list-header">${button('menu','展开邮箱导航','nav','mobile-nav')}<form id="search-form" class="searchbox">${icon('search')}<input id="search-input" aria-label="搜索邮件" placeholder="搜索邮件" value="${esc(search?state.search:'')}"><kbd>⌘ K</kbd></form></div><div class="list-title"><h1>${search?'搜索结果':folderNames[state.folder]||'项目资料'}</h1><span class="total">${rows.length} 封邮件</span></div>
  ${state.checked.size?`<div class="list-controls"><span class="quiet">已选 ${state.checked.size} 封</span>${button('archive','归档选中邮件','bulk-archive')}${button('trash','删除选中邮件','bulk-trash')}${button('close','取消选择','clear-select')}</div>`:`<div class="list-controls"><button class="pill ${state.filter==='all'?'active':''}" data-filter="all">全部</button><button class="pill ${state.filter==='unread'?'active':''}" data-filter="unread">未读</button>${button('filter','列表密度与显示','density')}</div>`}
  ${search?`<div class="search-filters"><select id="search-account" aria-label="搜索账户">${Object.entries(accountNames).map(([k,v])=>`<option value="${k}" ${state.searchAccount===k?'selected':''}>${v}</option>`).join('')}</select><select id="search-period" aria-label="时间范围"><option value="all">全部时间</option><option value="today" ${state.searchPeriod==='today'?'selected':''}>今天</option></select><button class="secondary ${state.searchAttachment?'active':''}" data-action="search-attachment">${icon('clip')}有附件 ${state.searchAttachment?'✓':''}</button><button class="text-button" data-action="clear-search">清除筛选</button></div>`:''}
  ${state.scenario==='indexing'?'<div class="list-banner">正在建立搜索索引，结果尚未完整。<br>还有 1,248 封历史邮件待处理。</div>':''}
  <div class="message-list">${rows.length?`<div class="date-label">${search?'按时间从新到旧':'最近邮件'}</div>${rows.map(messageRow).join('')}`: `<div class="empty">${icon(search?'search':'inbox')}<h2>${search?'没有找到邮件':'这里暂时没有邮件'}</h2><p>${search?'试试更短的关键词，或移除部分筛选条件。':'新邮件到达后会显示在这里。'}</p>${search?'<button class="secondary" data-action="clear-search">清除筛选</button>':''}</div>`}</div><div class="list-foot"><span>${search?'本地邮件 · 按时间排序':accountNames[state.account]}</span><span>已更新 10:45</span></div></section>`;
}
function newsletter() {
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:"><style>*{box-sizing:border-box}body{margin:0;background:#f7f5ef;color:#283c32;font-family:-apple-system,'PingFang SC',sans-serif}header{padding:25px 30px 19px;border-bottom:1px solid #d5d8ca;display:flex;justify-content:space-between;font-size:10px;letter-spacing:2px}main{padding:30px}h1{font-family:'Songti SC',Georgia,serif;font-weight:500;font-size:40px;line-height:1.4;letter-spacing:1px;margin:10px 0 18px}p{font-size:12px;line-height:2;color:#546359;margin:12px 0}small{font-size:9px;letter-spacing:2px;color:#617568}.topline{width:26px;height:2px;background:#617568;margin:0 0 20px}.image-note{margin:25px 0;padding:22px;background:#e5e8dc;border-radius:2px;color:#50614f;font-size:11px;text-align:center}table{border-collapse:collapse;width:100%;margin-top:25px}td{padding:15px 0;vertical-align:top;border-top:1px solid #d5d8ca;font-size:11px;line-height:1.9}td:first-child{width:26%;color:#667762}footer{padding:18px 30px;font-size:9px;color:#637065;border-top:1px solid #d5d8ca}@media(max-width:420px){main{padding:24px}h1{font-size:32px}header{padding-inline:24px}}</style></head><body><header><b>沿海来信</b><span>COAST LETTERS / 018</span></header><main><div class="topline"></div><small>SEPTEMBER NOTES</small><h1>把周末<br>留给海边。</h1><p>嗨，Qing，</p><p>九月的海岸安静了下来。走到堤坝尽头，<br>风里有一点咸，也有一点秋天的味道。</p><p>这期来信，想和你分享两个不赶时间的去处。</p><div class="image-note">${state.remote?'本封图片已放行 · 原型仅模拟状态，不发起网络请求':'海岸照片 · 远程图片已隐藏'}</div><table><tr><td>01 / 去走走</td><td><b>沿着旧灯塔，走到日落</b><br>一条长 2.4 公里的海边步道。不需要导航。</td></tr><tr><td>02 / 去读读</td><td><b>藏在码头边的书店</b><br>木窗、旧地图，以及一杯刚好的热茶。</td></tr></table></main><footer>你订阅了沿海来信，因此收到这封邮件。<br>原创演示刊物 · 全部内容为设计样本</footer></body></html>`;
}
function bodyMarkup(m) {
  if(state.scenario==='loading')return '<div aria-busy="true" aria-label="邮件正文加载中"><div class="skeleton short"></div><div class="skeleton"></div><div class="skeleton"></div><div class="skeleton body"></div><p class="quiet">正在读取邮件内容…</p></div>';
  if(state.scenario==='htmlerror')return '<div class="empty">'+icon('alert')+'<h2>这封邮件暂时无法显示</h2><p>原有内容仍在，重试不会改变邮件。</p><button class="secondary" data-action="retry-html">重新加载 HTML</button></div>';
  if(m.kind==='newsletter')return `<div class="privacy-strip">${icon('shield')}<span>${state.remote?'本封邮件的远程图片已放行':'为保护隐私，已阻止远程图片'}</span><button class="text-button" data-action="remote">${state.remote?'恢复拦截':'加载本封图片'}</button></div><iframe class="mail-frame" title="沿海来信 HTML 邮件正文" sandbox="" referrerpolicy="no-referrer" srcdoc="${esc(newsletter())}" style="height:800px"></iframe>`;
  if(m.kind==='thread')return `${state.history?'<div class="thread-history"><small>周予安 · 昨天 14:20 · 收件箱</small><p>纸样已经寄出，封面我们选了自然白。麻烦确认一下用纸和数量。</p><small>我 · 昨天 16:48 · 已发送</small><p>纸样没有问题，请保留封面原来的自然色。</p></div>':''}<button class="thread-summary" data-action="history">${icon('down')}${state.history?'收起':'展开'}此前 2 封邮件<span class="spacer"></span>同一会话</button><div class="letter"><p>Qing，上午好，</p><p>第三版的纸样收到了，质感很合适。我们就按这个方向推进。</p><p>封面采用 240g 自然白，内页 100g 米白。装订方式仍然是裸脊锁线，计划下周三开始印刷。</p><p>附件是最终的印刷确认单，麻烦再核对一下数量和交付地址。如果没有调整，我今天下午就安排。</p><p>谢谢，<br>予安</p></div>${attachment('秋季刊_印刷确认单.pdf','246 KB')}`;
  if(m.kind==='invoice')return `<div class="letter"><p>Qing，你好，</p><p>九月的印刷费用电子发票已开具，请查收附件。</p><table style="width:100%;border-collapse:collapse"><tbody><tr><td style="padding:16px 0;border-bottom:1px solid var(--line)">项目</td><td>秋季刊第三版印刷</td></tr><tr><td style="padding:16px 0;border-bottom:1px solid var(--line)">金额</td><td>¥2,860.00</td></tr><tr><td style="padding:16px 0">开票日期</td><td>2026 年 9 月 11 日</td></tr></tbody></table><p>如需更正开票信息，请直接回复此邮件。</p><p>山海工作室 · 财务</p></div>${attachment('电子发票_20260911.pdf','138 KB')}`;
  if(m.kind==='personal')return '<div class="letter"><p>Qing，</p><p>周六去看展吗？看了展览的介绍，里面有一组手工装帧很喜欢。</p><p>如果你有空，我们下午两点在美术馆门口见，看完还可以去河边走走。</p><p>闻溪</p></div>';
  return `<div class="letter"><p>Qing，你好，</p><p>${esc(m.snippet)}</p><p>${m.kind==='order'?'你可以通过订单邮件核对寄送信息。如果有问题，直接回复这封邮件就能联系我们。':'这是一封用于检查长文本、邮件选择和阅读排版的演示邮件。'}</p><p>祝好，<br>${esc(m.sender)}</p></div>`;
}
function attachment(name,size){return `<div class="attachments"><button class="attachment" data-action="attachment"><span class="file-icon">PDF</span><span><strong>${name}</strong><small>${size} · 附件</small></span>${icon('download')}</button></div>`;}
function scenarioBanner() {
  const messages={offline:'工作邮箱离线。已缓存的邮件仍可阅读，标记与移动会等待重连。',daemon:'尚未连接邮件后台。当前画面是已有缓存，暂时不能提交修改。',auth:'工作邮箱登录已失效。其他邮箱仍在正常同步。',unknown:'有一项归档结果需要确认，重复操作可能产生副本。',incompatible:'应用与邮件后台版本不匹配。更新前仅可阅读已有内容。'};
  return messages[state.scenario]?`<div class="banner ${state.scenario==='auth'?'error':''}">${icon('alert')}<span>${messages[state.scenario]}</span><button data-action="activity">查看</button></div>`:'';
}
function reader() {
  const m=currentMessages().find(m=>m.id===state.selected);
  if(!m)return `<section class="reader" aria-label="邮件正文"><div class="reader-toolbar"><span class="spacer"></span>${button('compose','写邮件','compose')}</div>${scenarioBanner()}<div class="empty">${icon('mail')}<h2>留一点空间给阅读</h2><p>选择一封邮件，在这里查看完整内容。</p></div></section>`;
  const disabled=['daemon','incompatible'].includes(state.scenario)?'disabled':'';
  return `<section class="reader" aria-label="邮件正文"><div class="reader-toolbar">${button('back','返回列表','back-list','back-to-list')}<button class="icon-button" data-action="archive" title="归档当前邮件" aria-label="归档当前邮件" ${disabled}>${icon('archive')}</button><button class="icon-button" data-action="trash" title="移到废纸篓" aria-label="移到废纸篓" ${disabled}>${icon('trash')}</button>${button('folder','移动到文件夹','move')}<span class="toolbar-divider"></span>${button('mail',m.unread?'标为已读':'标为未读','read')}${button('more','更多邮件操作','more')}<span class="spacer"></span><span class="toolbar-label">${currentMessages().findIndex(x=>x.id===m.id)+1} / ${currentMessages().length}</span>${button('back','上一封','previous','next-prev')}${button('chevron','下一封','next','next-prev')}</div>${scenarioBanner()}<div class="reader-scroll"><div class="reader-inner"><div class="reader-path"><span class="source-chip"><span class="status-dot"></span>${accountNames[m.account]}</span><span> / </span><span>${folderNames[m.folder]}</span></div><h1>${esc(m.subject)}</h1><div class="sender-row"><span class="avatar ${m.kind==='newsletter'?'sand':''}">${esc(m.sender.slice(0,1))}</span><div class="sender-info"><strong>${esc(m.sender)}</strong><span class="address">${esc(m.address)}</span><details class="recipient-line"><summary>发送至我${m.kind==='thread'?'、陈以宁':''}</summary><p>收件人：Qing &lt;qing@${m.account==='work'?'work':'mail'}.example&gt;${m.kind==='thread'?'<br>抄送：陈以宁 &lt;yining@shanhai.example&gt;':''}<br>2026 年 9 月 11 日 ${m.time}</p></details></div><time>${m.time.includes(':')?'今天 ':''}${m.time}</time></div>${bodyMarkup(m)}<div class="reply-actions"><button class="secondary" data-action="reply" ${disabled}>${icon('reply')}回复</button>${m.kind==='thread'?`<button class="secondary" data-action="reply-all" ${disabled}>${icon('replyAll')}全部回复</button>`:''}<button class="quiet" data-action="forward" ${disabled}>${icon('arrow')}转发</button></div>${state.reply?inlineComposer(m):''}</div></div></section>`;
}
function inlineComposer(m){return `<section class="inline-compose" aria-label="回复编辑器"><header>${icon('reply')}<strong>${state.reply==='all'?'全部回复':'回复'}</strong><span>${esc(m.sender)}${state.reply==='all'?'、陈以宁':''}</span><span class="spacer"></span>${button('close','收起回复草稿','collapse-reply')}</header><textarea id="inline-body" aria-label="回复正文" placeholder="写下你的回复…">${esc(state.inlineText)}</textarea><footer><button class="primary" data-action="send-reply">${icon('send')}发送</button><span class="quiet">${state.inlineText?'已暂存 · 本次演示':'⌘ Enter 发送'}</span><span class="spacer"></span>${button('trash','丢弃回复草稿','discard-reply')}</footer></section>`;}
function composePage(){
  return `<section class="page compose-page"><header class="page-toolbar">${button('back','返回收件箱并暂存草稿','save-draft')}<h1>新邮件</h1><small>保留在本次演示中</small>${button('more','写信选项','compose-options')}</header>${scenarioBanner()}<div class="page-scroll"><form id="compose-form" class="page-column"><div class="compose-field"><label for="compose-from">发件人</label><select id="compose-from"><option>Qing &lt;qing@work.example&gt; · 工作邮箱</option><option>Qing &lt;qing@mail.example&gt; · 个人邮箱</option><option>hello@work.example · 工作别名</option></select></div><div class="compose-field"><label for="compose-to">收件人</label><input id="compose-to" data-draft="to" placeholder="输入邮箱地址" value="${esc(state.draft.to)}" aria-describedby="compose-error"><button type="button" class="text-button" data-action="cc">抄送 / 密送</button></div>${state.cc?`<div class="compose-field"><label for="compose-cc">抄送</label><input id="compose-cc" data-draft="cc" placeholder="多个地址用逗号分隔" value="${esc(state.draft.cc)}"></div><div class="compose-field"><label for="compose-bcc">密送</label><input id="compose-bcc" data-draft="bcc" placeholder="其他收件人不会看到这些地址" value="${esc(state.draft.bcc)}"></div>`:''}<div class="compose-field"><label for="compose-subject">主题</label><input id="compose-subject" class="compose-subject" data-draft="subject" placeholder="添加主题" value="${esc(state.draft.subject)}"></div><div id="compose-error" class="field-error" role="alert">${esc(state.draftError)}</div><textarea id="compose-body" class="compose-textarea" data-draft="body" aria-label="邮件正文" placeholder="从一句问候开始…">${esc(state.draft.body)}</textarea>${state.draft.attachment?attachment('秋季刊_印刷确认单.pdf','246 KB'):''}<div class="compose-bottom"><button type="submit" class="primary" ${['sending','unknown'].includes(state.sendState)||['daemon','incompatible','offline'].includes(state.scenario)?'disabled':''}>${icon('send')}${state.sendState==='sending'?'正在发送…':state.sendState==='unknown'?'等待确认结果':'发送'}</button><kbd>⌘ Enter</kbd><span class="spacer"></span><button type="button" class="quiet" data-action="add-attachment">${icon('clip')}添加附件</button><span class="quiet">纯文本</span>${button('trash','丢弃草稿','discard-draft')}</div>${state.sendState==='unknown'?'<div class="banner">投递结果尚未确认。请先查看已发送或与收件人核对，避免重复投递。</div>':''}<p class="draft-note">${state.scenario==='offline'?'工作邮箱离线，请保留草稿后稍后发送。':'本次原型的草稿只留在当前页面，刷新后清除。正式版本需先补齐持久化。'}</p></form></div></section>`;
}
const settingPages=[['accounts','users','账户'],['appearance','monitor','外观与操作'],['privacy','shield','阅读与隐私'],['notifications','bell','通知与规则'],['addresses','at','地址与别名'],['extensions','puzzle','扩展'],['background','cloud','后台与存储']];
function toggle(key,label){return `<button class="toggle" role="switch" aria-label="${label}" aria-checked="${!!state.toggles[key]}" data-toggle="${key}"><span class="toggle-track"></span></button>`;}
function settingRow(title,desc,control){return `<div class="setting-row"><div class="setting-copy"><strong>${title}</strong><p>${desc}</p></div>${control}</div>`;}
function settingsBody(){
  switch(state.setting){
    case 'accounts': return `<h2>账户</h2><p class="page-intro">所有邮箱，各自井然有序。</p>${[['personal','Q','个人邮箱','qing@mail.example','收发正常'],['work','山','工作邮箱','qing@work.example',state.scenario==='auth'?'需要重新登录':'收发正常'],['dev','N','开发订阅','qing@nodeseek.example','通过 NodeSeek Email 接收']].map(([key,avatar,name,address,status])=>`<div class="account-card"><span class="avatar">${avatar}</span><div class="account-details"><strong>${name}</strong><p>${address}</p><div class="account-meta"><span class="status-dot ${status.includes('登录')?'warn':''}"></span>${status}</div></div><button class="secondary" data-manage-account="${key}">管理</button></div>`).join('')}<div class="setting-action-row"><button class="primary" data-action="add-account">${icon('plus')}添加邮箱账户</button></div><p class="group-note">暂停同步会保留本地邮件。移除账户前，会单独说明本地数据和服务器邮件的处理方式。</p>`;
    case 'appearance': return `<h2>外观与操作</h2><p class="page-intro">让阅读适应你的习惯。</p>${settingRow('外观','应用外壳的颜色，邮件默认保留发件人的原始配色。',`<select id="theme-select" aria-label="外观"><option value="light">浅色</option><option value="dark" ${state.theme==='dark'?'selected':''}>深色</option></select>`)}${settingRow('邮件列表密度','舒适显示摘要；紧凑显示更多邮件。',`<select id="density-select" aria-label="邮件列表密度"><option value="comfortable">舒适</option><option value="compact" ${state.compact?'selected':''}>紧凑</option></select>`)}${settingRow('键盘快捷键','在邮件正文中输入时，不会触发单键操作。','<button class="secondary" data-action="shortcuts">查看快捷键</button>')}<p class="section-label">窗口行为</p>${settingRow('关闭窗口后继续收信','后台邮件服务独立运行，下次打开回到上次阅读位置。',toggle('background','关闭窗口后继续收信'))}<p class="group-note">系统跟随、栏宽拖动和记忆阅读位置属于正式实现验收项。原型可检查固定断点和明暗配色。</p>`;
    case 'privacy': return `<h2>阅读与隐私</h2><p class="page-intro">完整阅读邮件，也保留自己的选择。</p>${settingRow('默认阻止远程图片','打开邮件不会因此向发件人请求图片。每封信都可单独放行。','<span class="tag">已开启</span>')}${settingRow('可信发件人','只有你主动添加的发件人，才能自动加载远程图片。','<button class="secondary" data-action="trusted">管理名单</button>')}${settingRow('邮件显示','HTML 邮件在当前阅读区打开，保留原始排版。','<span class="tag">原始 HTML</span>')}${settingRow('邮件中的链接','查看链接目标后，在系统浏览器打开。','<span class="quiet">系统浏览器</span>')}<p class="group-note">阻止远程资源可能影响图片或网络字体，不改成纯文本。加载失败可以重试；不默默替换成降级正文。</p>`;
    case 'notifications': return `<h2>通知与规则</h2><p class="page-intro">值得打断你的邮件，才打断你。</p>${settingRow('新邮件通知','仅收件箱来信通知；需系统已允许 Maild 发送通知。',toggle('notification','新邮件通知'))}${settingRow('通知显示正文预览','关闭后，锁屏通知只显示有新邮件。',toggle('preview','通知显示正文预览'))}<p class="section-label">当前运行规则</p>${settingRow('工作邮箱 · 静默时段','22:00 至次日 08:00，固定 UTC+08:00。','<button class="secondary" data-action="edit-rule">编辑</button>')}${settingRow('验证码通知','识别成功时在通知中提供复制入口。','<span class="tag">已开启</span>')}<p class="group-note">当前内核的规则修改只对本次运行生效，重启后恢复配置文件值。按钮使用“应用到本次运行”，不能误写为“保存”。</p>`;
    case 'addresses': return `<h2>地址与别名</h2><p class="page-intro">使用不同地址，仍回到同一个邮箱。</p><p class="section-label">工作邮箱 · qing@work.example</p>${settingRow('hello@work.example','工作室公开联系地址 · 可收发','<button class="secondary" data-action="manage-address">管理</button>')}${settingRow('press@work.example','刊物往来专用 · 可收发','<button class="secondary" data-action="manage-address">管理</button>')}<div class="setting-action-row"><button class="secondary" data-action="new-address">${icon('plus')}添加地址</button></div><p class="group-note">只展示账户提供方实际支持的地址能力；没有能力时说明原因，不显示无效的创建按钮。</p>`;
    case 'extensions': return `<h2>扩展</h2><p class="page-intro">需要时扩展，平时专注邮件。</p><p class="section-label">默认前端</p>${settingRow('Maild UI','随应用提供 · Tauri 前端 · 当前正在使用','<span class="tag">默认</span>')}<p class="section-label">邮箱服务</p>${settingRow('NodeSeek Email','支持 NodeSeek 邮箱。开发订阅账户正在使用。','<button class="secondary" data-action="plugin-detail">查看</button>')}<div class="setting-action-row"><button class="secondary" data-action="install-plugin">${icon('plus')}从本地文件安装</button></div><p class="group-note">安装前显示来源、版本与请求权限。禁用正在被账户使用的扩展前，明确列出受影响账户。没有扩展商店首页。</p>`;
    case 'background': return `<h2>后台与存储</h2><p class="page-intro">关掉窗口，邮件仍能到达。</p>${settingRow('邮件后台','正在运行 · 3 个账户 · 上次同步 10:45','<span class="tag">运行正常</span>')}${settingRow('同步活动','待同步的操作与需要处理的问题。','<button class="secondary" data-action="activity">查看活动</button>')}${settingRow('本地邮件','邮件与索引保存在这台 Mac 上。','<span class="quiet">2.4 GB · 演示</span>')}${settingRow('数据与恢复','查看备份与恢复说明；恢复前需要关闭邮件后台。','<button class="secondary" data-action="backup">查看说明</button>')}<p class="section-label">退出</p>${settingRow('停止后台收信','之后不会同步新邮件或发出通知，直到重新启动。','<button class="secondary danger" data-action="stop-background">停止后台</button>')}<p class="group-note">普通“关闭窗口”与“停止后台收信”分开，不把停止同步藏在窗口关闭按钮里。</p>`;
  }
}
function settingsPage(){return `<section class="page"><header class="page-toolbar">${button('back','返回邮件','back-mail')}<h1>设置</h1><small>Maild · 你的邮件工作台</small></header><div class="settings-layout"><nav class="settings-nav" aria-label="设置分类">${settingPages.map(([id,ico,title])=>`<button data-setting="${id}" class="${state.setting===id?'active':''}" ${state.setting===id?'aria-current="page"':''}>${icon(ico)}${title}</button>`).join('')}</nav><div class="settings-content"><div class="page-column">${settingsBody()}</div></div></section>`;}
function onboarding(){
  const d=state.wizardData;
  let body='';
  if(state.wizard===0)body=`<div class="step-label">WELCOME TO MAILD</div><h1>从你的第一个邮箱开始。</h1><p class="lead">邮件留在本地，阅读回到眼前。<br>连接现有邮箱，就可以开始使用。</p><button class="provider-choice" data-action="wizard-next"><span class="provider-symbol">${icon('mail')}</span><span class="provider-copy"><strong>连接邮箱</strong><small>使用 IMAP / SMTP 和邮箱应用专用密码</small></span>${icon('chevron')}</button><button class="provider-choice" data-action="wizard-provider"><span class="provider-symbol">${icon('puzzle')}</span><span class="provider-copy"><strong>通过已安装的邮箱扩展连接</strong><small>NodeSeek Email 及其他支持的服务</small></span>${icon('chevron')}</button><p class="onboard-foot">无需注册 Maild 账户 · 凭据交由系统钥匙串保管<br>原型只演示流程，请勿输入真实邮箱密码。</p>`;
  if(state.wizard===1)body=`<div class="step-label">01 / 连接邮箱</div><h1>填入邮箱连接信息。</h1><p class="lead">从邮箱服务商获取服务器地址和应用专用密码。</p><form id="wizard-form"><div class="form-grid"><label class="full">邮箱地址<input id="wizard-address" type="email" required placeholder="qing@mail.example" value="${esc(d.address||'')}"></label><label class="full">显示名称<input id="wizard-name" placeholder="个人邮箱" value="${esc(d.name||'')}"></label><label>IMAP 服务器<input id="wizard-imap" required placeholder="imap.mail.example" value="${esc(d.imap||'')}"></label><label>端口与加密<select aria-label="IMAP 端口与加密"><option>993 · TLS</option></select></label><label>SMTP 服务器<input id="wizard-smtp" required placeholder="smtp.mail.example" value="${esc(d.smtp||'')}"></label><label>端口与加密<select aria-label="SMTP 端口与加密"><option>465 · TLS</option><option>587 · STARTTLS</option></select></label><label class="full">应用专用密码<input id="wizard-secret" type="password" autocomplete="off" placeholder="演示模式：填任意虚构值" required></label></div><div class="field-error" role="alert">${esc(state.wizardError)}</div><div class="setting-action-row"><button type="button" class="secondary" data-action="wizard-back">返回</button><button class="primary" type="submit">测试连接 ${icon('arrow')}</button></div></form>`;
  if(state.wizard===2)body=`<div class="step-label">02 / 检查连接</div><h1>${state.scenario==='auth'?'需要检查登录信息。':'连接信息已验证。'}</h1><p class="lead">${state.scenario==='auth'?'密码未通过验证。你的输入仍保留在这里，可以修改后重试。':'完成连接后，我们会开始同步邮件。同步期间也可以阅读已收到的内容。'}</p>${settingRow('接收邮件 · IMAP',esc(d.imap||'imap.mail.example'),`<span class="tag ${state.scenario==='auth'?'warn':''}">${state.scenario==='auth'?'认证失败':'连接成功'}</span>`)}${settingRow('发送邮件 · SMTP',esc(d.smtp||'smtp.mail.example'),`<span class="tag ${state.scenario==='auth'?'warn':''}">${state.scenario==='auth'?'尚未测试':'连接成功'}</span>`)}<div class="setting-action-row"><button class="secondary" data-action="wizard-back">修改信息</button>${state.scenario!=='auth'?'<button class="primary" data-action="wizard-next">连接并开始同步</button>':''}</div><p class="onboard-foot">此处为模拟测试，不会向服务器发送请求。</p>`;
  if(state.wizard===3)body=`<div class="step-label">03 / 准备就绪</div><h1>你的邮箱，已经在这里。</h1><p class="lead">正在同步最近的邮件。这个窗口可以先关掉，后台会继续完成同步。</p><div class="setting-row"><span class="avatar">Q</span><div class="setting-copy"><strong>${esc(d.name||'个人邮箱')}</strong><p>${esc(d.address||'qing@mail.example')}</p></div><span class="tag">同步中</span></div><div class="progress-line"><i></i></div><p class="quiet">已同步 126 封 · 演示进度</p><div class="setting-action-row"><button class="primary" data-action="wizard-finish">进入收件箱 ${icon('arrow')}</button></div><p class="onboard-foot">进入邮箱后，再按需开启系统通知。</p>`;
  return `<main id="workspace" tabindex="-1" class="onboard-page"><header class="onboard-top"><span class="traffic" aria-hidden="true"><i></i><i></i><i></i></span><span class="brand">Maild</span><span>设置你的邮箱</span></header><div class="onboard-body"><div class="steps" aria-label="第 ${state.wizard+1} 步，共 4 步">${[0,1,2,3].map(i=>`<i class="${i<=state.wizard?'done':''}"></i>`).join('')}</div>${body}</div></main>`;
}
function render(){
  document.body.classList.toggle('dark',state.theme==='dark'); document.body.classList.toggle('compact',state.compact);
  document.querySelector('#theme-button').innerHTML=icon(state.theme==='dark'?'sun':'moon');
  document.querySelector('#scenario').value=state.scenario;
  document.querySelectorAll('[data-route]').forEach(b=>b.classList.toggle('review-active',b.dataset.route===state.route));
  if(state.route==='onboard')app.innerHTML=onboarding();
  else app.innerHTML=`<div class="shell">${sidebar()}<main id="workspace" tabindex="-1">${state.route==='settings'?settingsPage():state.route==='compose'?composePage():`<div class="mail-workspace ${state.mobileReader?'show-reader':''}">${listPane()}${reader()}</div>`}</main></div>`;
  document.querySelectorAll('#compose-form button:not([type])').forEach(b=>b.type='button');
  if (state.route==='compose' && ['sending','unknown'].includes(state.sendState)) document.querySelectorAll('#compose-form input,#compose-form textarea,#compose-form select,#compose-form button').forEach(el=>el.disabled=true);
  document.querySelectorAll('#search-form button').forEach(b=>b.type='button');
}
function route(name){
  if(state.route==='compose')captureDraft();
  state.route=name;state.checked.clear();state.mobileReader=false;
  if(name==='compose') {state.draftOpen=true;}
  if(name==='search'){state.search=state.search||'发票'; const rows=currentMessages();state.selected=rows[0]?.id||null;}
  if(name==='onboard')state.wizard=0;
  render();
}
function selectMessage(id){state.selected=id;state.remote=false;state.reply=false;state.inlineText='';state.history=false;state.mobileReader=true;render();}
function modal(title,body,footer=''){dialogTrigger=document.activeElement;const d=document.querySelector('#dialog');d.innerHTML=`<div class="dialog-head"><h2>${title}</h2>${button('close','关闭对话框','close-dialog')}</div><div class="dialog-body">${body}</div>${footer?`<div class="dialog-footer">${footer}</div>`:''}`;if(!d.open)d.showModal();}
function closeModal(){document.querySelector('#dialog').close(); if(dialogTrigger?.isConnected)dialogTrigger.focus();}
function explain(title,text){modal(title,`<p>${text}</p>`,`<button class="primary" data-action="close-dialog">知道了</button>`);}
function activity(){
  const s=state.scenario;
  let body='';
  if(s==='unknown')body=`<div class="activity-item"><span class="tag warn">结果未知</span><strong>归档「秋季刊印刷确认」</strong><p>工作邮箱 · 收件箱 → 归档<br>连接中断前已发出请求，但没有收到结果。邮件可能已经移走。</p><div class="activity-actions"><button class="secondary" data-action="check-target">查看目标文件夹</button><button class="secondary" data-action="retry-unknown">仍要重试</button><button class="text-button" data-action="discard-op">不再重试</button></div></div>`;
  if(s==='auth')body=`<div class="activity-item"><span class="tag warn">需要登录</span><strong>工作邮箱 · qing@work.example</strong><p>登录凭据已失效，接收和发送暂时不可用。其他账户不受影响。</p><button class="secondary" data-manage-account="work">更新登录信息</button></div>`;
  if(s==='offline')body=`<div class="activity-item"><span class="tag warn">等待连接</span><strong>工作邮箱暂时离线</strong><p>本地缓存仍可读，标记和移动会在重连后提交。发送新邮件需要重新连接。</p><button class="secondary" data-action="reconnect">重新连接</button></div>`;
  if(s==='daemon'||s==='incompatible')body=`<div class="activity-item"><span class="tag warn">${s==='daemon'?'后台未连接':'版本不匹配'}</span><strong>暂时不能提交修改</strong><p>现有画面属于缓存。${s==='daemon'?'重新连接后台后继续。':'先更新到配套版本，再恢复发送和设置操作。'}</p><button class="secondary" data-action="reconnect">${s==='daemon'?'重试连接':'模拟更新完成'}</button></div>`;
  body+=state.queue.map(q=>`<div class="activity-item"><span class="tag">待同步</span><strong>${esc(q.title)}</strong><p>${q.type==='trash'?'正在移到废纸篓':'正在归档'} · ${accountNames[q.account]}<br>目标文件夹将在下次同步时显示邮件。</p></div>`).join('');
  if(!body)body='<div class="empty" style="min-height:180px">'+icon('check')+'<h2>所有操作已同步</h2><p>3 个邮箱收发正常。没有需要处理的问题。</p></div>';
  modal('同步活动',body,'<button class="secondary" data-action="close-dialog">关闭</button>');
}
function canMutate(){if(['daemon','incompatible'].includes(state.scenario)){activity();return false;}return true;}
function moveMessages(target,bulk=false){if(!canMutate())return; const ids=bulk?[...state.checked]:[state.selected];const rows=samples.filter(m=>ids.includes(m.id)); rows.forEach(m=>{state.queue.push({title:m.subject,account:m.account,type:target});m.folder=target;});state.checked.clear();state.selected=currentMessages()[0]?.id||null;state.mobileReader=false;render();toast(`演示：${rows.length} 封邮件${target==='trash'?'已移到废纸篓':'已归档'}，等待同步`);}
function captureDraft(){document.querySelectorAll('[data-draft]').forEach(el=>state.draft[el.dataset.draft]=el.value);}
function sendDraft(){
  captureDraft();if(state.sendState==='sending'||state.sendState==='unknown')return;
  const recipients=[state.draft.to,state.draft.cc,state.draft.bcc].filter(Boolean).flatMap(x=>x.split(/[,;，；]/)).map(x=>x.trim()).filter(Boolean);
  if(!state.draft.to.trim()){state.draftError='请填写至少一位收件人。';render();document.querySelector('#compose-to').focus();return;}
  if(recipients.some(x=>!/^\S+@[^\s@]+\.[^\s@]+$/.test(x))){state.draftError='有一项邮箱地址格式不完整，请检查收件人、抄送和密送。';render();return;}
  if(!state.draft.subject.trim()){modal('没有填写主题','<p>这封邮件没有主题，仍然发送吗？</p>','<button class="secondary" data-action="close-dialog">继续编辑</button><button class="primary" data-action="send-confirm">仍然发送</button>');return;}
  performSend();
}
function performSend(){closeModal();if(!canMutate())return;if(state.scenario==='offline'){toast('邮箱离线，请先保留草稿，重连后发送。');return;}state.sendState='sending';state.draftError='';render();setTimeout(()=>{
  if(state.scenario==='sendfail'){state.sendState='failed';state.draftError='服务器明确拒绝了这次投递。内容已保留，请检查发送账户后重试。';render();return;}
  if(state.scenario==='sendunknown'){state.sendState='unknown';render();return;}
  samples.unshift({id:Date.now(),sender:'我',address:'qing@work.example',subject:state.draft.subject||'(无主题)',snippet:state.draft.body.slice(0,80),time:'刚刚',account:'work',folder:'sent',unread:false,star:false,kind:'text'});
  state.draft={to:'',cc:'',bcc:'',subject:'',body:''};state.draftOpen=false;state.sendState='idle';state.route='mail';state.folder='sent';state.account='all';state.selected=samples[0].id;render();toast('演示：邮件已发送，已发送副本正在同步。');
  },650);}
document.addEventListener('click',e=>{
 const el=e.target.closest('button, a');if(!el)return;
 if(el.dataset.route){route(el.dataset.route);return;}
 if(el.dataset.folder){closeModal();state.route='mail';state.folder=el.dataset.folder;state.filter='all';state.checked.clear();state.mobileReader=false;if(state.folder==='drafts'&&state.draftOpen){route('compose');return;}state.selected=currentMessages()[0]?.id||null;render();return;}
 if(el.dataset.account){closeModal();state.account=el.dataset.account;state.route='mail';state.folder='inbox';state.selected=currentMessages()[0]?.id||null;state.mobileReader=false;render();return;}
 if(el.dataset.message){selectMessage(Number(el.dataset.message));return;}
 if(el.dataset.star){if(!canMutate())return;const m=samples.find(x=>x.id===Number(el.dataset.star));m.star=!m.star;render();return;}
 if(el.dataset.filter){state.filter=el.dataset.filter;state.selected=currentMessages()[0]?.id||null;render();return;}
 if(el.dataset.setting){state.setting=el.dataset.setting;render();return;}
 if(el.dataset.toggle){state.toggles[el.dataset.toggle]=!state.toggles[el.dataset.toggle];render();toast('演示偏好已更新，仅作用于当前原型。');return;}
 if(el.dataset.manageAccount){
   closeModal(); const key=el.dataset.manageAccount;state.managedAccount=key;
   const name=accountNames[key],address={personal:'qing@mail.example',work:'qing@work.example',dev:'qing@nodeseek.example'}[key];
   modal(`管理${name}`,`<p>以下操作只演示交互，不会更改真实邮箱。</p>${settingRow('邮箱地址',address,'')}${settingRow('接收 / 发送',key==='dev'?'NodeSeek Email 扩展':'IMAP + SMTP · TLS','<span class="tag">已配置</span>')}<div class="setting-action-row"><button class="secondary" data-action="update-login">更新登录信息</button><button class="secondary" data-action="pause-account">暂停同步</button><button class="secondary danger" data-action="remove-account">移除账户</button></div>`);return;
 }

 const action=el.dataset.action;
 switch(action){
 case 'settings':route('settings');break;
 case 'compose':route('compose');break;
 case 'theme':state.theme=state.theme==='light'?'dark':'light';render();break;
 case 'density':modal('列表显示',`<p>舒适密度保留摘要，紧凑密度增加可见邮件数。</p><button class="secondary" data-action="comfortable">舒适 · 90px</button> <button class="secondary" data-action="compact">紧凑 · 68px</button>`);break;
 case 'comfortable':case 'compact':state.compact=action==='compact';closeModal();render();break;
 case 'nav':modal('邮箱',`<nav class="nav-drawer">${navContent()}<button class="nav-item" data-action="compose">${icon('compose')}写邮件</button><button class="nav-item" data-action="settings">${icon('settings')}设置</button><button class="nav-item" data-action="activity">${icon('cloud')}同步活动</button></nav>`);break;
 case 'back-list':state.mobileReader=false;render();break;
 case 'back-mail':closeModal();state.route='mail';render();break;
 case 'remote':state.remote=!state.remote;render();toast(state.remote?'演示：只为本封邮件放行图片。':'演示：已恢复拦截远程图片。');break;
 case 'history':state.history=!state.history;render();break;
 case 'previous':case 'next':{const rows=currentMessages();const at=rows.findIndex(x=>x.id===state.selected);const m=rows[at+(action==='next'?1:-1)];if(m)selectMessage(m.id);break;}
 case 'archive':case 'trash':moveMessages(action);break;
 case 'bulk-archive':moveMessages('archive',true);break;
 case 'bulk-trash':moveMessages('trash',true);break;
 case 'clear-select':state.checked.clear();render();break;
 case 'read':closeModal();if(canMutate()){const m=samples.find(m=>m.id===state.selected);m.unread=!m.unread;render();toast(m.unread?'演示：已标为未读。':'演示：已标为已读。');}break;
 case 'move':if(canMutate())modal('移动当前邮件','<p>只显示当前账户的目标文件夹，不跨账户移动。</p><button class="nav-item" data-action="choose-archive">'+icon('archive')+'归档</button><button class="nav-item" data-action="choose-trash">'+icon('trash')+'废纸篓</button>');break;
 case 'choose-archive':case 'choose-trash':closeModal();moveMessages(action==='choose-archive'?'archive':'trash');break;
 case 'more':modal('邮件操作','<button class="nav-item" data-action="read">'+icon('mail')+'切换已读状态</button><button class="nav-item" data-action="source">'+icon('draft')+'查看邮件详情</button><button class="nav-item" data-action="shortcuts">'+icon('help')+'键盘快捷键</button>');break;
 case 'source':explain('邮件详情','显示完整发件人、收件人、日期、来源文件夹与会话标识。原始 EML 导出尚无对应接口，设计不把它作为已可用操作。');break;
 case 'reply':case 'reply-all':if(canMutate()){state.reply=action==='reply-all'?'all':'one';state.inlineText=state.replyDrafts[`${state.selected}:${state.reply}`]||'';render();const body=document.querySelector('#inline-body');body.scrollIntoView({block:'center'});body.focus();}break;
 case 'collapse-reply':state.reply=false;render();toast('回复草稿已暂存在本次演示中。');break;
 case 'discard-reply':modal('丢弃回复草稿？','<p>尚未发送的回复内容将清除。</p>','<button class="secondary" data-action="close-dialog">继续编辑</button><button class="primary" data-action="discard-reply-confirm">丢弃</button>');break;
 case 'discard-reply-confirm':closeModal();delete state.replyDrafts[`${state.selected}:${state.reply}`];state.reply=false;state.inlineText='';render();break;
 case 'send-reply':if(!state.inlineText.trim()){toast('请先填写回复内容。');return;}if(state.scenario==='offline'){toast('工作邮箱离线，请先保留回复草稿。');return;}delete state.replyDrafts[`${state.selected}:${state.reply}`];state.reply=false;state.inlineText='';render();toast('演示：回复已发送。');break;
 case 'forward':{const m=samples.find(m=>m.id===state.selected);state.draft={to:'',cc:'',bcc:'',subject:'Fwd: '+m.subject,body:`\n\n转发邮件\n发件人：${m.sender} <${m.address}>\n主题：${m.subject}\n\n${m.snippet}`,attachment:m.attach};route('compose');break;}
 case 'cc':captureDraft();state.cc=!state.cc;render();break;
 case 'save-draft':captureDraft();state.draftOpen=!!Object.values(state.draft).some(Boolean);state.route='mail';state.folder='inbox';render();toast('草稿暂存在本次演示中，可从侧栏继续编辑。');break;
 case 'discard-draft':modal('丢弃这封草稿？','<p>这封邮件还没有发送。丢弃后，当前编辑内容将清除。</p>','<button class="secondary" data-action="close-dialog">继续编辑</button><button class="primary" data-action="discard-confirm">丢弃草稿</button>');break;
 case 'discard-confirm':closeModal();state.draft={to:'',cc:'',bcc:'',subject:'',body:''};state.draftOpen=false;state.draftError='';state.route='mail';render();break;
 case 'send-confirm':performSend();break;
 case 'compose-options':explain('写信方式','本期采用纯文本编辑，收到的 HTML 邮件仍完整显示。富文本工具栏、定时发送和撤回邮件不在本期范围。');break;
 case 'attachment':explain('附件下载设计','目标体验：点击附件，选择保存位置并查看下载状态。当前协议只返回附件元数据，字节读取与安全保存通道尚待实现；原型不会下载文件。');break;
 case 'add-attachment':explain('添加附件 · 设计目标','目标体验：选择本地文件，显示文件名、体积与准备状态，未准备好时禁用发送。新附件上传尚不在现有内核能力内，需要独立补齐后开放。转发已有附件另行支持。');break;
 case 'clear-search':state.search='';state.searchAttachment=false;state.searchAccount='all';state.searchPeriod='all';state.filter='all';state.scenario='normal';state.selected=currentMessages()[0]?.id||null;render();break;
 case 'search-attachment':state.searchAttachment=!state.searchAttachment;state.selected=currentMessages()[0]?.id||null;render();break;
 case 'activity':activity();break;
 case 'reconnect':case 'retry-html':closeModal();state.scenario='normal';render();toast('演示：已恢复连接。');break;
 case 'check-target':closeModal();state.route='mail';state.folder='archive';state.account='work';state.selected=currentMessages()[0]?.id||null;render();break;
 case 'retry-unknown':modal('确认重新归档？','<p>上一次操作可能已完成。再次执行可能在目标文件夹产生副本。请先检查目标文件夹。</p>','<button class="secondary" data-action="close-dialog">先不重试</button><button class="primary" data-action="resolve-retry">我已检查，重新尝试</button>');break;
 case 'resolve-retry':closeModal();state.scenario='normal';render();toast('演示：操作重新进入同步队列。');break;
 case 'discard-op':closeModal();state.scenario='normal';render();toast('演示：不再重试，下一次同步会核对实际位置。');break;
 case 'close-dialog':closeModal();break;
 case 'add-account':closeModal();route('onboard');break;
 case 'wizard-next':state.wizard++;render();break;
 case 'wizard-back':state.wizard=Math.max(0,state.wizard-1);render();break;
 case 'wizard-finish':state.scenario='normal';state.route='mail';state.folder='inbox';state.account='all';state.selected=1;render();toast('演示：账户已连接，正在同步。');break;
 case 'wizard-provider':modal('选择邮箱扩展','<p>显示已安装且支持账户绑定的扩展。扩展需要的秘密只在绑定表单输入，不在账户列表回显。</p><button class="nav-item" data-action="provider-connect">'+icon('puzzle')+'NodeSeek Email '+icon('chevron')+'</button>');break;
 case 'provider-connect':modal('连接 NodeSeek Email','<p>这里只接受演示值，不会创建或访问任何真实邮箱。</p><label>邮箱地址<input class="modal-field" placeholder="qing@nodeseek.example"></label><label>API Key<input class="modal-field" type="password" placeholder="演示值"></label>','<button class="secondary" data-action="close-dialog">返回</button><button class="primary" data-action="provider-test">测试已有邮箱</button>');break;
 case 'provider-test':explain('连接成功 · 演示','正式绑定必须先只读探测已有邮箱。创建新的远程邮箱是独立操作，要单独说明影响，不能由测试步骤顺带执行。');break;
 case 'update-login':closeModal();state.wizardData={name:accountNames[state.managedAccount||'work'],address:{personal:'qing@mail.example',work:'qing@work.example',dev:'qing@nodeseek.example'}[state.managedAccount||'work']};state.wizard=1;state.route='onboard';render();break;
 case 'pause-account':closeModal();toast('演示：已暂停该账户同步，本地邮件保留。');break;
 case 'remove-account':modal(`移除${accountNames[state.managedAccount||'work']}？`,'<p>停止此账户收发，并移除其受管配置与凭据。服务器邮件不会因该动作被删除。</p><p>正式实现必须按已接受的账户删除契约说明缓存如何处理，有未提交操作时先处理队列。</p>','<button class="secondary" data-action="close-dialog">保留账户</button><button class="secondary danger" data-action="remove-demo">仅演示移除</button>');break;
 case 'remove-demo':closeModal();toast('演示操作结束，账户样本保留用于继续评审。');break;
 case 'trusted':explain('可信发件人','目前名单为空。目标是允许逐个添加和移除明确地址；不能把一次加载扩展成整个域名永久信任。名单持久化与资源放行接口尚待内核实现。');break;
 case 'edit-rule':modal('工作邮箱 · 静默时段','<p>修改仅对本次运行生效。</p><div class="form-grid"><label>开始<input type="time" value="22:00"></label><label>结束<input type="time" value="08:00"></label><label class="full">时区<select><option>UTC+08:00（固定偏移）</option></select></label></div>','<button class="secondary" data-action="close-dialog">取消</button><button class="primary" data-action="apply-runtime">应用到本次运行</button>');break;
 case 'apply-runtime':closeModal();toast('演示：规则已应用到本次运行，重启后恢复配置。');break;
 case 'manage-address':explain('管理地址','可修改显示名和备注，或停用这个地址。停用前需要说明其收发影响；主地址不能在这里被误删。');break;
 case 'new-address':modal('添加工作邮箱地址','<p>这里只演示地址表单，实际可用性由邮箱提供方决定。</p><label>地址<input class="modal-field" placeholder="hello@work.example"></label><label>用途备注<input class="modal-field" placeholder="例如：刊物联系"></label>','<button class="secondary" data-action="close-dialog">取消</button><button class="primary" data-action="address-demo">添加演示地址</button>');break;
 case 'address-demo':closeModal();toast('演示：地址表单已提交，未创建真实地址。');break;
 case 'plugin-detail':explain('NodeSeek Email','邮箱服务扩展 · 已启用\n开发订阅账户正在使用此扩展。禁用它会暂停该账户的收发；在实际实现中应展示准确的权限和版本。');break;
 case 'install-plugin':explain('从本地文件安装','目标流程：选择包 → 校验 → 展示来源、版本和权限 → 用户确认安装 → 明确成功或回滚结果。本次原型不打开文件选择器、不安装任何包。');break;
 case 'backup':explain('数据与恢复','备份、恢复和存储管理需遵循现有维护锁及发布流程。此页不提供未经验证的“一键清理”或恢复按钮；正式图形流程需单独验收。');break;
 case 'stop-background':modal('停止后台收信？','<p>所有账户会暂停同步，Maild 不再发送新邮件通知。已保存的邮件仍保留。</p>','<button class="secondary" data-action="close-dialog">继续运行</button><button class="secondary danger" data-action="stop-demo">停止后台 · 演示</button>');break;
 case 'stop-demo':closeModal();state.scenario='daemon';render();toast('演示：后台已停止。');break;
 case 'custom-folder':state.route='mail';state.folder='projects';state.selected=null;render();break;
 case 'shortcuts':modal('键盘快捷键','<div class="hotkeys"><span>搜索全部邮件</span><kbd>⌘ K</kbd><span>写邮件</span><kbd>⌘ N</kbd><span>发送当前草稿</span><kbd>⌘ Enter</kbd><span>设置</span><kbd>⌘ ,</kbd><span>上一封 / 下一封</span><kbd>↑ / ↓</kbd><span>关闭浮层 / 返回列表</span><kbd>Esc</kbd></div><p style="margin-top:20px">网页环境中部分系统快捷键可能被浏览器保留。Tauri 真机验收时验证焦点与系统菜单。</p>');break;
 }
 if(document.querySelector('#dialog').open&&['settings','compose'].includes(action))closeModal();
});
document.addEventListener('input',e=>{if(e.target.dataset.draft){state.draft[e.target.dataset.draft]=e.target.value;state.draftOpen=true;}if(e.target.id==='inline-body'){state.inlineText=e.target.value;state.replyDrafts[`${state.selected}:${state.reply}`]=e.target.value;}});
document.addEventListener('change',e=>{
 const el=e.target;
 if(el.dataset.select){const id=Number(el.dataset.select);el.checked?state.checked.add(id):state.checked.delete(id);render();}
 if(el.id==='scenario'){state.scenario=el.value;state.sendState='idle';state.draftError='';if(['noresults','indexing'].includes(el.value)){state.route='search';state.search=el.value==='noresults'?'不存在的关键词':'发票';}else if(['sendfail','sendunknown'].includes(el.value)){state.route='compose';state.draftOpen=true;state.draft={to:'yuan@shanhai.example',cc:'',bcc:'',subject:'Re: 秋季刊印刷确认',body:'予安，确认单已核对，没有调整。谢谢。'};}else{state.route='mail';state.folder='inbox';state.account='all';}state.selected=currentMessages()[0]?.id||null;render();}
 if(el.id==='theme-select'){state.theme=el.value;render();}
 if(el.id==='density-select'){state.compact=el.value==='compact';render();}
 if(el.id==='search-account'){state.searchAccount=el.value;state.selected=currentMessages()[0]?.id||null;render();}
 if(el.id==='search-period'){state.searchPeriod=el.value;state.selected=currentMessages()[0]?.id||null;render();}
});
document.addEventListener('submit',e=>{
 e.preventDefault();
 if(e.target.id==='search-form'){state.search=document.querySelector('#search-input').value;state.route='search';state.scenario=state.scenario==='noresults'?'normal':state.scenario;state.selected=currentMessages()[0]?.id||null;state.mobileReader=false;render();}
 if(e.target.id==='compose-form')sendDraft();
 if(e.target.id==='wizard-form'){state.wizardData={address:document.querySelector('#wizard-address').value,name:document.querySelector('#wizard-name').value,imap:document.querySelector('#wizard-imap').value,smtp:document.querySelector('#wizard-smtp').value};state.wizard=2;render();}
});
document.addEventListener('keydown',e=>{
 const typing=e.target.matches('input,textarea,select,[contenteditable=true]');
 if(document.querySelector('#dialog').open)return;
 if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==='k'){e.preventDefault();if(!['mail','search'].includes(state.route))route('search');document.querySelector('#search-input')?.focus();return;}
 if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==='n'){e.preventDefault();route('compose');return;}
 if((e.metaKey||e.ctrlKey)&&e.key===','){e.preventDefault();route('settings');return;}
 if((e.metaKey||e.ctrlKey)&&e.key==='Enter'){e.preventDefault();if(state.route==='compose')sendDraft();else if(state.reply)document.querySelector('[data-action="send-reply"]')?.click();return;}
 if(e.key==='Escape'){if(state.route==='compose'){captureDraft();state.route='mail';render();}else if(state.mobileReader){state.mobileReader=false;render();}return;}
 if(!typing&&['ArrowUp','ArrowDown'].includes(e.key)&&['mail','search'].includes(state.route)){e.preventDefault();const rows=currentMessages(),at=rows.findIndex(x=>x.id===state.selected);const m=rows[at+(e.key==='ArrowDown'?1:-1)];if(m)selectMessage(m.id);}
});
document.querySelector('#dialog').addEventListener('close',()=>{if(dialogTrigger?.isConnected)dialogTrigger.focus();});
render();
