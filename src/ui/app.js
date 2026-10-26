'use strict';
/* kron web UI: chats, agent runs, files on the machines, settings.
   No build step: plain DOM, marked + DOMPurify + highlight.js served by the hub. */

// ── Utilities ────────────────────────────────────────────────────────────────
const $ = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const icon = (name, cls = '') => `<svg class="ic ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
function el(tag, cls, html) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (html != null) e.innerHTML = html;
  return e;
}
function btn(cls, html, onClick, title) {
  const b = el('button', cls, html);
  b.type = 'button';
  if (title) { b.title = title; b.setAttribute('aria-label', title); }
  if (onClick) b.onclick = onClick;
  return b;
}
function store(k, v) {
  try {
    if (v === undefined) return localStorage.getItem('kron-' + k);
    if (v === null) localStorage.removeItem('kron-' + k); else localStorage.setItem('kron-' + k, v);
  } catch { return null; }
}
const storeJSON = (k, v) => {
  if (v !== undefined) return store(k, JSON.stringify(v));
  try { return JSON.parse(store(k) || 'null'); } catch { return null; }
};
const debounce = (f, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => f(...a), ms); }; };
const plural = (n, one, few, many) => {
  const m10 = n % 10, m100 = n % 100;
  return m10 === 1 && m100 !== 11 ? one : m10 >= 2 && m10 <= 4 && (m100 < 10 || m100 >= 20) ? few : many;
};
const fmtSize = n => n < 1024 ? n + ' Б' : n < 1048576 ? (n / 1024).toFixed(n < 10240 ? 1 : 0) + ' КБ' : n < 1e9 ? (n / 1048576).toFixed(1) + ' МБ' : (n / 1e9).toFixed(1) + ' ГБ';
const fmtDur = ms => { const s = Math.round(ms / 1000); return s < 60 ? s + ' с' : Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); };
const fmtTime = ts => new Date(ts).toLocaleString('ru', {day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit'});
const isMobile = () => matchMedia('(max-width: 800px)').matches;
const coarse = () => matchMedia('(pointer: coarse)').matches;
const android = window.KronAndroid || null;
const b64ToBytes = b => Uint8Array.from(atob(b), c => c.charCodeAt(0));
const bytesToB64 = async blob => {
  const buf = new Uint8Array(await blob.arrayBuffer());
  let s = '';
  for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
  return btoa(s);
};
function shortPath(p) {
  if (!p) return '';
  const home = S.info && S.nodeInfo()?.home;
  if (home && p.startsWith(home)) p = '~' + p.slice(home.length);
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 3 ? '…/' + parts.slice(-2).join('/') : p;
}

// ── State ────────────────────────────────────────────────────────────────────
const params = new URLSearchParams(location.search);
const S = {
  token: params.get('token') || store('token') || '',
  info: null,
  thread: null, title: '', threads: [], query: '',
  run: null, busy: false, runStart: 0, follower: null,
  pick: storeJSON('pick'),                 // {kind: provider | agent | site, name}
  mode: store('mode') || 'agent',          // agent | chat
  node: store('node'),
  cwds: storeJSON('cwds') || {},           // node -> working folder
  access: store('access') || 'full',       // full | ask | read
  effort: store('effort') || '',
  models: storeJSON('models') || {},       // provider -> model override
  attachments: [],
  ask: null,                               // pending ask in the open thread
  enterSends: store('enter') !== 'off',
  notify: store('notify') === 'on',
  nodeInfo() { return this.info?.nodes.find(n => n.name === this.node) || null; },
  cwd() { return this.cwds[this.node] || ''; },
};
if (params.has('token')) { store('token', S.token); history.replaceState(null, '', '/'); }
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});

// ── API ──────────────────────────────────────────────────────────────────────
let offline = false;
async function api(path, opts = {}) {
  opts.headers = Object.assign({'content-type': 'application/json'}, opts.headers, S.token ? {authorization: 'Bearer ' + S.token} : {});
  if (opts.json !== undefined) { opts.body = JSON.stringify(opts.json); opts.method = opts.method || 'POST'; }
  let r;
  try { r = await fetch(path, opts); } catch (e) {
    setOffline(true);
    throw new Error('нет связи с хабом');
  }
  setOffline(false);
  if (r.status === 401) { showLogin(); throw new Error('нужен токен'); }
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  return r;
}
const getJSON = path => api(path).then(r => r.json());
const postJSON = (path, json) => api(path, {json}).then(r => r.status === 204 ? null : r.json());
function setOffline(v) {
  if (v === offline) return;
  offline = v;
  const b = $('banner');
  b.hidden = !v;
  b.innerHTML = icon('cloud', 'sm') + ' Нет связи с хабом — переподключаюсь…';
}
function showLogin() { $('login').hidden = false; $('tokenInput').focus(); }
$('loginForm').onsubmit = e => {
  e.preventDefault();
  S.token = $('tokenInput').value.trim(); store('token', S.token);
  $('login').hidden = true; boot();
};

// ── Toasts, dialogs, menus ───────────────────────────────────────────────────
function toast(text, {action, onAction, err, timeout = 4500, ic} = {}) {
  const t = el('div', 'toast' + (err ? ' err' : ''));
  t.innerHTML = (ic ? icon(ic, 'sm') : err ? icon('circle-alert', 'sm') : '') + `<span>${esc(text)}</span>`;
  let done = false;
  const close = () => { if (!done) { done = true; t.remove(); } };
  if (action) t.append(btn('', esc(action), () => { close(); onAction(); }));
  $('toasts').append(t);
  setTimeout(close, timeout);
  return close;
}
const fail = e => toast(String(e?.message || e), {err: true, timeout: 7000});

/** A modal: resolves to true / the input text, or null when dismissed. */
function dialog({title, text, input, value = '', placeholder = '', ok = 'Готово', danger = false, cancel = 'Отмена'}) {
  return new Promise(resolve => {
    const scrim = el('div', 'scrim'), d = el('div', 'dialog');
    d.setAttribute('role', 'dialog'); d.setAttribute('aria-modal', 'true');
    d.innerHTML = `<h2>${esc(title)}</h2>` + (text ? `<p>${esc(text)}</p>` : '');
    let inp = null;
    if (input) { inp = el('input', 'input'); inp.value = value; inp.placeholder = placeholder; d.append(inp); }
    const row = el('div', 'row-end');
    const finish = v => { scrim.remove(); document.removeEventListener('keydown', key, true); resolve(v); };
    if (cancel) row.append(btn('btn', esc(cancel), () => finish(null)));
    row.append(btn('btn ' + (danger ? 'danger solid' : 'primary'), esc(ok), () => finish(inp ? inp.value : true)));
    d.append(row); scrim.append(d); document.body.append(scrim);
    const key = e => {
      if (e.key === 'Escape') { e.stopPropagation(); finish(null); }
      if (e.key === 'Enter' && !e.isComposing && (inp ? document.activeElement === inp : true)) { e.preventDefault(); finish(inp ? inp.value : true); }
    };
    document.addEventListener('keydown', key, true);
    scrim.onclick = e => { if (e.target === scrim) finish(null); };
    setTimeout(() => (inp || row.lastChild).focus(), 30);
  });
}

let openMenu = null;
function closeMenu() {
  if (!openMenu) return;
  openMenu.el.remove(); openMenu.anchor?.setAttribute('aria-expanded', 'false');
  document.removeEventListener('keydown', openMenu.key, true);
  openMenu = null;
}
/**
 * Popover menu next to `anchor`. Items: {icon, label, sub, meta, on, danger, onClick, keep},
 * {sec}, {sep}, {el}. `search` adds a filter box.
 */
function menu(anchor, build, {search, place = 'below', width} = {}) {
  if (openMenu && openMenu.anchor === anchor) { closeMenu(); return; }
  closeMenu();
  const m = el('div', 'menu'); m.setAttribute('role', 'menu');
  if (width) m.style.minWidth = width + 'px';
  let q = '';
  const list = el('div');
  const render = () => {
    list.innerHTML = '';
    let shown = 0, pendingSec = null;
    for (const it of build()) {
      if (it.sec) { pendingSec = it.sec; continue; }
      if (it.sep) { if (!q) list.append(el('div', 'sep')); continue; }
      if (it.el) { if (!q) list.append(it.el); continue; }
      const hay = (it.label + ' ' + (it.sub || '')).toLowerCase();
      if (q && !hay.includes(q)) continue;
      if (pendingSec) { list.append(el('div', 'sec', esc(pendingSec))); pendingSec = null; }
      const b = el('button', 'mi' + (it.on ? ' on' : '') + (it.danger ? ' danger' : ''));
      b.setAttribute('role', 'menuitem');
      b.innerHTML = (it.icon ? icon(it.icon) : '') + `<span class="txt"><b>${esc(it.label)}</b>${it.sub ? `<small>${esc(it.sub)}</small>` : ''}</span>` +
        (it.meta ? `<span class="meta">${it.meta}</span>` : '') + (it.on !== undefined ? icon('check', 'sm chk') : '');
      b.onclick = e => { e.stopPropagation(); if (!it.keep) closeMenu(); it.onClick?.(); };
      list.append(b); shown++;
    }
    if (!shown && q) list.append(el('div', 'empty', 'Ничего не найдено'));
  };
  if (search) {
    const s = el('div', 'search'), inp = el('input');
    inp.placeholder = search; inp.setAttribute('aria-label', search);
    inp.oninput = () => { q = inp.value.trim().toLowerCase(); render(); };
    s.append(inp); m.append(s);
    setTimeout(() => { if (!coarse()) inp.focus(); }, 20);
  }
  m.append(list); render();
  document.body.append(m);
  const r = anchor.getBoundingClientRect(), mw = m.offsetWidth, mh = m.offsetHeight;
  let left = Math.min(Math.max(8, r.left), innerWidth - mw - 8);
  let top = place === 'above' || (r.bottom + mh + 8 > innerHeight && r.top - mh - 8 > 0) ? r.top - mh - 6 : r.bottom + 6;
  m.style.left = left + 'px'; m.style.top = Math.max(8, top) + 'px';
  anchor.setAttribute('aria-expanded', 'true');
  const key = e => {
    if (e.key === 'Escape') { e.stopPropagation(); closeMenu(); anchor.focus(); return; }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const items = [...m.querySelectorAll('.mi')];
      if (!items.length) return;
      let i = items.indexOf(document.activeElement);
      i = e.key === 'ArrowDown' ? (i + 1) % items.length : (i <= 0 ? items.length - 1 : i - 1);
      items[i].focus();
    }
  };
  document.addEventListener('keydown', key, true);
  openMenu = {el: m, anchor, key, render};
}
document.addEventListener('pointerdown', e => {
  if (openMenu && !openMenu.el.contains(e.target) && !openMenu.anchor.contains(e.target)) closeMenu();
});
addEventListener('resize', () => closeMenu());

function copy(text, b) {
  const done = () => {
    if (!b) return toast('Скопировано', {ic: 'check', timeout: 1500});
    const old = b.innerHTML; b.innerHTML = icon('check', 'xs') + (b.dataset.label ? '<span>Скопировано</span>' : '');
    setTimeout(() => b.innerHTML = old, 1400);
  };
  if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text).then(done, () => fallbackCopy(text, done));
  fallbackCopy(text, done);
}
function fallbackCopy(text, done) {
  const ta = document.createElement('textarea'); ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
  document.body.append(ta); ta.select(); document.execCommand('copy'); ta.remove(); done();
}
async function download(name, blob) {
  if (android?.saveFile) {
    const err = android.saveFile(name, await bytesToB64(blob));
    return err ? fail(err) : toast('Сохранено в «Загрузки»: ' + name, {ic: 'download'});
  }
  const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = name;
  document.body.append(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(a.href), 4000);
}

// ── Theme & preferences ─────────────────────────────────────────────────────
function applyTheme() {
  const t = store('theme') || 'system', root = document.documentElement;
  if (t === 'system') delete root.dataset.theme; else root.dataset.theme = t;
  const z = store('textsize'); if (z) root.dataset.text = z; else delete root.dataset.text;
  const dark = t === 'dark' || (t === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
  try { android?.setBars?.(dark ? '#1f1e1d' : '#faf9f5', dark); } catch {}
}
matchMedia('(prefers-color-scheme: dark)').addEventListener?.('change', applyTheme);
applyTheme();

// ── Markdown ─────────────────────────────────────────────────────────────────
const mk = new marked.Marked({gfm: true, breaks: true});
mk.use({renderer: {
  code({text, lang}) {
    const l = (lang || '').trim().split(/\s+/)[0].toLowerCase();
    return `<div class="code"><div class="code-head"><span class="lang">${esc(l || 'текст')}</span>` +
      `<button data-act="wrap" title="Перенос строк" aria-label="Перенос строк">${icon('chevron-down', 'xs')}</button>` +
      `<button data-act="copy" data-label="1">${icon('copy', 'xs')}<span>Копировать</span></button></div>` +
      `<pre><code data-lang="${esc(l)}">${esc(text)}</code></pre></div>`;
  },
}});
DOMPurify.addHook('afterSanitizeAttributes', n => {
  if (n.tagName === 'A' && n.getAttribute('href')) { n.setAttribute('target', '_blank'); n.setAttribute('rel', 'noopener noreferrer'); }
});
function md(src) {
  let html = mk.parse(src || '');
  html = html.replace(/<table>/g, '<div class="tbl"><table>').replace(/<\/table>/g, '</table></div>');
  return DOMPurify.sanitize(html, {ADD_ATTR: ['target', 'data-act', 'data-label', 'data-lang']});
}
function highlight(root) {
  for (const c of root.querySelectorAll('pre code[data-lang]:not(.hljs)')) {
    const l = c.dataset.lang;
    try {
      if (l && hljs.getLanguage(l)) hljs.highlightElement(Object.assign(c, {className: 'language-' + l}));
      else if (!l && c.textContent.length < 4000) { const r = hljs.highlightAuto(c.textContent); if (r.relevance > 6) { c.innerHTML = r.value; c.classList.add('hljs'); } }
    } catch {}
  }
}
document.addEventListener('click', e => {
  const b = e.target.closest('.code [data-act]');
  if (!b) return;
  const box = b.closest('.code');
  if (b.dataset.act === 'copy') copy(box.querySelector('code').textContent, b);
  if (b.dataset.act === 'wrap') box.classList.toggle('wrap');
});

// ── Sidebar: threads ─────────────────────────────────────────────────────────
const sideOpen = () => isMobile() ? document.body.classList.contains('drawer') : !document.body.classList.contains('side-closed');
function toggleSide(open = !sideOpen()) {
  if (isMobile()) document.body.classList.toggle('drawer', open);
  else { document.body.classList.toggle('side-closed', !open); store('side', open ? null : 'closed'); }
}
if (document.documentElement.classList.contains('side-closed-init')) document.body.classList.add('side-closed');
$('sideClose').onclick = () => toggleSide(false);
$('sideOpen').onclick = () => toggleSide(true);
$('sideScrim').onclick = () => toggleSide(false);

const hidden = new Set(); // threads deleted but still undoable
async function loadThreads() {
  const q = S.query ? '?q=' + encodeURIComponent(S.query) : '';
  S.threads = await getJSON('/api/threads' + q);
  renderThreads();
}
function dayGroup(ts) {
  const d = new Date(ts), now = new Date();
  const day = 864e5, start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (ts >= start) return 'Сегодня';
  if (ts >= start - day) return 'Вчера';
  if (ts >= start - 7 * day) return 'Последние 7 дней';
  if (ts >= start - 30 * day) return 'Последние 30 дней';
  return d.toLocaleString('ru', {month: 'long', year: 'numeric'}).replace(/^./, c => c.toUpperCase());
}
function renderThreads() {
  const box = $('threads'); box.innerHTML = '';
  const list = S.threads.filter(t => !hidden.has(t.name));
  if (!list.length) {
    box.append(el('div', 'side-empty', S.query ? 'Ничего не найдено' : 'Здесь появятся ваши чаты'));
  }
  let group = null;
  for (const t of list) {
    const g = S.query ? 'Найдено' : dayGroup(t.updated);
    if (g !== group) { group = g; box.append(el('div', 'grp', esc(g))); }
    const d = el('div', 'th' + (t.name === S.thread ? ' on' : ''));
    d.tabIndex = 0; d.setAttribute('role', 'link');
    const state = t.waiting ? '<span class="dot waiting" title="Ждёт вашего решения"></span>' : t.running ? '<span class="dot busy" title="Агент работает"></span>' : '';
    d.innerHTML = state + (t.forked_from ? icon('git-branch', 'xs') : '') +
      `<span class="t">${esc(t.title || 'Без названия')}${t.snippet ? `<span class="snip">${esc(t.snippet)}</span>` : ''}</span>`;
    d.title = t.title;
    const more = btn('icon-btn sm more', icon('ellipsis', 'sm'), e => { e.stopPropagation(); threadMenu(more, t); }, 'Действия');
    more.setAttribute('aria-haspopup', 'menu');
    d.append(more);
    d.onclick = () => openThread(t.name);
    d.onkeydown = e => { if (e.key === 'Enter') openThread(t.name); };
    d.oncontextmenu = e => { e.preventDefault(); threadMenu(more, t); };
    box.append(d);
  }
  const running = S.threads.filter(t => t.running).length;
  scheduleThreadPoll(running ? 5000 : 30000);
}
let threadPoll;
function scheduleThreadPoll(ms) {
  clearTimeout(threadPoll);
  threadPoll = setTimeout(() => { if (!document.hidden) loadThreads().catch(() => {}); else scheduleThreadPoll(ms); }, ms);
}
function threadMenu(anchor, t) {
  menu(anchor, () => [
    {icon: 'pencil', label: 'Переименовать', onClick: () => renameThread(t)},
    {icon: 'git-branch', label: 'Копия чата', sub: 'Продолжить разговор отдельно', onClick: () => branch(t.name)},
    {icon: 'download', label: 'Скачать как Markdown', onClick: () => exportThread(t.name, t.title)},
    {sep: true},
    {icon: 'trash-2', label: 'Удалить', danger: true, onClick: () => deleteThread(t)},
  ]);
}
async function renameThread(t) {
  const v = await dialog({title: 'Название чата', input: true, value: t.title, ok: 'Сохранить'});
  if (v == null) return;
  await postJSON(`/api/threads/${t.name}/title`, {title: v}).catch(fail);
  if (S.thread === t.name) setTitle(v);
  loadThreads();
}
async function branch(name, upto) {
  try {
    const r = await postJSON(`/api/threads/${name}/fork`, upto == null ? {} : {upto});
    await openThread(r.thread);
    toast('Создана копия чата', {ic: 'git-branch'});
    return r.thread;
  } catch (e) { fail(e); }
}
function deleteThread(t) {
  // Undo instead of "are you sure?": the delete happens when the toast is gone.
  hidden.add(t.name); renderThreads();
  if (S.thread === t.name) openThread(null);
  let undone = false;
  toast('Чат удалён', {action: 'Отменить', timeout: 5000, onAction: () => { undone = true; hidden.delete(t.name); renderThreads(); }});
  setTimeout(async () => {
    if (undone) return;
    await api('/api/threads/' + t.name, {method: 'DELETE'}).catch(fail);
    hidden.delete(t.name); loadThreads();
  }, 5200);
}
async function exportThread(name, title) {
  const recs = await getJSON('/api/threads/' + name);
  let out = `# ${title || name}\n\n`;
  for (const r of recs) {
    if (r.role === 'user') out += `## Вы\n\n${r.content}\n\n`;
    if (r.role === 'assistant' && r.text) out += `## kron\n\n${r.text}\n\n`;
    if (r.role === 'assistant') for (const tc of r.tool_calls || []) out += `> ${toolName(tc.name)}: ${toolArg(tc.name, parseArgs(tc.arguments)).split('\n')[0]}\n\n`;
  }
  download((title || name).replace(/[\\/:*?"<>|]+/g, ' ').slice(0, 60).trim() + '.md', new Blob([out], {type: 'text/markdown'}));
}
const search = $('search');
search.oninput = debounce(() => {
  S.query = search.value.trim(); $('searchClear').hidden = !S.query; loadThreads().catch(fail);
}, 220);
$('searchClear').onclick = () => { search.value = ''; search.oninput(); search.focus(); };
$('newChat').onclick = () => { openThread(null); $('text').focus(); };

// ── Tools: names, icons, argument summaries ──────────────────────────────────
const TOOLS = {
  shell: ['terminal', 'Команда'], exec_command: ['terminal', 'Процесс'], write_stdin: ['keyboard', 'Ввод в процесс'],
  apply_patch: ['file-diff', 'Правка файлов'], write_file: ['file-code', 'Запись файла'], read_file: ['file-text', 'Чтение файла'],
  list_dir: ['folder-open', 'Папка'], grep_files: ['text-search', 'Поиск в файлах'], web_search: ['globe', 'Поиск в интернете'],
  web_fetch: ['link', 'Страница'], update_plan: ['list-checks', 'План'], spawn_agent: ['bot', 'Субагент'],
  ask_user: ['message-circle-question', 'Вопрос'],
};
const toolIcon = n => (TOOLS[n] || ['wrench'])[0];
const toolName = n => (TOOLS[n] || [null, n])[1];
function parseArgs(a) { if (typeof a !== 'string') return a || {}; try { return JSON.parse(a); } catch { return {input: a}; } }
function patchFiles(input) {
  const files = []; let cur = null;
  for (const line of String(input || '').split('\n')) {
    let m;
    if ((m = line.match(/^\*\*\* (Add|Update|Delete) File: (.+)$/))) { cur = {op: m[1], path: m[2].trim(), lines: [], add: 0, del: 0}; files.push(cur); continue; }
    if ((m = line.match(/^\*\*\* Move to: (.+)$/)) && cur) { cur.move = m[1].trim(); continue; }
    if (/^\*\*\* (Begin Patch|End Patch|End of File)/.test(line) || !cur) continue;
    if (line.startsWith('@@')) cur.lines.push(['h', line]);
    else if (line.startsWith('+')) { cur.lines.push(['a', line.slice(1)]); cur.add++; }
    else if (line.startsWith('-')) { cur.lines.push(['d', line.slice(1)]); cur.del++; }
    else cur.lines.push(['c', line.slice(1)]);
  }
  return files;
}
function toolArg(name, a) {
  switch (name) {
    case 'shell': return Array.isArray(a.command) ? a.command.join(' ') : String(a.command || '');
    case 'exec_command': return a.cmd || '';
    case 'write_stdin': return a.chars ? JSON.stringify(a.chars).slice(1, -1) : 'чтение вывода';
    case 'apply_patch': return patchFiles(a.input || a.patch).map(f => f.path.split('/').pop()).join(', ');
    case 'write_file': return a.path || '';
    case 'read_file': return a.file_path || '';
    case 'list_dir': return a.dir_path || '';
    case 'grep_files': return (a.pattern || '') + (a.path ? '  в ' + a.path : '');
    case 'web_search': return a.query || '';
    case 'web_fetch': return (a.url || '').replace(/^https?:\/\//, '');
    case 'spawn_agent': return (a.agent ? a.agent + ': ' : '') + (a.task || '');
    case 'ask_user': return a.question || '';
    default: return Object.values(a).filter(v => typeof v === 'string').join(' ');
  }
}
/** ok | err | denied, and a short badge text. */
function toolStatus(name, output) {
  if (output == null) return ['run', ''];
  if (/^The user declined/.test(output)) return ['denied', 'отклонено'];
  if (/^error:/.test(output)) return ['err', 'ошибка'];
  const m = output.match(/^Exit code: (-?\d+)/) || output.match(/exited with code (-?\d+)/);
  if (m && m[1] !== '0') return ['err', 'код ' + m[1]];
  if (/^Command timed out/.test(output)) return ['err', 'тайм-аут'];
  return ['ok', ''];
}
function diffView(f) {
  const d = el('div', 'diff');
  const op = {Add: 'новый', Delete: 'удалён', Update: ''}[f.op];
  d.innerHTML = `<div class="fh">${icon(f.op === 'Delete' ? 'trash-2' : 'file-code', 'xs')}<b>${esc(f.path)}${f.move ? ' → ' + esc(f.move) : ''}</b>` +
    (op ? `<span class="badge">${op}</span>` : '') + (f.add ? `<span class="plus">+${f.add}</span>` : '') + (f.del ? `<span class="minus">−${f.del}</span>` : '') + '</div>';
  if (f.lines.length) {
    const ls = el('div', 'lines');
    ls.innerHTML = f.lines.slice(0, 600).map(([k, t]) => `<div class="ln ${k}">${k === 'a' ? '+' : k === 'd' ? '−' : k === 'h' ? '' : ' '}${esc(t)}</div>`).join('') +
      (f.lines.length > 600 ? `<div class="ln h">… ещё ${f.lines.length - 600} строк</div>` : '');
    d.append(ls);
  }
  return d;
}
function stripShell(out) { return String(out).replace(/^Exit code: -?\d+\nOutput:\n?/, ''); }
function pre(text, cls = 'out') { const p = el('pre', cls); p.textContent = text; return p; }

// ── Rendering a thread ───────────────────────────────────────────────────────
const log = $('log'), scroller = $('scroll');
let stick = true;
scroller.addEventListener('scroll', () => {
  stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 120;
  showToBottom(!stick);
}, {passive: true});
const toBottom = force => { if (force || stick) scroller.scrollTop = scroller.scrollHeight; };
let toBottomBtn = null;
function showToBottom(v) {
  if (v && !toBottomBtn) {
    toBottomBtn = btn('', icon('arrow-down'), () => { stick = true; scroller.scrollTo({top: scroller.scrollHeight, behavior: 'smooth'}); }, 'Вниз');
    toBottomBtn.id = 'toBottom'; $('composerWrap').append(toBottomBtn);
  } else if (!v && toBottomBtn) { toBottomBtn.remove(); toBottomBtn = null; }
}
function setHasLog() { document.body.classList.toggle('has-log', log.children.length > 0); }

/** Lines "📎 path" at the end of a user message are its attachments. */
function splitAttachments(text) {
  const lines = text.split('\n'), files = [];
  while (lines.length && /^📎 /.test(lines[lines.length - 1])) files.unshift(lines.pop().slice(2).trim());
  return [lines.join('\n').trimEnd(), files];
}
let userCount = 0;
function addUser(text, n = userCount++) {
  const d = el('div', 'msg-user'); d.dataset.n = n;
  const [body, files] = splitAttachments(text);
  if (files.length) {
    const fc = el('div', 'files');
    for (const f of files) {
      const c = el('button', 'file-chip', icon(/\.(png|jpe?g|gif|webp|svg)$/i.test(f) ? 'image' : 'file', 'sm') + `<span class="n">${esc(f.split(/[\\/]/).pop())}</span>`);
      c.title = f; c.onclick = () => openFiles(f);
      fc.append(c);
    }
    d.append(fc);
  }
  const b = el('div', 'b'); b.textContent = body || '(файлы)';
  const acts = el('div', 'msg-actions');
  acts.append(
    btn('icon-btn sm', icon('copy', 'sm'), e => copy(text, e.currentTarget), 'Копировать'),
    btn('icon-btn sm', icon('pencil', 'sm'), () => editUser(d, text), 'Изменить и отправить заново'),
  );
  d.append(b, acts);
  log.append(d); setHasLog();
  return d;
}
function editUser(d, text) {
  if (S.busy) return toast('Дождитесь конца ответа или остановите его');
  const [body, files] = splitAttachments(text);
  const box = el('div', 'edit'), ta = el('textarea');
  ta.value = body;
  const row = el('div', 'row-end');
  const restore = () => { box.remove(); [...d.children].forEach(c => c.hidden = false); };
  row.append(
    btn('btn sm', 'Отмена', restore),
    btn('btn sm primary', 'Отправить', () => {
      const t = ta.value.trim(); if (!t) return;
      resend(+d.dataset.n, t + (files.length ? '\n\n' + files.map(f => '📎 ' + f).join('\n') : ''));
    }),
  );
  box.append(ta, row);
  [...d.children].forEach(c => c.hidden = true);
  d.append(box); ta.focus(); ta.setSelectionRange(ta.value.length, ta.value.length);
  ta.onkeydown = e => { if (e.key === 'Escape') restore(); if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) row.lastChild.click(); };
}
/** The n-th user message and everything after it go to a new branch, where `text` is sent instead. */
async function resend(n, text) {
  if (!S.thread) return;
  try {
    const recs = await getJSON('/api/threads/' + S.thread);
    let k = -1, upto = recs.length;
    for (let i = 0; i < recs.length; i++) if (recs[i].role === 'user' && ++k === n) { upto = i; break; }
    const r = await postJSON(`/api/threads/${S.thread}/fork`, {upto});
    await openThread(r.thread);
    toast('Новая ветка чата', {ic: 'git-branch'});
    send(text);
  } catch (e) { fail(e); }
}

/** One assistant turn: thinking, text, steps, asks — in the order they happen. */
class Turn {
  constructor({live = false, n = userCount - 1} = {}) {
    this.root = el('div', 'turn'); this.root.dataset.n = n;
    log.append(this.root); setHasLog();
    this.live = live; this.textEl = null; this.raw = ''; this.texts = [];
    this.think = null; this.group = null; this.groups = []; this.steps = new Map(); this.plan = null; this.count = 0;
    this.started = Date.now();
    if (live) this.tick = setInterval(() => this.updateHead(), 1000);
  }
  addText(t) {
    if (!t) return;
    if (!this.textEl) {
      this.closeThink(); this.group = null;
      this.textEl = el('div', 'md' + (this.live ? ' caret' : '')); this.root.append(this.textEl); this.raw = '';
    }
    this.raw += t; this.dirty();
  }
  dirty() {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => { this.frame = null; if (this.textEl) this.textEl.innerHTML = md(this.raw); toBottom(); });
  }
  closeText() {
    if (!this.textEl) return;
    cancelAnimationFrame(this.frame); this.frame = null;
    this.textEl.innerHTML = md(this.raw); this.textEl.classList.remove('caret');
    highlight(this.textEl); this.texts.push(this.raw);
    this.textEl = null;
  }
  addReasoning(t) {
    if (!t) return;
    if (!this.think) {
      this.closeText();
      const d = el('details', 'think');
      d.innerHTML = `<summary>${icon('chevron-right', 'xs chev')}${icon('brain', 'sm')}<span class="${this.live ? 'shimmer' : ''}">${this.live ? 'Размышляет…' : 'Размышления'}</span></summary><div class="body"></div>`;
      this.root.append(d); this.think = {el: d, body: d.querySelector('.body'), raw: ''};
    }
    this.think.raw += t; this.think.body.textContent = this.think.raw.trim();
    if (this.think.el.open) this.think.body.scrollTop = this.think.body.scrollHeight;
    toBottom();
  }
  closeThink() {
    if (!this.think) return;
    const s = this.think.el.querySelector('summary span:last-child');
    s.className = ''; s.textContent = 'Размышления';
    this.think = null;
  }
  ensureGroup() {
    if (this.group) return this.group;
    this.closeText(); this.closeThink();
    const g = el('div', 'steps open');
    const head = btn('head', ''), list = el('div', 'list');
    head.onclick = () => g.classList.toggle('open');
    g.append(head, list); this.root.append(g);
    this.group = {el: g, head, list, items: [], errors: 0};
    this.groups.push(this.group);
    return this.group;
  }
  tool(id, name, args, parent, output) {
    const a = parseArgs(args);
    if (name === 'update_plan') { this.renderPlan(a); return; }
    if (name === 'ask_user' && !parent) {
      // Live, the question arrives as an "ask" event; in history it is the call and its result.
      if (output != null) this.ask({id: id || 'q' + this.count++, kind: 'question', question: a.question || ''}, String(output).replace(/^The user answered: /, ''));
      return;
    }
    let container, group;
    const p = parent && this.steps.get(parent);
    if (p) {
      if (!p.sub) { p.sub = el('div', 'sub'); p.el.insertBefore(p.sub, p.detail); }
      container = p.sub; group = p.group;
    } else { group = this.ensureGroup(); container = group.list; group.items.push(id); }
    const s = el('div', 'step');
    const row = btn('row', '');
    const detail = el('div', 'detail');
    row.onclick = () => s.classList.toggle('open');
    s.append(row, detail); container.append(s);
    const st = {el: s, row, detail, name, args: a, sub: null, output: null, group, top: !p};
    this.steps.set(id || name + '#' + this.count, st);
    if (!parent) this.count++;
    this.fillStep(st, output);
    this.updateHead(); toBottom();
  }
  fillStep(st, output) {
    const {name, args: a} = st;
    const [state, badge] = toolStatus(name, output);
    st.el.classList.toggle('err', state === 'err');
    const arg = toolArg(name, a).split('\n')[0];
    st.row.innerHTML = (state === 'run' ? icon('loader-circle', 'sm spin') : icon(toolIcon(name), 'sm')) +
      `<span class="name">${esc(toolName(name))}</span><span class="arg">${esc(arg.slice(0, 200))}</span>` +
      (badge ? `<span class="badge ${state === 'denied' ? 'warn' : 'err'}">${badge}</span>` : '');
    st.detail.innerHTML = '';
    const add = x => st.detail.append(x);
    switch (name) {
      case 'shell': case 'exec_command': {
        const cmd = toolArg(name, a);
        if (/\*\*\* Begin Patch/.test(cmd)) patchFiles(cmd).forEach(f => add(diffView(f)));
        else add(pre(cmd, 'cmd'));
        if (output != null) add(pre(stripShell(output) || '(нет вывода)'));
        break;
      }
      case 'apply_patch': patchFiles(a.input || a.patch).forEach(f => add(diffView(f))); if (output != null) add(pre(output)); break;
      case 'write_file': add(diffView({op: 'Add', path: a.path || '', lines: String(a.content || '').split('\n').map(l => ['a', l]), add: String(a.content || '').split('\n').length, del: 0})); if (output != null) add(pre(output)); break;
      case 'web_search': if (output != null) { const m = el('div', 'md'); m.innerHTML = md(output); add(m); } break;
      case 'spawn_agent': {
        add(pre(a.task || '', 'cmd'));
        if (output != null) { const m = el('div', 'md'); m.innerHTML = md(output); add(m); }
        break;
      }
      case 'ask_user': if (output != null) add(pre(output)); break;
      default: if (output != null) add(pre(output)); else if (Object.keys(a).length) add(pre(JSON.stringify(a, null, 2)));
    }
    if (output != null) {
      if (state === 'err' && st.output == null && st.top) st.group.errors++;
      st.output = output;
    }
  }
  toolDone(id, name, output) {
    if (name === 'update_plan' || name === 'ask_user') return;
    let st = id && this.steps.get(id);
    if (!st) st = [...this.steps.values()].find(s => s.name === name && s.output == null);
    if (!st) return;
    this.fillStep(st, output);
    this.updateHead();
  }
  /** Group headers: the step in progress while live, then a count of steps and errors. */
  updateHead(all = false) {
    const last = this.groups[this.groups.length - 1];
    for (const g of all ? this.groups : [last].filter(Boolean)) {
      const running = [...this.steps.values()].filter(s => s.group === g && s.output == null);
      const live = this.live && g === last;
      const n = g.items.length;
      let html;
      if (live && running.length) {
        const cur = running[running.length - 1];
        html = icon('loader-circle', 'sm spin') + `<span class="lbl">${esc(toolName(cur.name))} · ${esc(toolArg(cur.name, cur.args).split('\n')[0].slice(0, 120))}</span>`;
      } else {
        html = icon('list-checks', 'sm') + `<span class="lbl">${n} ${plural(n, 'действие', 'действия', 'действий')}${g.errors ? ` · ${g.errors} ${plural(g.errors, 'ошибка', 'ошибки', 'ошибок')}` : ''}</span>`;
      }
      g.head.innerHTML = html + (live ? `<span class="badge">${fmtDur(Date.now() - this.started)}</span>` : '') + icon('chevron-right', 'sm chev');
    }
  }
  renderPlan(a) {
    if (!Array.isArray(a.plan)) return;
    if (!this.plan) { this.closeText(); this.closeThink(); this.group = null; this.plan = el('div', 'plan'); this.root.append(this.plan); }
    const done = a.plan.filter(p => p.status === 'completed').length, total = a.plan.length;
    this.plan.innerHTML = `<div class="ph">${icon('list-checks', 'sm')}План<span class="prog">${done} из ${total}</span></div>` +
      `<div class="bar"><i style="width:${total ? (100 * done / total).toFixed(0) : 0}%"></i></div>` +
      (a.explanation ? `<div class="expl">${esc(a.explanation)}</div>` : '') +
      '<ul>' + a.plan.map(p => `<li class="${esc(p.status)}">${icon(p.status === 'completed' ? 'circle-check' : p.status === 'in_progress' ? 'loader-circle' : 'square', 'xs' + (p.status === 'in_progress' && this.live ? ' spin' : ''))}<span>${esc(p.step)}</span></li>`).join('') + '</ul>';
    toBottom();
  }
  ask(q, answered) {
    this.closeText(); this.closeThink();
    const card = el('div', 'ask' + (q.kind === 'question' ? ' q' : ''));
    card.dataset.ask = q.id;
    this.root.append(card);
    if (answered != null) { card.innerHTML = `<div class="ah"></div>`; return this.answered(q.id, answered, q); }
    if (q.kind === 'question') {
      card.innerHTML = `<div class="ah">${icon('message-circle-question')}Вопрос от агента</div><div class="qtext"></div>`;
      card.querySelector('.qtext').textContent = q.question;
      const b = el('div', 'btns');
      for (const o of q.options || []) b.append(btn('btn', esc(o), () => answer(q, o)));
      card.append(b, el('div', 'hint-text', 'Или напишите ответ в поле ниже.'));
      S.ask = q; $('text').placeholder = 'Ваш ответ агенту…';
      if (!coarse()) $('text').focus();
    } else {
      const a = parseArgs(q.args);
      card.innerHTML = `<div class="ah">${icon('shield-alert')}Агент хочет: ${esc(toolName(q.name).toLowerCase())}</div>`;
      const body = el('div');
      if (q.name === 'apply_patch' || /\*\*\* Begin Patch/.test(toolArg(q.name, a))) patchFiles(a.input || a.patch || toolArg(q.name, a)).forEach(f => body.append(diffView(f)));
      else if (q.name === 'write_file') body.append(diffView({op: 'Add', path: a.path || '', lines: String(a.content || '').split('\n').slice(0, 200).map(l => ['a', l]), add: 0, del: 0}));
      else body.append(pre(toolArg(q.name, a) || JSON.stringify(a, null, 2), 'cmd'));
      const b = el('div', 'btns');
      b.append(
        btn('btn primary', icon('check', 'sm') + 'Разрешить', () => answer(q, 'allow')),
        btn('btn', icon('shield-check', 'sm') + 'Разрешать всё до конца ответа', () => answer(q, 'allow_all')),
        btn('btn danger', icon('x', 'sm') + 'Отклонить', () => answer(q, 'deny')),
      );
      card.append(body, b, el('div', 'hint-text', 'Можно отклонить и написать в поле ниже, что сделать вместо этого.'));
      S.ask = q; $('text').placeholder = 'Почему нет или что сделать вместо этого…';
    }
    setBusy(true); notifyHidden('Нужно ваше решение', q.question || toolArg(q.name, parseArgs(q.args)));
    toBottom(true);
  }
  answered(id, answer, q) {
    const card = this.root.querySelector(`[data-ask="${CSS.escape(id)}"]`);
    if (S.ask?.id === id) { S.ask = null; $('text').placeholder = 'Напишите сообщение…'; setBusy(S.busy); }
    if (!card) return;
    q = q || {kind: card.classList.contains('q') ? 'question' : 'approval'};
    card.className = 'ask done';
    let txt;
    if (q.kind === 'question') txt = icon('message-circle-question', 'sm') + `<span>Ответ: ${esc(answer)}</span>`;
    else if (answer === 'allow') txt = icon('circle-check', 'sm') + 'Разрешено';
    else if (answer === 'allow_all') txt = icon('shield-check', 'sm') + 'Разрешено всё до конца ответа';
    else txt = icon('circle-x', 'sm') + 'Отклонено' + (answer.startsWith('deny:') ? ': ' + esc(answer.slice(5)) : '');
    card.innerHTML = `<div class="ah">${txt}</div>`;
  }
  error(msg) {
    this.closeText(); this.closeThink();
    if (msg === 'stopped') { this.root.append(el('div', 'note', icon('square', 'xs') + 'Остановлено')); return; }
    const box = el('div', 'err-box', icon('circle-alert', 'sm') + `<span class="t">${esc(msg)}</span>`);
    if (/логин|login|sign in|401|403|unauthor|expired|войти/i.test(msg)) box.append(btn('btn sm', 'Войти', () => openSettings('accounts')));
    else if (!this.live || this.retry) box.append(btn('btn sm', icon('refresh-cw', 'xs') + 'Повторить', () => this.retry ? this.retry() : retryTurn(+this.root.dataset.n)));
    this.root.append(box);
  }
  finish() {
    clearInterval(this.tick);
    this.closeText(); this.closeThink();
    for (const st of this.steps.values()) if (st.output == null) { st.output = ''; st.row.querySelector('.ic').outerHTML = icon('circle-x', 'sm'); }
    this.live = false;
    for (const g of this.groups) if (g.items.length > 4) g.el.classList.remove('open');
    this.updateHead(true);
    this.root.querySelectorAll('.plan .spin').forEach(i => i.classList.remove('spin'));
    const all = this.texts.join('\n\n');
    const acts = el('div', 'msg-actions');
    if (all) acts.append(btn('icon-btn sm', icon('copy', 'sm'), e => copy(all, e.currentTarget), 'Копировать ответ'));
    acts.append(
      btn('icon-btn sm', icon('refresh-cw', 'sm'), () => retryTurn(+this.root.dataset.n), 'Ответить заново (в новой ветке)'),
      btn('icon-btn sm', icon('git-branch', 'sm'), () => branchAfter(+this.root.dataset.n), 'Продолжить отсюда в новой ветке'),
    );
    if (this.label) acts.append(el('span', 'meta', esc(this.label)));
    this.root.append(acts);
  }
}
async function retryTurn(n) {
  if (S.busy) return;
  const u = log.querySelector(`.msg-user[data-n="${n}"]`);
  const recs = await getJSON('/api/threads/' + S.thread).catch(() => []);
  const users = recs.filter(r => r.role === 'user');
  if (!u || !users[n]) return;
  resend(n, users[n].content);
}
async function branchAfter(n) {
  const recs = await getJSON('/api/threads/' + S.thread).catch(() => []);
  let k = -1, upto = recs.length;
  for (let i = 0; i < recs.length; i++) if (recs[i].role === 'user' && ++k === n + 1) { upto = i; break; }
  branch(S.thread, upto);
}

function render(records, upto) {
  log.innerHTML = ''; userCount = 0;
  // Results follow their calls; ids can repeat between model turns (Gemini), so match in order.
  const outputs = new Map();
  let pending = [];
  for (const r of records) {
    if (r.role === 'assistant') pending = [...(r.tool_calls || [])];
    if (r.role === 'tool') { const i = pending.findIndex(c => c.id === r.call_id); if (i >= 0) outputs.set(pending.splice(i, 1)[0], r.content); }
  }
  let turn = null;
  for (const r of records.slice(0, upto ?? records.length)) {
    if (r.role === 'user') { turn?.finish(); turn = null; addUser(r.content); }
    if (r.role === 'assistant') {
      turn = turn || new Turn();
      if (r.reasoning) { turn.addReasoning(r.reasoning); turn.closeThink(); }
      if (r.text) { turn.addText(r.text); turn.closeText(); }
      for (const tc of r.tool_calls || []) turn.tool(tc.id, tc.name, tc.arguments, null, outputs.get(tc) ?? '');
    }
  }
  turn?.finish();
  setHasLog(); stick = true; toBottom(true);
}

// ── Opening threads ──────────────────────────────────────────────────────────
function setTitle(t) {
  S.title = t || '';
  $('threadTitle').textContent = S.title;
  document.title = (S.busy ? (S.ask ? '⏸ ' : '● ') : '') + (S.title ? S.title + ' · kron' : 'kron');
  $('threadMenuBtn').hidden = !S.thread;
}
async function openThread(name, {keepDraft} = {}) {
  S.follower?.abort(); S.follower = null;
  saveDraft();
  S.thread = name; S.ask = null; S.run = null;
  if (isMobile()) toggleSide(false);
  log.innerHTML = ''; userCount = 0; setHasLog(); setBusy(false);
  $('text').placeholder = 'Напишите сообщение…';
  const t = S.threads.find(x => x.name === name);
  setTitle(name ? t?.title || '' : '');
  if (name) {
    const [records, active, meta] = await Promise.all([
      getJSON('/api/threads/' + name),
      getJSON('/api/threads/' + name + '/run').catch(() => ({run: null})),
      getJSON('/api/threads/' + name + '/meta').catch(() => ({})),
    ]).catch(e => { fail(e); return [[], {run: null}, {}]; });
    if (S.thread !== name) return;
    if (meta.title) setTitle(meta.title);
    else if (!t) { const u = records.find(r => r.role === 'user'); setTitle(u ? u.content.slice(0, 80) : ''); }
    restoreSettings(meta.settings);
    if (active.run) { render(records, active.base); follow(active.run); }
    else render(records);
  } else {
    restoreSettings(null);
  }
  if (!keepDraft) loadDraft();
  renderThreads(); refreshControls();
  if (!coarse()) $('text').focus({preventScroll: true});
}
function restoreSettings(s) {
  if (!s) return;
  const pick = s.site ? {kind: 'site', name: s.site} : s.agent ? {kind: 'agent', name: s.agent} : s.provider ? {kind: 'provider', name: s.provider} : null;
  if (pick && validPick(pick)) S.pick = pick;
  if (s.mode === 'chat' || s.mode === 'agent') S.mode = s.mode;
  if (s.node && S.info?.nodes.some(n => n.name === s.node)) S.node = s.node;
  if (s.node && s.cwd != null) S.cwds[s.node] = s.cwd || '';
  if (s.access) S.access = s.access;
  if (s.provider && s.model) S.models[s.provider] = s.model;
  S.effort = s.effort || '';
}

// ── Model, mode, machine, access pickers ─────────────────────────────────────
const SITE_NAME = {chatgpt: 'ChatGPT', gemini: 'Gemini', claude: 'Claude'};
const KIND = {codex: 'Подписка ChatGPT', claude: 'Подписка Claude', gemini: 'Подписка Gemini', antigravity: 'Antigravity', local: 'На этом устройстве', openai: 'API · OpenAI-совместимый', anthropic: 'API · Anthropic', responses: 'API · Responses'};
const provider = name => S.info?.providers.find(p => p.name === name);
function validPick(p) {
  if (!p || !S.info) return false;
  if (p.kind === 'site') return S.info.sites.includes(p.name);
  if (p.kind === 'agent') return S.info.agents.some(a => a.name === p.name);
  return S.info.providers.some(x => x.name === p.name);
}
function pickModel(p) {
  if (p?.kind !== 'provider') return '';
  return S.models[p.name] || provider(p.name)?.model || '';
}
function effortCapable() {
  const p = S.pick?.kind === 'provider' ? provider(S.pick.name) : S.pick?.kind === 'agent' ? provider(S.info.agents.find(a => a.name === S.pick.name)?.provider || S.info.default) : null;
  return !!p && (p.kind === 'codex' || p.kind === 'responses');
}
function modelMenu() {
  const items = [];
  const sel = (kind, name) => S.pick?.kind === kind && S.pick?.name === name;
  const choose = pick => { S.pick = pick; storeJSON('pick', pick); refreshControls(); };
  const groups = [['Подписки', p => p.oauth], ['На этом устройстве', p => p.kind === 'local'], ['API', p => !p.oauth && p.kind !== 'local']];
  for (const [title, f] of groups) {
    const ps = S.info.providers.filter(f);
    if (!ps.length) continue;
    items.push({sec: title});
    for (const p of ps) items.push({
      icon: p.kind === 'local' ? 'cpu' : p.oauth ? 'sparkles' : 'key-round',
      label: p.name, sub: (S.models[p.name] || p.model) + ' · ' + (KIND[p.kind] || p.kind),
      meta: p.oauth && !p.logged_in ? '<span class="badge warn">нужен вход</span>' : '',
      on: sel('provider', p.name),
      onClick: () => { choose({kind: 'provider', name: p.name}); if (p.oauth && !p.logged_in) toast('Сначала войдите в подписку', {action: 'Войти', onAction: () => openSettings('accounts')}); },
    });
  }
  if (S.info.agents.length) {
    items.push({sec: 'Агенты'});
    for (const a of S.info.agents) items.push({icon: 'bot', label: a.name, sub: a.about || [a.provider, a.model].filter(Boolean).join(' · ') || 'пресет из config.toml', on: sel('agent', a.name), onClick: () => choose({kind: 'agent', name: a.name})});
  }
  if (S.info.sites.length) {
    items.push({sec: 'Веб-чаты · лимиты чата, через браузер'});
    for (const s of S.info.sites) items.push({icon: 'globe', label: SITE_NAME[s] || s, sub: 'как обычный чат на сайте', on: sel('site', s), onClick: () => choose({kind: 'site', name: s})});
  }
  if (S.pick?.kind === 'provider') {
    const p = provider(S.pick.name);
    const row = el('div', 'model-row'), inp = el('input');
    inp.value = S.models[p.name] || ''; inp.placeholder = p.model; inp.setAttribute('aria-label', 'Другая модель');
    inp.title = 'Другая модель этого провайдера (пусто — из config.toml)';
    const apply = () => { const v = inp.value.trim(); if (v && v !== p.model) S.models[p.name] = v; else delete S.models[p.name]; storeJSON('models', S.models); refreshControls(); };
    inp.onkeydown = e => { if (e.key === 'Enter') { apply(); closeMenu(); } };
    inp.onchange = apply;
    row.append(el('span', 'meta', 'Модель'), inp);
    items.push({sep: true}, {el: row});
  }
  items.push({sep: true}, {icon: 'settings', label: 'Подписки, ключи и модели…', onClick: () => openSettings('accounts')});
  return items;
}
$('modelBtn').onclick = () => menu($('modelBtn'), modelMenu, {search: 'Найти модель', width: 320});

function nodesFor() {
  const web = S.pick?.kind === 'site';
  return (S.info?.nodes || []).filter(n => !web || n.browser);
}
const nodeIcon = n => !n ? 'monitor' : n.os === 'android' ? 'smartphone' : n.hub && n.os === 'linux' ? 'server' : n.os === 'windows' || n.os === 'macos' ? 'laptop' : 'monitor';
const ACCESS = {
  full: ['shield', 'Полный доступ', 'Команды и правки без вопросов', 'Полный'],
  ask: ['shield-check', 'Спрашивать', 'Подтверждать команды и изменения файлов', 'Спрашивать'],
  read: ['eye', 'Только чтение', 'Смотреть файлы и искать, ничего не менять', 'Чтение'],
};
/** Chip text with a shorter variant for phones. */
const label2 = (long, short) => short && short !== long ? `<span class="long">${long}</span><span class="short">${short}</span>` : long;
const EFFORT = {'': 'авто', minimal: 'минимальное', low: 'низкое', medium: 'среднее', high: 'высокое'};
function refreshControls() {
  if (!S.info) return;
  if (!validPick(S.pick)) S.pick = S.info.default ? {kind: 'provider', name: S.info.default} : null;
  const p = S.pick, web = p?.kind === 'site', agentPreset = p?.kind === 'agent';
  const agentMode = !web && (agentPreset || S.mode === 'agent');
  const nodes = nodesFor();
  if (!nodes.some(n => n.name === S.node)) S.node = nodes[0]?.name || null;

  // Header: the model.
  const mb = $('modelBtn');
  if (!p) { mb.querySelector('.name').textContent = 'Нет моделей'; mb.querySelector('.sub').textContent = ''; }
  else if (p.kind === 'site') { mb.querySelector('.name').textContent = SITE_NAME[p.name] || p.name; mb.querySelector('.sub').textContent = 'веб-чат'; }
  else if (p.kind === 'agent') { mb.querySelector('.name').textContent = p.name; mb.querySelector('.sub').textContent = 'агент'; }
  else { mb.querySelector('.name').textContent = p.name; mb.querySelector('.sub').textContent = pickModel(p); }

  // Composer chips.
  const chips = $('chips'); chips.innerHTML = '';
  if (!web) {
    const c = btn('chip' + (agentMode ? ' on' : ''), icon(agentMode ? 'wrench' : 'message-square', 'sm') + `<span class="t">${agentMode ? 'Агент' : 'Чат'}</span>`,
      () => { S.mode = S.mode === 'agent' ? 'chat' : 'agent'; store('mode', S.mode); refreshControls(); },
      agentPreset ? 'Пресет агента всегда работает с инструментами' : 'Агент с инструментами или обычный чат');
    c.disabled = agentPreset; c.setAttribute('aria-pressed', agentMode);
    chips.append(c);
  }
  if (web || agentMode) {
    const n = S.nodeInfo(), cwd = S.cwd();
    const c = btn('chip', icon(web ? 'globe' : nodeIcon(n), 'sm') + `<span class="t">${esc(S.node || 'нет машин')}${!web && cwd ? '<span class="long"> · ' + esc(shortPath(cwd)) + '</span>' : ''}</span>` + icon('chevron-down', 'xs'),
      () => menu(c, nodeMenu, {place: 'above'}), web ? 'Где открыть браузер' : 'Где работает агент');
    chips.append(c);
  }
  if (agentMode) {
    const [ic, label, , short] = ACCESS[S.access] || ACCESS.full;
    const c = btn('chip' + (S.access === 'ask' ? ' okc' : S.access === 'read' ? ' on' : ''), icon(ic, 'sm') + `<span class="t">${label2(label, short)}</span>`,
      () => menu(c, accessMenu, {place: 'above'}), 'Что агенту можно без вопросов');
    chips.append(c);
  }
  if (!web && effortCapable()) {
    const c = btn('chip' + (S.effort ? ' on' : ''), icon('zap', 'sm') + `<span class="t">${label2('Усилие: ' + (EFFORT[S.effort] || S.effort), EFFORT[S.effort] || S.effort)}</span>`,
      () => menu(c, () => Object.entries(EFFORT).map(([k, v]) => ({label: v[0].toUpperCase() + v.slice(1), sub: k ? '' : 'как в config.toml', on: S.effort === k, onClick: () => { S.effort = k; store('effort', k); refreshControls(); }})), {place: 'above'}),
      'Сколько модель думает перед ответом');
    chips.append(c);
  }

  // Empty state.
  const ctx = $('emptyCtx');
  const bits = [];
  if (web) bits.push(icon('globe', 'sm') + 'Вопрос уйдёт в обычный веб-чат ' + esc(SITE_NAME[p.name] || p.name));
  else if (agentMode) bits.push(icon('wrench', 'sm') + 'Агент на машине <b>' + esc(S.node || '—') + '</b>' + (S.cwd() ? ' в ' + esc(shortPath(S.cwd())) : ''));
  else bits.push(icon('message-square', 'sm') + 'Обычный чат, без инструментов');
  ctx.innerHTML = bits.join(' ');
  renderSuggestions(web ? 'chat' : agentMode ? 'agent' : 'chat');
  $('filesBtn').hidden = !S.node;
  grow();
}
function nodeMenu() {
  const web = S.pick?.kind === 'site';
  const items = [{sec: web ? 'Браузер на машине' : 'Машина'}];
  for (const n of nodesFor()) items.push({
    icon: nodeIcon(n), label: n.name + (n.hub ? ' · хаб' : ''), sub: [n.os, n.cwd].filter(Boolean).join(' · '), on: n.name === S.node,
    onClick: () => { S.node = n.name; store('node', n.name); refreshControls(); },
  });
  if (!web) {
    items.push({sec: 'Рабочая папка'});
    items.push({icon: 'folder-open', label: S.cwd() ? shortPath(S.cwd()) : 'Папка машины по умолчанию', sub: S.cwd() || S.nodeInfo()?.cwd || '', onClick: () => openFiles(S.cwd() || '', {pick: true})});
    items.push({icon: 'folder', label: 'Выбрать другую папку…', onClick: () => openFiles(S.cwd() || '', {pick: true})});
    if (S.cwd()) items.push({icon: 'x', label: 'Сбросить папку', onClick: () => setCwd('')});
  }
  items.push({sep: true}, {icon: 'server', label: 'Подключить машину…', onClick: () => openSettings('machines')});
  return items;
}
function accessMenu() {
  return Object.entries(ACCESS).map(([k, [ic, label, sub]]) => ({icon: ic, label, sub, on: S.access === k, onClick: () => { S.access = k; store('access', k); refreshControls(); }}));
}
function setCwd(path) {
  if (!S.node) return;
  if (path) S.cwds[S.node] = path; else delete S.cwds[S.node];
  storeJSON('cwds', S.cwds); refreshControls();
}

const SUGG = {
  agent: [
    ['folder-open', 'Разберись в проекте', 'Изучи проект в рабочей папке и расскажи, как он устроен: структура, как собрать, запустить и проверить.'],
    ['wrench', 'Найди и почини ошибки', 'Запусти тесты и проверки проекта, найди причину падений и исправь её.'],
    ['globe', 'Сделай сайт', 'Сделай аккуратный одностраничный сайт в папке site/ (index.html и style.css, без сборки) и расскажи, как его открыть.'],
    ['cpu', 'Что с машиной', 'Покажи, чем занята машина: свободное место на диске, память и самые тяжёлые процессы.'],
  ],
  chat: [
    ['sparkles', 'Объясни простыми словами', 'Объясни простыми словами, как работает '],
    ['pencil', 'Помоги с текстом', 'Помоги написать письмо: '],
    ['list-checks', 'Составь план', 'Составь пошаговый план: '],
    ['message-square', 'Переведи', 'Переведи на английский, сохрани стиль:\n\n'],
  ],
};
function renderSuggestions(kind) {
  const box = $('sugg'); box.innerHTML = '';
  for (const [ic, title, prompt] of SUGG[kind]) {
    box.append(btn('', icon(ic) + `<span>${esc(title)}</span>`, () => {
      const t = $('text'); t.value = prompt; grow(); t.focus();
      if (kind === 'agent') send(); else t.setSelectionRange(t.value.length, t.value.length);
    }));
  }
}

// ── Composer ─────────────────────────────────────────────────────────────────
const text = $('text');
function grow() {
  text.style.height = 'auto';
  text.style.height = Math.min(text.scrollHeight, innerHeight * 0.4) + 'px';
  const has = text.value.trim() || S.attachments.length;
  const sendBtn = $('send');
  const stop = S.busy && !S.ask;
  sendBtn.disabled = !stop && !has;
  sendBtn.classList.toggle('stop', stop);
  sendBtn.innerHTML = icon(stop ? 'square' : 'arrow-up');
  sendBtn.setAttribute('aria-label', stop ? 'Остановить' : 'Отправить');
  sendBtn.title = stop ? 'Остановить (Esc)' : 'Отправить';
}
text.addEventListener('input', () => { grow(); saveDraftSoon(); });
text.addEventListener('keydown', e => {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    const viaEnter = S.enterSends && !coarse();
    if (viaEnter || e.ctrlKey || e.metaKey) { e.preventDefault(); if (!S.busy || S.ask) submit(); }
  }
  if (e.key === 'Escape' && S.busy && !S.ask) { e.preventDefault(); stop(); }
  if (e.key === 'ArrowUp' && !text.value && !S.busy) {
    const last = [...log.querySelectorAll('.msg-user')].pop();
    if (last) { e.preventDefault(); last.querySelector('[title^="Изменить"]').click(); }
  }
});
$('send').onclick = () => submit();
function submit() {
  const t = text.value.trim();
  if (S.ask && (t || S.ask.kind === 'question')) {
    if (!t) return;
    text.value = ''; grow(); saveDraft();
    return answer(S.ask, S.ask.kind === 'question' ? t : 'deny: ' + t);
  }
  if (S.busy) return stop();
  send();
}
function setBusy(b) {
  S.busy = b;
  setTitle(S.title);
  grow();
}
const drafts = storeJSON('drafts') || {};
function saveDraft() {
  const k = S.thread || '_new', v = text.value;
  if (v.trim()) drafts[k] = v; else delete drafts[k];
  storeJSON('drafts', drafts);
}
const saveDraftSoon = debounce(saveDraft, 400);
function loadDraft() { text.value = drafts[S.thread || '_new'] || ''; grow(); }

// Attachments: pasted, dropped or picked files. Agents get them as files in the
// working folder; plain chats get text files inline.
$('attachBtn').onclick = () => $('fileInput').click();
$('fileInput').onchange = e => { addFiles([...e.target.files]); e.target.value = ''; };
text.addEventListener('paste', e => {
  const files = [...(e.clipboardData?.files || [])];
  if (files.length) { e.preventDefault(); addFiles(files); }
});
const composer = $('composer');
['dragenter', 'dragover'].forEach(t => composer.addEventListener(t, e => { if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); composer.classList.add('drag'); } }));
['dragleave', 'drop'].forEach(t => composer.addEventListener(t, () => composer.classList.remove('drag')));
composer.addEventListener('drop', e => { if (e.dataTransfer?.files.length) { e.preventDefault(); addFiles([...e.dataTransfer.files]); } });
function addFiles(files) {
  for (const f of files) {
    if (f.size > 40e6) { toast(`${f.name}: больше 40 МБ`, {err: true}); continue; }
    S.attachments.push({file: f, name: f.name || 'вставка.png', url: f.type.startsWith('image/') ? URL.createObjectURL(f) : null});
  }
  renderAttachments();
}
function renderAttachments() {
  const box = $('attach'); box.innerHTML = '';
  box.hidden = !S.attachments.length;
  S.attachments.forEach((a, i) => {
    const c = el('div', 'file-chip');
    c.innerHTML = (a.url ? `<img src="${a.url}" alt="">` : icon('file', 'sm')) + `<span class="n">${esc(a.name)}</span><span class="s">${fmtSize(a.file.size)}</span>`;
    c.append(btn('icon-btn sm', icon('x', 'xs'), () => { if (a.url) URL.revokeObjectURL(a.url); S.attachments.splice(i, 1); renderAttachments(); }, 'Убрать'));
    box.append(c);
  });
  grow();
}
const TEXTY = /\.(txt|md|markdown|csv|tsv|json|ya?ml|toml|ini|cfg|conf|log|xml|html?|css|s[ac]ss|less|js|mjs|cjs|ts|tsx|jsx|py|rb|rs|go|java|kt|kts|swift|c|h|cc|cpp|hpp|cs|php|sh|bash|zsh|ps1|bat|sql|lua|r|pl|vue|svelte|dockerfile|gradle|env|gitignore)$/i;
async function attachmentsText(mode) {
  if (!S.attachments.length) return '';
  const list = S.attachments.splice(0);
  renderAttachments();
  if (mode === 'chat') {
    let out = '';
    for (const a of list) {
      if (!(TEXTY.test(a.name) || a.file.type.startsWith('text/')) || a.file.size > 400e3) { toast(`${a.name}: в обычном чате можно приложить только текстовый файл`, {err: true}); continue; }
      const lang = (a.name.split('.').pop() || '').toLowerCase();
      out += `\n\n${a.name}:\n\`\`\`${lang}\n${await a.file.text()}\n\`\`\``;
    }
    return out;
  }
  const stamp = new Date().toISOString().slice(0, 10);
  const lines = [];
  for (const a of list) {
    const safe = a.name.replace(/[\\/:*?"<>|\s]+/g, '_');
    const path = `.kron-uploads/${stamp}/${safe}`;
    try {
      await postJSON('/api/files', {node: S.node, cwd: S.cwd(), op: 'write', path, data: await bytesToB64(a.file)});
      lines.push('📎 ' + path);
    } catch (e) { fail(`${a.name}: ${e.message}`); }
    if (a.url) URL.revokeObjectURL(a.url);
  }
  return lines.length ? '\n\n' + lines.join('\n') : '';
}

async function send(textOverride) {
  let t = (textOverride ?? text.value).trim();
  if ((!t && !S.attachments.length) || !S.pick) return;
  if (S.busy) return;
  const p = S.pick, web = p.kind === 'site';
  const mode = web ? 'web' : (p.kind === 'agent' || S.mode === 'agent') ? 'agent' : 'chat';
  if (textOverride == null) { text.value = ''; delete drafts[S.thread || '_new']; storeJSON('drafts', drafts); }
  setBusy(true);
  try { t = (t + await attachmentsText(mode)).trim(); } catch (e) { fail(e); }
  if (!t) { setBusy(false); return; }
  const body = {thread: S.thread || '', text: t, mode, interactive: true};
  if (p.kind === 'provider') { body.provider = p.name; if (S.models[p.name]) body.model = S.models[p.name]; }
  if (p.kind === 'agent') body.agent = p.name;
  if (web) body.site = p.name;
  if (mode !== 'chat') body.node = S.node;
  if (mode === 'agent') { body.access = S.access; if (S.cwd()) body.cwd = S.cwd(); }
  if (mode !== 'web' && S.effort && effortCapable()) body.effort = S.effort;
  addUser(t); stick = true; toBottom(true);
  try {
    const run = await postJSON('/api/runs', body);
    if (!S.thread) { S.thread = run.thread; setTitle(t.split('\n')[0].slice(0, 80)); }
    loadThreads().catch(() => {});
    await follow(run.run, {retry: () => send(t)});
  } catch (e) {
    const turn = new Turn(); turn.retry = () => send(t); turn.error(String(e.message || e)); turn.finish(); setBusy(false);
  }
}
async function stop() {
  if (!S.run) return;
  await api('/api/runs/' + S.run + '/cancel', {method: 'POST'}).catch(() => {});
}
async function answer(q, value) {
  try { await postJSON(`/api/runs/${S.run}/answer`, {ask: q.id, answer: value}); }
  catch (e) { fail(e); }
}

/** Stream a run into a new assistant turn; reattach after a dropped connection. */
async function follow(runId, {retry} = {}) {
  S.run = runId; setBusy(true);
  const thread = S.thread;
  const ctl = new AbortController(); S.follower = ctl;
  const turn = new Turn({live: true});
  turn.retry = retry;
  const pk = S.pick; turn.label = pk?.kind === 'provider' ? pickModel(pk) : pk?.name || '';
  let finished = false, lost = false;
  try {
    const r = await api('/api/runs/' + runId + '/events', {signal: ctl.signal});
    const reader = r.body.getReader(), dec = new TextDecoder();
    let buf = '';
    for (;;) {
      const {value, done} = await reader.read();
      if (done) break;
      buf += dec.decode(value, {stream: true});
      let i;
      while ((i = buf.indexOf('\n\n')) >= 0) {
        const chunk = buf.slice(0, i); buf = buf.slice(i + 2);
        const data = chunk.split('\n').filter(l => l.startsWith('data:')).map(l => l.slice(5).trimStart()).join('\n');
        if (!data) continue;
        let e; try { e = JSON.parse(data); } catch { continue; }
        switch (e.type) {
          case 'text': turn.addText(e.content); break;
          case 'reasoning': turn.addReasoning(e.content); break;
          case 'tool_start': turn.tool(e.id, e.name, e.args, e.parent); break;
          case 'tool_end': turn.toolDone(e.id, e.name, e.output); break;
          case 'ask': turn.ask(e); break;
          case 'answered': turn.answered(e.id, e.answer); break;
          case 'error': turn.error(e.message); finished = true; break;
          case 'done': finished = true; break;
        }
      }
    }
  } catch (e) {
    if (ctl.signal.aborted) { turn.finish(); return; }
    lost = true;
  }
  if (S.follower !== ctl) { turn.finish(); return; }
  if (!finished) {
    // The stream ended without "done": the network dropped. Reopen the thread,
    // which replays the run from its start if it is still going.
    turn.finish();
    setTimeout(() => { if (S.thread === thread && S.follower === ctl) openThread(thread, {keepDraft: true}).catch(() => {}); }, lost ? 1500 : 300);
    return;
  }
  turn.finish();
  S.run = null; S.follower = null; S.ask = null;
  setBusy(false); loadThreads().catch(() => {});
  if (S.thread === thread) notifyHidden('Ответ готов', turn.texts.join(' ').slice(0, 140) || S.title);
}

// Notifications when the page is in the background (desktop, Android app).
function notifyHidden(title, body) {
  if (!document.hidden || !S.notify) return;
  try {
    if (android?.notice) return android.notice('kron · ' + title, body || '');
    if ('Notification' in window && Notification.permission === 'granted') {
      const n = new Notification('kron · ' + title, {body: body || '', icon: '/icon-192.png', tag: 'kron-' + (S.thread || '')});
      n.onclick = () => { focus(); n.close(); };
    }
  } catch {}
  if (navigator.vibrate) navigator.vibrate(60);
}

// ── Files panel ──────────────────────────────────────────────────────────────
const F = {path: '', parent: null, entries: [], pick: false, view: null};
const rawUrl = path => `/raw/${encodeURIComponent(S.token || '-')}/${encodeURIComponent(S.node)}/` +
  path.replace(/\\/g, '/').replace(/^\/+/, '').split('/').map(encodeURIComponent).join('/');
const joinPath = (dir, name) => dir.replace(/[\\/]+$/, '') + (/^[A-Za-z]:/.test(dir) ? '\\' : '/') + name;
const filesReq = (op, path, extra = {}) => postJSON('/api/files', {node: S.node, cwd: S.cwd(), op, path, ...extra});
async function openFiles(path = '', {pick = false} = {}) {
  closeMenu();
  F.pick = pick;
  const panel = $('panel');
  panel.hidden = false; $('filesBtn').classList.add('on');
  if (isMobile()) toggleSide(false);
  // A file path opens its folder and then the file.
  let dir = path, file = null;
  if (path && /\.[^/\\]+$/.test(path)) {
    try { const st = await filesReq('stat', path); if (!st.dir) { file = st.path; dir = st.path.replace(/[\\/][^\\/]*$/, '') || '/'; } } catch {}
  }
  await listDir(dir);
  if (file) viewFile(file);
}
function closeFiles() { $('panel').hidden = true; $('filesBtn').classList.remove('on'); F.view = null; }
$('filesBtn').onclick = () => $('panel').hidden ? openFiles(S.cwd() || '') : closeFiles();
async function listDir(path) {
  try {
    const r = await filesReq('list', path);
    Object.assign(F, {path: r.path, parent: r.parent, entries: r.entries, view: null, error: null});
  } catch (e) {
    if (path) { fail(e); return listDir(''); }
    Object.assign(F, {path: '', entries: [], error: e.message});
  }
  renderPanel();
}
function renderPanel() {
  const panel = $('panel'); panel.innerHTML = '';
  const n = S.nodeInfo();
  const head = el('div', 'ph');
  head.innerHTML = `<h2>${icon(nodeIcon(n), 'sm')}<span>${F.pick ? 'Выберите рабочую папку' : 'Файлы · ' + esc(S.node || '')}</span></h2>`;
  head.append(
    btn('icon-btn', icon('refresh-cw', 'sm'), () => listDir(F.path), 'Обновить'),
    btn('icon-btn', icon('x'), closeFiles, 'Закрыть'),
  );
  panel.append(head);
  if (F.view) return renderViewer(panel);
  // Breadcrumbs.
  const crumbs = el('div', 'crumbs');
  const win = /^[A-Za-z]:/.test(F.path);
  const parts = F.path.split(/[\\/]/).filter(Boolean);
  const upTo = i => win ? parts.slice(0, i + 1).join('\\') + (i === 0 ? '\\' : '') : '/' + parts.slice(0, i + 1).join('/');
  if (!win) crumbs.append(btn('', icon('folder', 'xs'), () => listDir('/'), 'Корень'));
  parts.forEach((p, i) => {
    if (i || !win) crumbs.append(el('span', 'sl', '/'));
    crumbs.append(btn('', esc(p), () => listDir(upTo(i))));
  });
  panel.append(crumbs);
  requestAnimationFrame(() => crumbs.scrollLeft = crumbs.scrollWidth);
  // Tools.
  const tools = el('div', 'ptools');
  if (F.pick) {
    tools.append(btn('btn sm primary', icon('check', 'sm') + 'Работать в этой папке', () => { setCwd(F.path); closeFiles(); toast('Рабочая папка: ' + shortPath(F.path), {ic: 'folder-open'}); }));
  } else {
    const isCwd = S.cwd() === F.path;
    tools.append(btn('btn sm' + (isCwd ? '' : ''), icon(isCwd ? 'check' : 'folder-open', 'sm') + (isCwd ? 'Рабочая папка' : 'Сделать рабочей'), () => { setCwd(isCwd ? '' : F.path); renderPanel(); }, 'Здесь агент будет работать в этом чате'));
  }
  tools.append(
    btn('btn sm', icon('upload', 'sm') + 'Загрузить', () => {
      const inp = el('input'); inp.type = 'file'; inp.multiple = true;
      inp.onchange = () => uploadTo(F.path, [...inp.files]); inp.click();
    }),
    btn('btn sm', icon('folder-plus', 'sm') + 'Папка', async () => {
      const name = await dialog({title: 'Новая папка', input: true, placeholder: 'Название', ok: 'Создать'});
      if (!name) return;
      await filesReq('mkdir', joinPath(F.path, name)).catch(fail);
      listDir(F.path);
    }),
  );
  panel.append(tools);
  const list = el('div'); list.id = 'fileList';
  if (F.error && !F.entries.length) list.append(el('div', 'pempty', esc(F.error)));
  if (F.parent && F.parent !== F.path) list.append(btn('fe dir', icon('arrow-left', 'sm') + '<span class="n">..</span>', () => listDir(F.parent)));
  const shown = F.pick ? F.entries.filter(e => e.dir) : F.entries;
  for (const e of shown) {
    const full = joinPath(F.path, e.name);
    const ic = e.dir ? 'folder' : /\.(png|jpe?g|gif|webp|svg|avif|ico)$/i.test(e.name) ? 'image' : TEXTY.test(e.name) ? 'file-code' : 'file';
    const b = btn('fe' + (e.dir ? ' dir' : ''), icon(ic, 'sm') + `<span class="n">${esc(e.name)}</span><span class="s">${e.dir ? '' : fmtSize(e.size)}</span>`, () => e.dir ? listDir(full) : viewFile(full));
    b.title = e.name + (e.modified ? ' · изменён ' + fmtTime(e.modified) : '');
    list.append(b);
  }
  if (!shown.length && !F.error) list.append(el('div', 'pempty', F.pick ? 'Внутри нет папок' : 'Папка пуста'));
  panel.append(list);
  // Drop files onto the panel to upload.
  list.ondragover = e => { e.preventDefault(); };
  list.ondrop = e => { e.preventDefault(); if (e.dataTransfer.files.length) uploadTo(F.path, [...e.dataTransfer.files]); };
}
async function uploadTo(dir, files) {
  for (const f of files) {
    try {
      await filesReq('write', joinPath(dir, f.name), {data: await bytesToB64(f)});
      toast('Загружено: ' + f.name, {ic: 'check', timeout: 2000});
    } catch (e) { fail(`${f.name}: ${e.message}`); }
  }
  listDir(dir);
}
async function viewFile(path) {
  F.view = {path, loading: true}; renderPanel();
  try {
    const r = await filesReq('read', path);
    F.view = {path: r.path, size: r.size, bytes: b64ToBytes(r.data), mode: 'auto'};
  } catch (e) { F.view = {path, error: e.message}; }
  renderPanel();
}
function renderViewer(panel) {
  const v = F.view, name = v.path.split(/[\\/]/).pop(), ext = (name.split('.').pop() || '').toLowerCase();
  const box = el('div'); box.id = 'viewer';
  const head = el('div', 'vh');
  head.append(btn('icon-btn sm', icon('arrow-left', 'sm'), () => { F.view = null; renderPanel(); }, 'К списку'));
  const t = el('b'); t.textContent = name; t.title = v.path; head.append(t);
  const body = el('div', 'vb');
  const isHtml = /^html?$/.test(ext), isMd = /^(md|markdown)$/.test(ext);
  if (v.bytes) {
    head.append(
      btn('icon-btn sm', icon('copy', 'sm'), e => copy(v.path, e.currentTarget), 'Копировать путь'),
      btn('icon-btn sm', icon('message-square', 'sm'), () => { text.value += (text.value && !/\s$/.test(text.value) ? ' ' : '') + v.path; grow(); text.focus(); }, 'Вставить путь в сообщение'),
      btn('icon-btn sm', icon('download', 'sm'), () => download(name, new Blob([v.bytes])), 'Скачать'),
      btn('icon-btn sm', icon('external-link', 'sm'), () => window.open(rawUrl(v.path), '_blank', 'noopener'), 'Открыть в новой вкладке'),
    );
    if (isHtml || isMd) {
      const src = v.mode === 'source';
      head.insertBefore(btn('btn sm ghost', src ? 'Просмотр' : 'Код', () => { v.mode = src ? 'auto' : 'source'; renderPanel(); }), head.children[2]);
    }
  }
  box.append(head, body); panel.append(box);
  if (v.loading) { body.append(el('div', 'pempty', icon('loader-circle', 'spin') + ' Открываю…')); return; }
  if (v.error) { body.append(el('div', 'pempty', esc(v.error))); return; }
  const mime = {png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', svg: 'image/svg+xml', avif: 'image/avif', ico: 'image/x-icon', mp4: 'video/mp4', webm: 'video/webm', mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg', pdf: 'application/pdf'}[ext];
  if (mime?.startsWith('image/') && ext !== 'svg') {
    const img = el('img'); img.src = URL.createObjectURL(new Blob([v.bytes], {type: mime})); img.alt = name; body.append(img); return;
  }
  if (ext === 'svg' && v.mode !== 'source') { const img = el('img'); img.src = URL.createObjectURL(new Blob([v.bytes], {type: mime})); body.append(img); return; }
  if (mime?.startsWith('video/') || mime?.startsWith('audio/')) {
    const m = el(mime.startsWith('video/') ? 'video' : 'audio'); m.controls = true; m.src = URL.createObjectURL(new Blob([v.bytes], {type: mime})); body.append(m); return;
  }
  if (ext === 'pdf' || (isHtml && v.mode !== 'source')) {
    const f = el('iframe'); f.src = rawUrl(v.path); f.title = name;
    if (isHtml) f.setAttribute('sandbox', 'allow-scripts allow-forms allow-popups allow-modals');
    body.append(f); return;
  }
  let txt;
  try { txt = new TextDecoder('utf-8', {fatal: true}).decode(v.bytes); } catch { txt = null; }
  if (txt == null || v.size > 3e6) { body.append(el('div', 'pempty', `Двоичный файл, ${fmtSize(v.size)}. Его можно скачать.`)); return; }
  if (isMd && v.mode !== 'source') { const m = el('div', 'md'); m.innerHTML = md(txt); highlight(m); body.append(m); return; }
  const lang = {rs: 'rust', py: 'python', js: 'javascript', mjs: 'javascript', ts: 'typescript', tsx: 'typescript', jsx: 'javascript', sh: 'bash', ps1: 'powershell', yml: 'yaml', md: 'markdown', htm: 'xml', html: 'xml', toml: 'ini', h: 'c', hpp: 'cpp', kt: 'kotlin', rb: 'ruby'}[ext] || ext;
  const c = el('div', 'code');
  c.innerHTML = `<pre><code data-lang="${esc(hljs.getLanguage(lang) ? lang : '')}"></code></pre>`;
  c.querySelector('code').textContent = txt;
  if (txt.length < 300e3) highlight(c);
  body.append(c);
}

// ── Settings ─────────────────────────────────────────────────────────────────
const TABS = [
  ['accounts', 'sparkles', 'Подписки'],
  ['api', 'key-round', 'Свои API'],
  ['local', 'cpu', 'Модели на устройстве'],
  ['machines', 'server', 'Машины'],
  ['look', 'sun-moon', 'Интерфейс'],
  ['config', 'file-code', 'config.toml'],
  ['keys', 'command', 'Клавиши'],
];
let settingsEl = null, settingsTab = 'accounts', modelTimer = null;
function openSettings(tab = settingsTab) {
  closeMenu();
  if (isMobile()) toggleSide(false);
  settingsTab = tab;
  if (!settingsEl) {
    settingsEl = el('div'); settingsEl.id = 'settings';
    settingsEl.onclick = e => { if (e.target === settingsEl) closeSettings(); };
    document.body.append(settingsEl);
  }
  const sheet = el('div', 'sheet'); sheet.setAttribute('role', 'dialog'); sheet.setAttribute('aria-modal', 'true');
  const nav = el('nav', '', '<h2>Настройки</h2>'); nav.setAttribute('role', 'tablist');
  for (const [id, ic, label] of TABS) {
    const b = btn('', icon(ic, 'sm') + esc(label), () => openSettings(id));
    b.setAttribute('role', 'tab'); b.setAttribute('aria-selected', id === tab);
    nav.append(b);
  }
  const pane = el('div', 'pane');
  const header = el('header', '', `<h3>${esc(TABS.find(t => t[0] === tab)[2])}</h3>`);
  header.append(btn('btn sm', 'Готово', closeSettings));
  const body = el('div', 'body');
  pane.append(header, body); sheet.append(nav, pane);
  settingsEl.innerHTML = ''; settingsEl.append(sheet);
  ({accounts: tabAccounts, api: tabApi, local: tabLocal, machines: tabMachines, look: tabLook, config: tabConfig, keys: tabKeys})[tab](body);
  nav.querySelector('[aria-selected="true"]')?.scrollIntoView({inline: 'center', block: 'nearest'});
}
function closeSettings() { settingsEl?.remove(); settingsEl = null; clearTimeout(modelTimer); }
$('openSettings').onclick = () => openSettings('accounts');

function card(ic, title, sub, ...right) {
  const c = el('div', 'card'), n = el('div', 'n');
  n.innerHTML = `<b>${title}</b>` + (sub ? `<small>${esc(sub)}</small>` : '');
  if (ic) c.innerHTML = icon(ic);
  c.append(n, ...right);
  return c;
}
function tabAccounts(body) {
  body.append(el('p', 'lead', 'Войдите в подписки, и их модели появятся в выборе модели. Вход хранится на хабе; официальные CLI продолжают работать.'));
  const subs = S.info.providers.filter(p => p.oauth);
  if (!subs.length) body.append(el('p', 'lead', 'В config.toml нет подписок (kind = codex / claude / gemini / antigravity). Добавьте их во вкладке config.toml.'));
  for (const p of subs) {
    const status = p.logged_in ? '<span class="badge ok">вход выполнен</span>' : '<span class="badge warn">нужен вход</span>';
    const b = btn('btn sm' + (p.logged_in ? '' : ' primary'), icon('log-in', 'sm') + (p.logged_in ? 'Войти заново' : 'Войти'));
    const c = card('sparkles', esc(p.name) + ' ' + status, `${KIND[p.kind] || p.kind} · ${p.model}`, b);
    b.onclick = () => signIn(p, c);
    body.append(c);
  }
}
async function signIn(p, row) {
  let r;
  try { r = await postJSON('/api/login/' + p.name, {}); } catch (e) { return fail(e); }
  // In the Android app this navigation opens the system browser.
  if (/kron-android/.test(navigator.userAgent)) location.href = r.url; else window.open(r.url, '_blank', 'noopener');
  row.nextElementSibling?.classList.contains('paste') && row.nextElementSibling.remove();
  const box = el('div', 'paste'), inp = el('input', 'input');
  inp.placeholder = r.paste ? 'Вставьте код со страницы входа' : 'Если не сработало само — вставьте адрес из браузера';
  const ok = btn('btn primary', 'Готово');
  box.append(inp, ok); row.after(box);
  const done = async () => { clearInterval(timer); await loadInfo(); toast('Вход выполнен: ' + p.name, {ic: 'check'}); if (settingsEl) openSettings('accounts'); };
  ok.onclick = async () => {
    try { await postJSON('/api/login/' + p.name + '/code', {code: inp.value}); done(); } catch (e) { fail(e); }
  };
  const timer = setInterval(async () => {
    if (!settingsEl) { clearInterval(timer); return; }
    const st = await getJSON('/api/login/' + p.name).catch(() => ({}));
    if (st.state === 'done') done();
    else if (st.state === 'error') { clearInterval(timer); fail(st.error); }
  }, 2000);
}

const PRESETS = {
  deepseek: ['DeepSeek', 'openai', 'https://api.deepseek.com/v1', 'deepseek-chat'],
  openrouter: ['OpenRouter', 'openai', 'https://openrouter.ai/api/v1', ''],
  openai: ['OpenAI', 'responses', 'https://api.openai.com/v1', ''],
  anthropic: ['Anthropic', 'anthropic', 'https://api.anthropic.com', ''],
  ollama: ['Ollama / LM Studio на этом ПК', 'openai', 'http://localhost:11434/v1', ''],
  cliproxy: ['CLIProxyAPI', 'anthropic', 'http://127.0.0.1:8317', ''],
  custom: ['Свой сервер', 'openai', '', ''],
};
function tabApi(body) {
  body.append(el('p', 'lead', 'Любой сервис с OpenAI-, Responses- или Anthropic-совместимым API: DeepSeek, OpenRouter, локальные модели. Провайдер добавится в config.toml.'));
  const apis = S.info.providers.filter(p => !p.oauth && p.kind !== 'local');
  if (apis.length) {
    body.append(el('h4', '', 'Подключены'));
    for (const p of apis) body.append(card('key-round', esc(p.name), `${KIND[p.kind] || p.kind} · ${p.model}`));
  }
  body.append(el('h4', '', 'Добавить'));
  const form = el('div', 'card col');
  form.innerHTML = `<div class="grid2">
    <label class="field"><span>Сервис</span><select class="select" name="preset">${Object.entries(PRESETS).map(([k, v]) => `<option value="${k}">${esc(v[0])}</option>`).join('')}</select></label>
    <label class="field"><span>Имя в kron</span><input class="input" name="name" placeholder="deepseek" autocomplete="off"></label>
    <label class="field"><span>Протокол</span><select class="select" name="kind"><option value="openai">OpenAI Chat Completions</option><option value="responses">OpenAI Responses</option><option value="anthropic">Anthropic Messages</option></select></label>
    <label class="field"><span>Модель</span><input class="input" name="model" placeholder="например, deepseek-chat" autocomplete="off"></label>
    <label class="field" style="grid-column:1/-1"><span>Адрес API</span><input class="input" name="base" autocomplete="off"></label>
    <label class="field" style="grid-column:1/-1"><span>Ключ API</span><input class="input" name="key" type="password" autocomplete="off" placeholder="хранится в config.toml на хабе"></label>
  </div>`;
  const f = n => form.querySelector(`[name=${n}]`);
  const preset = () => { const [, kind, base, model] = PRESETS[f('preset').value]; f('kind').value = kind; f('base').value = base; f('model').value = model; f('name').value = f('preset').value === 'custom' ? '' : f('preset').value; };
  f('preset').onchange = preset; preset();
  const add = btn('btn primary', icon('plus', 'sm') + 'Добавить провайдера', async () => {
    const name = f('name').value.trim(), model = f('model').value.trim();
    if (!/^[\w.-]+$/.test(name)) return fail('Имя: латиница, цифры, - _ .');
    if (!model) return fail('Укажите модель');
    if (S.info.providers.some(p => p.name === name)) return fail(`Провайдер «${name}» уже есть`);
    const q = s => '"' + s.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
    let toml = `\n[providers.${name}]\nkind = ${q(f('kind').value)}\nmodel = ${q(model)}\n`;
    if (f('base').value.trim()) toml += `base_url = ${q(f('base').value.trim())}\n`;
    if (f('key').value.trim()) toml += `api_key = ${q(f('key').value.trim())}\n`;
    try {
      const c = await getJSON('/api/config');
      await api('/api/config', {method: 'PUT', json: {text: c.text.trimEnd() + '\n' + toml}});
      await loadInfo();
      S.pick = {kind: 'provider', name}; storeJSON('pick', S.pick); refreshControls();
      toast('Добавлен ' + name, {ic: 'check'}); openSettings('api');
    } catch (e) { fail(e); }
  });
  const row = el('div'); row.style.marginTop = '14px'; row.append(add);
  form.append(row); body.append(form);
}
async function tabLocal(body) {
  body.append(el('p', 'lead', 'Работают на этой машине, без интернета и лимитов. Модель загружается в память при первом запросе.'));
  const list = el('div'); body.append(list);
  const draw = async () => {
    let r;
    try { r = await getJSON('/api/models'); } catch (e) { list.innerHTML = ''; list.append(el('p', 'lead', esc(e.message))); return; }
    list.innerHTML = '';
    if (!r.server) list.append(el('p', 'lead', 'На этой машине нет llama-server. В приложении для Android он встроен; на ПК установите llama.cpp (winget install llama.cpp / brew install llama.cpp).'));
    let busy = false;
    for (const m of r.models) {
      const p = m.progress, loading = p && !p.finished;
      const right = [];
      if (loading) { busy = true; right.push(el('span', 'badge', Math.floor(100 * p.done / (p.total || 1)) + '%')); }
      else if (m.installed) {
        right.push(el('span', 'badge ok', 'скачана'));
        right.push(btn('btn sm danger', icon('trash-2', 'sm') + 'Удалить', async () => {
          if (!await dialog({title: 'Удалить ' + m.title + '?', text: 'Файл модели удалится с устройства, его можно скачать снова.', ok: 'Удалить', danger: true})) return;
          await api('/api/models/' + m.file, {method: 'DELETE'}).catch(fail); await loadInfo(); draw();
        }));
      } else right.push(btn('btn sm primary', icon('download', 'sm') + 'Скачать', async () => { await api('/api/models/' + m.file, {method: 'POST'}).catch(fail); draw(); }));
      const c = card('cpu', esc(m.title), `${fmtSize(m.size)} · ${m.note}`, ...right);
      if (loading) { const bar = el('div', 'bar', `<i style="width:${(100 * p.done / (p.total || 1)).toFixed(1)}%"></i>`); c.querySelector('.n').append(bar); }
      if (p?.error) { const e = el('small'); e.style.color = 'var(--danger)'; e.textContent = p.error; c.querySelector('.n').append(e); }
      list.append(c);
    }
    clearTimeout(modelTimer);
    if (busy && settingsEl) modelTimer = setTimeout(draw, 1000);
    if (S.modelsBusy && !busy) loadInfo(); // a finished model becomes a provider
    S.modelsBusy = busy;
  };
  draw();
}
function tabMachines(body) {
  body.append(el('p', 'lead', 'Хаб хранит чаты и логины, а агенты работают на любой подключённой машине: ПК, сервере, телефоне.'));
  for (const n of S.info.nodes) {
    const tags = [n.hub ? '<span class="badge accent">хаб</span>' : '', n.browser ? '<span class="badge">браузер</span>' : ''].join(' ');
    const open = btn('btn sm', icon('folder-open', 'sm') + 'Файлы', () => { closeSettings(); S.node = n.name; refreshControls(); openFiles(''); });
    body.append(card(nodeIcon(n), `<span class="dot"></span>${esc(n.name)} ${tags}`, `${n.os || ''} · ${n.cwd || ''}`, open));
  }
  body.append(el('h4', '', 'Подключить ещё одну машину'));
  body.append(el('p', 'lead', 'Установите kron на машину, добавьте в её ~/.kron/config.toml адрес этого хаба и тот же токен, затем запустите kron node:'));
  const snip = el('pre', 'snippet');
  snip.textContent = `[hub]\nurl   = "${location.origin}"\ntoken = "${S.token ? '<ваш токен>' : 'длинный-секрет'}"\nnode  = "pc"\n\n# затем в терминале:\nkron node`;
  body.append(snip, btn('btn sm', icon('copy', 'sm') + 'Копировать', e => copy(snip.textContent, e.currentTarget)));
}
function tabLook(body) {
  const seg = (options, cur, onPick) => {
    const s = el('div', 'seg');
    for (const [k, label, ic] of options) {
      const b = btn('', (ic ? icon(ic, 'sm') : '') + esc(label), () => { onPick(k); openSettings('look'); });
      b.setAttribute('aria-pressed', k === cur); s.append(b);
    }
    return s;
  };
  const setting = (title, sub, control) => { const r = el('div', 'setting'); r.innerHTML = `<div class="n"><b>${esc(title)}</b><small>${esc(sub)}</small></div>`; r.append(control); return r; };
  const sw = (on, onChange) => { const b = btn('switch'); b.setAttribute('role', 'switch'); b.setAttribute('aria-checked', on); b.onclick = () => { const v = b.getAttribute('aria-checked') !== 'true'; b.setAttribute('aria-checked', v); onChange(v); }; return b; };
  body.append(
    setting('Тема', 'Системная следует настройке устройства', seg([['system', 'Система', 'monitor'], ['light', 'Светлая', 'sun'], ['dark', 'Тёмная', 'moon']], store('theme') || 'system', k => { store('theme', k === 'system' ? null : k); applyTheme(); })),
    setting('Размер текста', 'Для чтения длинных ответов', seg([['s', 'Мельче'], ['', 'Обычный'], ['l', 'Крупнее']], store('textsize') || '', k => { store('textsize', k || null); applyTheme(); })),
    setting('Enter отправляет', 'Иначе Ctrl+Enter; на телефоне Enter всегда переносит строку', sw(S.enterSends, v => { S.enterSends = v; store('enter', v ? null : 'off'); })),
    setting('Уведомления', 'Когда ответ готов или агент ждёт решения, а вкладка в фоне', sw(S.notify, async v => {
      if (v && !android && 'Notification' in window && Notification.permission !== 'granted') {
        const p = await Notification.requestPermission();
        if (p !== 'granted') { toast('Браузер не разрешил уведомления', {err: true}); v = false; openSettings('look'); }
      }
      S.notify = v; store('notify', v ? 'on' : null);
    })),
  );
  body.append(el('p', 'hint-text', `kron ${S.info.version || ''} · хаб ${S.info.hub}`));
}
async function tabConfig(body) {
  body.append(el('p', 'lead', 'Провайдеры, ключи API, агенты-пресеты и хаб. «Сохранить» применяет изменения сразу; адрес и токен хаба — после перезапуска.'));
  const ta = el('textarea', 'textarea'); ta.id = 'cfgText'; ta.spellcheck = false; ta.setAttribute('autocapitalize', 'off');
  const bar = el('div', 'cfg-bar'), msg = el('div', 'msg');
  const save = btn('btn primary', 'Сохранить', async () => {
    try {
      await api('/api/config', {method: 'PUT', json: {text: ta.value}});
      msg.className = 'msg'; msg.textContent = 'Сохранено и применено'; toast('Настройки применены', {ic: 'check'});
      await loadInfo();
    } catch (e) { msg.className = 'msg err'; msg.textContent = e.message; }
  });
  ta.onkeydown = e => { if ((e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); save.click(); } };
  bar.append(msg, save); body.append(ta, bar);
  try { const c = await getJSON('/api/config'); ta.value = c.text; msg.textContent = c.path; } catch (e) { msg.className = 'msg err'; msg.textContent = e.message; }
}
function tabKeys(body) {
  const mod = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl';
  const rows = [
    [[mod, 'K'], 'Команды и поиск по чатам'], [[mod, 'Shift', 'O'], 'Новый чат'], [[mod, 'B'], 'Показать или скрыть чаты'],
    [[mod, 'Shift', 'E'], 'Файлы на машине'], [[mod, ','], 'Настройки'], [['Enter'], 'Отправить'], [['Shift', 'Enter'], 'Новая строка'],
    [['Esc'], 'Остановить ответ / закрыть окно'], [['↑'], 'Изменить последнее сообщение (в пустом поле)'], [['/'], 'Перейти в поле ввода'],
  ];
  const g = el('div', 'shortcuts');
  for (const [keys, what] of rows) g.append(el('div', '', keys.map(k => `<span class="kbd">${esc(k)}</span>`).join('')), el('div', '', esc(what)));
  body.append(g);
}

// ── Command palette ──────────────────────────────────────────────────────────
let paletteEl = null;
function openPalette() {
  if (paletteEl) return closePalette();
  closeMenu();
  paletteEl = el('div'); paletteEl.id = 'palette';
  const box = el('div', 'box'), q = el('div', 'q', icon('search')), inp = el('input'), res = el('div', 'res');
  inp.placeholder = 'Команда или поиск по чатам…'; inp.setAttribute('aria-label', 'Команда или поиск');
  q.append(inp, el('span', 'kbd', 'Esc')); box.append(q, res); paletteEl.append(box);
  paletteEl.onclick = e => { if (e.target === paletteEl) closePalette(); };
  document.body.append(paletteEl);
  const actions = [
    ['square-pen', 'Новый чат', () => openThread(null)],
    ['folder', 'Файлы на машине', () => openFiles(S.cwd() || '')],
    ['folder-open', 'Выбрать рабочую папку', () => openFiles(S.cwd() || '', {pick: true})],
    ['wrench', S.mode === 'agent' ? 'Переключить на обычный чат' : 'Переключить на агента', () => { S.mode = S.mode === 'agent' ? 'chat' : 'agent'; store('mode', S.mode); refreshControls(); }],
    ['shield-check', 'Доступ: спрашивать перед командами', () => { S.access = 'ask'; store('access', 'ask'); refreshControls(); }],
    ['eye', 'Доступ: только чтение', () => { S.access = 'read'; store('access', 'read'); refreshControls(); }],
    ['shield', 'Доступ: полный', () => { S.access = 'full'; store('access', 'full'); refreshControls(); }],
    ['sun-moon', 'Сменить тему', () => { const t = store('theme') || 'system'; store('theme', t === 'dark' ? 'light' : t === 'light' ? null : 'dark'); applyTheme(); }],
    ['settings', 'Настройки', () => openSettings('accounts')],
    ['log-in', 'Войти в подписку', () => openSettings('accounts')],
    ['key-round', 'Добавить API-провайдера', () => openSettings('api')],
    ['cpu', 'Скачать локальную модель', () => openSettings('local')],
    ['server', 'Подключить машину', () => openSettings('machines')],
  ];
  if (S.thread) actions.push(
    ['download', 'Скачать чат как Markdown', () => exportThread(S.thread, S.title)],
    ['git-branch', 'Копия этого чата', () => branch(S.thread)],
    ['pencil', 'Переименовать чат', () => renameThread({name: S.thread, title: S.title})],
  );
  let items = [], sel = 0, found = [];
  const draw = () => {
    const ql = inp.value.trim().toLowerCase();
    items = [];
    res.innerHTML = '';
    const acts = actions.filter(a => !ql || a[1].toLowerCase().includes(ql));
    if (acts.length) res.append(el('div', 'sec', 'Действия'));
    for (const [ic, label, run] of acts) items.push({ic, label, run});
    const chats = (ql ? found : S.threads.slice(0, 8)).map(t => ({ic: 'message-square', label: t.title || t.name, sub: t.snippet && !(t.title || '').startsWith(t.snippet.slice(0, 40)) ? t.snippet : '', run: () => openThread(t.name)}));
    let i = 0;
    const add = it => {
      const b = el('button', 'mi' + (i === sel ? ' kb' : ''), icon(it.ic) + `<span class="txt"><b>${esc(it.label)}</b>${it.sub ? `<small>${esc(it.sub)}</small>` : ''}</span>`);
      const idx = i++;
      b.onmouseenter = () => { sel = idx; res.querySelectorAll('.mi').forEach((x, j) => x.classList.toggle('kb', j === sel)); };
      b.onclick = () => { closePalette(); it.run(); };
      res.append(b);
    };
    items.forEach(add);
    if (chats.length) { res.append(el('div', 'sec', ql ? 'Чаты' : 'Недавние чаты')); chats.forEach(c => { items.push(c); add(c); }); }
    if (!items.length) res.append(el('div', 'empty', 'Ничего не найдено'));
    res.querySelector('.kb')?.scrollIntoView({block: 'nearest'});
  };
  const searchChats = debounce(async () => {
    const ql = inp.value.trim();
    found = ql ? await getJSON('/api/threads?q=' + encodeURIComponent(ql)).catch(() => []) : [];
    draw();
  }, 200);
  inp.oninput = () => { sel = 0; draw(); searchChats(); };
  inp.onkeydown = e => {
    if (e.key === 'ArrowDown') { e.preventDefault(); sel = Math.min(sel + 1, items.length - 1); draw(); }
    if (e.key === 'ArrowUp') { e.preventDefault(); sel = Math.max(sel - 1, 0); draw(); }
    if (e.key === 'Enter') { e.preventDefault(); const it = items[sel]; if (it) { closePalette(); it.run(); } }
    if (e.key === 'Escape') { e.preventDefault(); closePalette(); }
  };
  draw(); inp.focus();
}
function closePalette() { paletteEl?.remove(); paletteEl = null; }

// ── Header thread menu & global keys ─────────────────────────────────────────
$('threadMenuBtn').onclick = () => {
  const t = S.threads.find(x => x.name === S.thread) || {name: S.thread, title: S.title};
  threadMenu($('threadMenuBtn'), t);
};
document.addEventListener('keydown', e => {
  const mod = e.ctrlKey || e.metaKey, k = e.key.toLowerCase();
  if (mod && k === 'k') { e.preventDefault(); openPalette(); }
  else if (mod && e.shiftKey && k === 'o') { e.preventDefault(); openThread(null); }
  else if (mod && !e.shiftKey && k === 'b') { e.preventDefault(); toggleSide(); }
  else if (mod && e.shiftKey && k === 'e') { e.preventDefault(); $('panel').hidden ? openFiles(S.cwd() || '') : closeFiles(); }
  else if (mod && k === ',') { e.preventDefault(); openSettings(); }
  else if (e.key === 'Escape') {
    if (paletteEl) closePalette();
    else if (settingsEl) closeSettings();
    else if (!$('panel').hidden && isMobile()) closeFiles();
    else if (document.body.classList.contains('drawer')) toggleSide(false);
    else if (S.busy && !S.ask) stop();
  } else if (e.key === '/' && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement?.tagName) && !document.activeElement?.isContentEditable) {
    e.preventDefault(); text.focus();
  }
});
addEventListener('beforeunload', saveDraft);
document.addEventListener('visibilitychange', () => { if (!document.hidden && S.info) { loadThreads().catch(() => {}); loadInfo().catch(() => {}); } });

// ── Info & boot ──────────────────────────────────────────────────────────────
async function loadInfo() {
  S.info = await getJSON('/api/info');
  const n = S.info.nodes.length;
  $('nodes').innerHTML = `<span class="dot"></span><span>${n} ${plural(n, 'машина', 'машины', 'машин')} онлайн: ${S.info.nodes.map(x => esc(x.name)).join(', ')}</span>`;
  $('nodes').title = S.info.nodes.map(x => `${x.name} · ${x.os} · ${x.cwd}`).join('\n');
  refreshControls();
}
async function boot() {
  try {
    await loadInfo();
    await loadThreads();
    const last = params.get('thread');
    await openThread(last && S.threads.some(t => t.name === last) ? last : null);
    setInterval(() => { if (!document.hidden) loadInfo().catch(() => {}); }, 30000);
  } catch (e) {
    if (!$('login').hidden) return;
    log.innerHTML = '';
    const t = new Turn(); t.retry = () => location.reload(); t.error(String(e.message || e));
  }
}
boot();
