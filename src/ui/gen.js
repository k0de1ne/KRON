'use strict';
/* kron generative UI: renders the interfaces the agent builds (A2UI v0.9-style
   surfaces: flat components, a data model, {"path"} bindings, actions), keeps the
   person's edits, and reports how the interface is used so kron can learn.
   Loaded after app.js and uses its helpers (el, btn, icon, esc, md, api, toast…). */

// ── JSON Pointer ─────────────────────────────────────────────────────────────
const ptr = {
  split(p) { return p === '' || p === '/' ? [] : p.replace(/^\//, '').split('/').map(t => t.replace(/~1/g, '/').replace(/~0/g, '~')); },
  join(base, rel) { return base.replace(/\/$/, '') + '/' + rel.replace(/^\//, ''); },
  get(obj, p) {
    let cur = obj;
    for (const t of ptr.split(p)) { if (cur == null) return undefined; cur = cur[Array.isArray(cur) && t === '-' ? cur.length - 1 : t]; }
    return cur;
  },
  set(obj, p, value) {
    const ts = ptr.split(p);
    if (!ts.length) return value;
    let cur = obj;
    ts.forEach((t, i) => {
      const last = i === ts.length - 1;
      if (Array.isArray(cur)) {
        const idx = t === '-' ? cur.length : +t;
        if (last) { if (value === undefined) cur.splice(idx, 1); else cur[idx] = value; return; }
        cur = cur[idx] ??= /^\d+$/.test(ts[i + 1]) || ts[i + 1] === '-' ? [] : {};
      } else {
        if (last) { if (value === undefined) delete cur[t]; else cur[t] = value; return; }
        if (cur[t] == null || typeof cur[t] !== 'object') cur[t] = /^\d+$/.test(ts[i + 1]) || ts[i + 1] === '-' ? [] : {};
        cur = cur[t];
      }
    });
    return obj;
  },
  /** Does a change at `a` affect what is read at `b` (or the other way round)? */
  overlaps(a, b) { const x = a.replace(/\/$/, '') + '/', y = b.replace(/\/$/, '') + '/'; return x.startsWith(y) || y.startsWith(x); },
};

// ── Expressions: {"expr": "..."} computed on the device, without the model ───
/* A small safe language (no eval): numbers, 'text', true/false/null, data paths
   (/a/b, @field inside a child template, * collects from every element),
   + - * / % == != < <= > >= && || ! ?: ?? and the functions in FN. */
const EXPR_CACHE = new Map();
function exprTokens(src) {
  const out = [], seg = /[\p{L}\p{N}_\-.~$]/u;
  let i = 0;
  const operand = () => { const t = out.at(-1); return !t || (t.t === 'op' && t.v !== ')'); };
  const readSegs = (path) => {
    while (src[i] === '/' && (src[i + 1] === '*' || seg.test(src[i + 1] || ''))) {
      i++;
      if (src[i] === '*') { path.push('*'); i++; continue; }
      let s = '';
      while (i < src.length && seg.test(src[i])) s += src[i++];
      path.push(s.replace(/~1/g, '/').replace(/~0/g, '~'));
    }
    return path;
  };
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) { i++; continue; }
    if (/[0-9]/.test(c) || (c === '.' && /[0-9]/.test(src[i + 1] || ''))) {
      const m = /^\d*\.?\d+(?:e[+-]?\d+)?/i.exec(src.slice(i)); out.push({t: 'num', v: +m[0]}); i += m[0].length; continue;
    }
    if (c === '"' || c === "'") {
      let s = ''; i++;
      while (i < src.length && src[i] !== c) { if (src[i] === '\\' && i + 1 < src.length) i++; s += src[i++]; }
      if (src[i] !== c) throw new Error('не закрыта строка');
      i++; out.push({t: 'str', v: s}); continue;
    }
    if (c === '/' && operand()) {
      const path = (src[i + 1] === '*' || seg.test(src[i + 1] || '')) ? readSegs([]) : (i++, []);
      out.push({t: 'path', abs: true, v: path}); continue;
    }
    if (c === '@') {
      i++; const path = [];
      if (seg.test(src[i] || '')) { let s = ''; while (i < src.length && seg.test(src[i])) s += src[i++]; path.push(s); }
      out.push({t: 'path', abs: false, v: readSegs(path)}); continue;
    }
    if (/[\p{L}_]/u.test(c)) { let s = ''; while (i < src.length && /[\p{L}\p{N}_]/u.test(src[i])) s += src[i++]; out.push({t: 'id', v: s}); continue; }
    const two = src.slice(i, i + 2);
    if (['==', '!=', '<=', '>=', '&&', '||', '??'].includes(two)) { out.push({t: 'op', v: two}); i += 2; continue; }
    if ('+-*/%<>!?:(),'.includes(c)) { out.push({t: 'op', v: c}); i++; continue; }
    throw new Error(`непонятный символ «${c}»`);
  }
  return out;
}
function parseExpr(src) {
  const ts = exprTokens(src);
  let k = 0;
  const peek = v => ts[k]?.t === 'op' && ts[k].v === v;
  const take = v => { if (!peek(v)) throw new Error(`ожидалось «${v}»`); k++; };
  const BIN = [['??'], ['||'], ['&&'], ['==', '!='], ['<', '<=', '>', '>='], ['+', '-'], ['*', '/', '%']];
  const ternary = () => {
    const c = binary(0);
    if (!peek('?')) return c;
    k++; const a = ternary(); take(':'); const b = ternary();
    return {k: '?', c, a, b};
  };
  const binary = lv => {
    if (lv >= BIN.length) return unary();
    let left = binary(lv + 1);
    while (ts[k]?.t === 'op' && BIN[lv].includes(ts[k].v)) { const op = ts[k++].v; left = {k: 'bin', op, a: left, b: binary(lv + 1)}; }
    return left;
  };
  const unary = () => {
    if (peek('!') || peek('-')) { const op = ts[k++].v; return {k: 'un', op, a: unary()}; }
    return primary();
  };
  const primary = () => {
    const t = ts[k++];
    if (!t) throw new Error('формула оборвалась');
    if (t.t === 'num' || t.t === 'str') return {k: 'lit', v: t.v};
    if (t.t === 'path') return {k: 'path', abs: t.abs, v: t.v};
    if (t.t === 'op' && t.v === '(') { const e = ternary(); take(')'); return e; }
    if (t.t === 'id') {
      if (t.v === 'true' || t.v === 'false') return {k: 'lit', v: t.v === 'true'};
      if (t.v === 'null') return {k: 'lit', v: null};
      if (!FN[t.v]) throw new Error(`нет функции «${t.v}»`);
      take('(');
      const args = [];
      if (!peek(')')) { args.push(ternary()); while (peek(',')) { k++; args.push(ternary()); } }
      take(')');
      return {k: 'call', f: t.v, args};
    }
    throw new Error(`неожиданное «${t.v}»`);
  };
  const ast = ternary();
  if (k < ts.length) throw new Error(`лишнее «${ts[k].v}»`);
  return ast;
}
/** Value of an expression; the data paths it reads are tracked like bindings. */
function expr(src, scope = '') {
  let ast = EXPR_CACHE.get(src);
  if (!ast) {
    try { ast = parseExpr(String(src)); } catch (e) { throw new Error(`формула «${src}»: ${e.message}`); }
    if (EXPR_CACHE.size > 1000) EXPR_CACHE.clear();
    EXPR_CACHE.set(src, ast);
  }
  return evalNode(ast, scope);
}
function wild(cur, segs) {
  for (let i = 0; i < segs.length; i++) {
    if (cur == null) return undefined;
    if (segs[i] === '*') {
      const all = Array.isArray(cur) ? cur : typeof cur === 'object' ? Object.values(cur) : [];
      return all.flatMap(x => { const v = wild(x, segs.slice(i + 1)); return v === undefined ? [] : [v]; });
    }
    cur = cur[segs[i]];
  }
  return cur;
}
function evalNode(n, scope) {
  switch (n.k) {
    case 'lit': return n.v;
    case 'path': {
      const base = n.abs ? [] : ptr.split(scope || '');
      const segs = base.concat(n.v), star = segs.indexOf('*');
      tracking?.add('/' + (star < 0 ? segs : segs.slice(0, star)).map(t => String(t).replace(/~/g, '~0').replace(/\//g, '~1')).join('/'));
      return wild(G.data, segs);
    }
    case 'un': { const a = evalNode(n.a, scope); return n.op === '!' ? !truthy(a) : -toNum(a); }
    case '?': return truthy(evalNode(n.c, scope)) ? evalNode(n.a, scope) : evalNode(n.b, scope);
    case 'call': return FN[n.f](...n.args.map(a => evalNode(a, scope)));
    case 'bin': {
      if (n.op === '&&') { const a = evalNode(n.a, scope); return truthy(a) ? evalNode(n.b, scope) : a; }
      if (n.op === '||') { const a = evalNode(n.a, scope); return truthy(a) ? a : evalNode(n.b, scope); }
      if (n.op === '??') { const a = evalNode(n.a, scope); return a ?? evalNode(n.b, scope); }
      const a = evalNode(n.a, scope), b = evalNode(n.b, scope);
      switch (n.op) {
        case '+':
          if (Array.isArray(a) && Array.isArray(b)) return a.concat(b);
          if (typeof a === 'string' || typeof b === 'string') return xtext(a) + xtext(b);
          return fin(toNum(a) + toNum(b));
        case '-': return fin(toNum(a) - toNum(b));
        case '*': return fin(toNum(a) * toNum(b));
        case '/': return fin(toNum(a) / toNum(b));
        case '%': return fin(toNum(a) % toNum(b));
        case '==': return same(a, b);
        case '!=': return !same(a, b);
        default: { const c = cmp(a, b); return n.op === '<' ? c < 0 : n.op === '<=' ? c <= 0 : n.op === '>' ? c > 0 : c >= 0; }
      }
    }
  }
}
const truthy = v => Array.isArray(v) ? v.length > 0 : v === 'false' ? false : !!v;
const fin = v => isFinite(v) ? v : null;
const xtext = v => v == null ? '' : typeof v === 'number' ? fmtNum(v) : typeof v === 'object' ? JSON.stringify(v) : String(v);
const numeric = v => typeof v === 'number' || (typeof v === 'string' && v.trim() !== '' && isFinite(+v));
function toNum(v) { if (typeof v === 'number') return v; if (typeof v === 'boolean') return +v; if (v == null || v === '') return 0; return num(v); }
function same(a, b) {
  if (a == null || b == null) return a == null && b == null;
  if (numeric(a) && numeric(b)) return +a === +b;
  if (typeof a === 'object' || typeof b === 'object') return JSON.stringify(a) === JSON.stringify(b);
  return String(a) === String(b);
}
function cmp(a, b) {
  if (numeric(a) && numeric(b)) return +a - +b;
  return String(a ?? '').localeCompare(String(b ?? ''), 'ru', {numeric: true});
}
const xlist = (args) => (args.length === 1 && Array.isArray(args[0]) ? args[0] : args.flat()).filter(x => x != null && x !== '');
const nums = args => xlist(args).map(toNum);
const FN = {
  sum: (...a) => nums(a).reduce((x, y) => x + y, 0),
  avg: (...a) => { const v = nums(a); return v.length ? v.reduce((x, y) => x + y, 0) / v.length : null; },
  min: (...a) => { const v = nums(a); return v.length ? Math.min(...v) : null; },
  max: (...a) => { const v = nums(a); return v.length ? Math.max(...v) : null; },
  len: x => Array.isArray(x) ? x.length : x && typeof x === 'object' ? Object.keys(x).length : xtext(x).length,
  count: (l, v) => (Array.isArray(l) ? l : []).filter(x => v === undefined ? truthy(x) : same(x, v)).length,
  round: (x, d = 0) => { const f = 10 ** Math.max(0, Math.min(10, toNum(d))); return Math.round(toNum(x) * f) / f; },
  abs: x => Math.abs(toNum(x)), floor: x => Math.floor(toNum(x)), ceil: x => Math.ceil(toNum(x)),
  fmt: (x, d = 2) => x == null || x === '' ? '—' : toNum(x).toLocaleString('ru', {maximumFractionDigits: Math.max(0, Math.min(10, toNum(d)))}),
  pct: (a, b) => toNum(b) ? Math.round(1000 * toNum(a) / toNum(b)) / 10 : 0,
  join: (l, sep = ', ') => (Array.isArray(l) ? l : [l]).map(xtext).join(xtext(sep)),
  where: (l, key, v) => (Array.isArray(l) ? l : []).filter(x => x && typeof x === 'object' && (v === undefined ? truthy(x[key]) : same(x[key], v))),
  pluck: (l, key) => (Array.isArray(l) ? l : []).map(x => x?.[key]),
  sort: (l, key) => (Array.isArray(l) ? l.slice() : []).sort((x, y) => key === undefined ? cmp(x, y) : cmp(x?.[key], y?.[key])),
  first: l => Array.isArray(l) ? l[0] : l, last: l => Array.isArray(l) ? l.at(-1) : l,
  uniq: l => Array.isArray(l) ? [...new Map(l.map(x => [JSON.stringify(x), x])).values()] : l,
  contains: (h, x) => Array.isArray(h) ? h.some(y => same(y, x)) : xtext(h).toLowerCase().includes(xtext(x).toLowerCase()),
  lower: x => xtext(x).toLowerCase(), upper: x => xtext(x).toUpperCase(),
  if: (c, a, b) => truthy(c) ? a : b,
  now: () => Date.now(),
  time: ms => ms == null ? '' : new Date(toNum(ms)).toLocaleTimeString('ru', {hour: '2-digit', minute: '2-digit', second: '2-digit'}),
  ago: ms => {
    if (ms == null) return '';
    const s = Math.max(0, Math.round((Date.now() - toNum(ms)) / 1000));
    return s < 60 ? `${s} с назад` : s < 3600 ? `${Math.round(s / 60)} мин назад` : s < 86400 ? `${Math.round(s / 3600)} ч назад` : fmtTime(toNum(ms));
  },
};

// ── State ────────────────────────────────────────────────────────────────────
const G = {
  surface: null,            // the surface shown, as the server sent it
  data: {},                 // live data model (with the person's unsaved edits)
  byId: new Map(),
  draft: false,
  records: [],              // rendered components and the data paths they read
  ui: new Map(),            // per-component view state (tab, sort, search, page)
  pending: [],              // [{path, value}] not saved yet
  used: new Set(), count: 0,
  frames: new Set(),        // Html sandboxes to keep in sync
  activity: false,
  step: '',
  editing: false,           // the person rearranges the interface
  parentOf: new Map(),      // component id → id of the component that lists it
  alertOn: new Set(),       // alerts whose condition is true now
};
const canvas = el('section'); canvas.id = 'canvas'; canvas.setAttribute('aria-label', 'Интерфейс задачи');
$('scroll').before(canvas);

const CHART_COLORS = 8;
const TONES = new Set(['ok', 'warn', 'danger', 'info', 'neutral']);
const tone = t => TONES.has(t) ? t : '';

// ── Bindings ─────────────────────────────────────────────────────────────────
let tracking = null; // the Set of paths the component being rendered reads
function abs(path, scope) { return path.startsWith('/') ? path : ptr.join(scope || '', path); }
/** A binding is an object with only a `path` (anything else with a `path` key is data). */
function pathOf(v, scope) { return v && typeof v === 'object' && !Array.isArray(v) && typeof v.path === 'string' && Object.keys(v).length === 1 ? abs(v.path, scope) : null; }
const isExpr = v => v && typeof v === 'object' && !Array.isArray(v) && typeof v.expr === 'string' && Object.keys(v).length === 1;
function val(v, scope) {
  if (isExpr(v)) return expr(v.expr, scope);
  const p = pathOf(v, scope);
  if (p == null) return v;
  tracking?.add(p);
  return ptr.get(G.data, p);
}
/** Resolve bindings inside an action context. */
function resolve(v, scope) {
  if (pathOf(v, scope) != null || isExpr(v)) return val(v, scope);
  if (Array.isArray(v)) return v.map(x => resolve(x, scope));
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, resolve(x, scope)]));
  return v;
}
const str = (v, scope) => { const x = val(v, scope); return x == null ? '' : typeof x === 'object' ? JSON.stringify(x) : String(x); };
const arr = (v, scope) => { const x = val(v, scope); return Array.isArray(x) ? x : x == null || x === '' ? [] : [x]; };

/** The person changed a value: update the model, re-render what reads it, save soon. */
function setData(path, value, src) {
  if (JSON.stringify(ptr.get(G.data, path)) === JSON.stringify(value)) return; // nothing changed: no version
  G.pending = G.pending.filter(p => !(p.path + '/').startsWith(path + '/'));
  G.pending.push({path, value: value === undefined ? null : value});
  saveSoon();
  applyData(path, value, src);
}
/** A new value in the model (from the person or a live source): re-render what reads it. */
function applyData(path, value, src) {
  G.data = ptr.set(G.data, path, value) ?? {};
  refresh(path, src);
  for (const f of G.frames) f.contentWindow?.postMessage({kron: 'data', data: G.data}, '*');
  checkAlerts();
}
const saveSoon = debounce(() => saveData(), 500);
async function saveData() {
  if (!G.surface?.id || !G.pending.length) return;
  const patches = G.pending.splice(0);
  try {
    const r = await postJSON(`/api/surfaces/${G.surface.id}/data`, {patches});
    G.surface.version = r.version;
  } catch (e) {
    G.pending.unshift(...patches);
    toast('Изменения не сохранены: ' + e.message, {err: true, action: 'Повторить', onAction: saveData});
  }
}
function use(kind) { G.used.add(kind); G.count++; flushSignalsSoon(); }
const flushSignalsSoon = debounce(flushSignals, 6000);
function flushSignals() {
  if (!G.surface?.id || !G.count) return;
  const body = {kind: 'interact', used: [...G.used], count: G.count};
  G.used.clear(); G.count = 0;
  postJSON(`/api/surfaces/${G.surface.id}/signal`, body).catch(() => {});
}

// ── Rendering ────────────────────────────────────────────────────────────────
function renderComp(id, scope = '') {
  const c = G.byId.get(id);
  if (!c) return el('div');
  const rec = {id, scope, deps: new Set(), el: null};
  const outer = tracking; tracking = rec.deps;
  let node;
  try {
    if (c.visible !== undefined && !truthy(val(c.visible, scope))) { node = el('div'); node.hidden = true; }
    else { const f = COMPONENTS[c.component]; node = f ? f(c, scope, rec) : unknown(c); }
  } catch (e) {
    console.error('render', id, e);
    node = el('div', 'g-broken', icon('circle-alert', 'sm') + `<span>Не удалось показать «${esc(c.component)}»: ${esc(e.message)}</span>`);
  } finally { tracking = outer; }
  node.dataset.gid = id;
  if (G.editing && !scope && G.parentOf.has(id)) node = editFrame(c, node);
  rec.el = node;
  G.records.push(rec);
  return node;
}
function unknown(c) {
  const text = c.text || c.label || c.title || '';
  return el('div', 'g-broken', icon('circle-alert', 'sm') + `<span>${esc(c.component)}${text ? ': ' + esc(str(text)) : ''}</span>`);
}
function kids(children, scope) {
  if (Array.isArray(children)) return children.map(id => renderComp(id, scope));
  if (children && typeof children.path === 'string' && children.componentId) {
    const p = abs(children.path, scope);
    return arr({path: p}, scope).map((_, i) => renderComp(children.componentId, `${p}/${i}`));
  }
  return [];
}
function refresh(path, src) {
  G.records = G.records.filter(r => r.el.isConnected);
  const hit = G.records.filter(r => r !== src && [...r.deps].some(d => ptr.overlaps(d, path)));
  const done = [];
  for (const r of hit) {
    if (done.some(d => d.contains(r.el)) || !r.el.isConnected) continue;
    if (src && r.el.contains(src.el)) continue; // keep the field being typed into
    const fresh = renderComp(r.id, r.scope);
    r.el.replaceWith(fresh); done.push(fresh);
  }
}
function uiState(c, scope, init) {
  const k = c.id + '@' + scope;
  if (!G.ui.has(k)) G.ui.set(k, init);
  return G.ui.get(k);
}

/** Show a surface. `draft`: a remembered layout while the agent builds the real one. */
function showSurface(s, {draft = false} = {}) {
  const same = G.surface?.id && G.surface.id === s.id;
  G.surface = s; G.draft = draft;
  G.data = structuredClone(s.data || {});
  for (const p of G.pending) ptr.set(G.data, p.path, p.value ?? undefined);
  G.byId = new Map((s.components || []).map(c => [c.id, c]));
  G.parentOf = new Map();
  for (const c of s.components || []) {
    for (const list of [c.children, ...(Array.isArray(c.tabs) ? c.tabs.map(t => t.children) : [])]) {
      if (Array.isArray(list)) for (const k of list) if (typeof k === 'string') G.parentOf.set(k, c.id);
    }
  }
  if (!same) { G.alertOn.clear(); G.alertsSeen = false; G.editing = false; }
  draw();
  if (!draft) setTitle(s.title);
  genBar();
  startLive(same);
}
/** Draw the current surface from the live model (keeps live values and the person's edits). */
function draw() {
  const s = G.surface;
  G.records = []; G.frames.clear();
  document.body.classList.add('surface');
  document.body.classList.toggle('g-editing', G.editing);
  const y = canvas.scrollTop;
  canvas.innerHTML = '';
  canvas.classList.toggle('draft', G.draft);
  if (G.draft) canvas.append(el('div', 'g-draftnote', icon('history', 'sm') + '<span>Ваш привычный вид для таких задач. Обновляю данные…</span>'));
  canvas.append(liveBox, alertBox);
  if (G.editing) {
    const note = el('div', 'g-editnote', icon('pencil', 'sm') + '<span>Двигайте и скрывайте блоки или нажмите «Изменить», чтобы сказать агенту, что сделать с блоком.</span>');
    note.append(btn('btn sm primary', 'Готово', () => setEditing(false)));
    canvas.append(note);
  }
  const root = el('div', 'g-root');
  try {
    if (!G.byId.has(s.root)) throw new Error('нет корневого компонента');
    root.append(renderComp(s.root));
  } catch (e) { return fallbackToText(e); }
  canvas.append(root);
  checkAlerts();
  canvas.scrollTop = y;
}
function fallbackToText(e) {
  canvas.innerHTML = '';
  const box = el('div', 'g-fallback', `${icon('circle-alert')}<div><b>Интерфейс не получилось показать</b><p>${esc(e?.message || e)}</p></div>`);
  box.append(btn('btn sm', icon('message-square', 'sm') + 'Показать текстом', () => setActivity(true)));
  if (G.surface?.id) box.append(btn('btn sm', icon('history', 'sm') + 'Предыдущая версия', () => undo()));
  canvas.append(box);
  setActivity(true);
}
function closeSurface() {
  flushSignals(); saveData(); stopLive();
  G.surface = null; G.draft = false; G.records = []; G.frames.clear(); G.ui.clear(); G.editing = false;
  G.alertOn.clear();
  document.body.classList.remove('g-editing');
  canvas.innerHTML = '';
  document.body.classList.remove('surface');
  setActivity(false);
  genBar();
}

// ── Components ───────────────────────────────────────────────────────────────
const gap = g => ({s: 'g-gap-s', l: 'g-gap-l'})[g] || '';
const COMPONENTS = {
  Column(c, s) { const d = el('div', 'g-col ' + gap(c.gap)); d.append(...kids(c.children, s)); return d; },
  Row(c, s) { const d = el('div', 'g-row ' + gap(c.gap) + (c.align ? ' g-al-' + c.align : '')); d.append(...kids(c.children, s)); return d; },
  Grid(c, s) {
    const d = el('div', 'g-grid'); d.style.setProperty('--min', Math.max(140, Math.min(600, +c.min || 240)) + 'px');
    d.append(...kids(c.children, s)); return d;
  },
  Card(c, s) {
    const d = el('section', 'g-card');
    const title = str(c.title, s), sub = str(c.subtitle, s);
    if (title || sub) d.append(el('header', '', (title ? `<h3>${esc(title)}</h3>` : '') + (sub ? `<p>${esc(sub)}</p>` : '')));
    const b = el('div', 'g-col'); b.append(...kids(c.children, s)); d.append(b);
    return d;
  },
  Tabs(c, s) {
    const tabs = Array.isArray(c.tabs) ? c.tabs : [];
    const st = uiState(c, s, {i: 0});
    const d = el('div', 'g-tabs'), bar = el('div', 'g-tabbar'), panel = el('div', 'g-col');
    bar.setAttribute('role', 'tablist');
    const show = i => {
      st.i = i; panel.innerHTML = '';
      [...bar.children].forEach((b, j) => b.setAttribute('aria-selected', j === i));
      panel.append(...kids(tabs[i]?.children || [], s));
    };
    tabs.forEach((t, i) => {
      const b = btn('', esc(str(t.label, s) || 'Вкладка ' + (i + 1)), () => { use('Tabs'); show(i); });
      b.setAttribute('role', 'tab'); bar.append(b);
    });
    d.append(bar, panel); show(Math.min(st.i, Math.max(0, tabs.length - 1)));
    return d;
  },
  Collapsible(c, s) {
    const st = uiState(c, s, {open: !!c.open});
    const d = el('details', 'g-coll'); d.open = st.open;
    d.innerHTML = `<summary>${icon('chevron-right', 'xs chev')}<span>${esc(str(c.title, s))}</span></summary>`;
    const b = el('div', 'g-col'); b.append(...kids(c.children, s)); d.append(b);
    d.ontoggle = () => { st.open = d.open; use('Collapsible'); };
    return d;
  },
  Divider() { return el('hr', 'g-hr'); },
  Heading(c, s) { const lv = Math.min(3, Math.max(1, +c.level || 2)); const h = el('h' + (lv + 1), 'g-h g-h' + lv); h.textContent = str(c.text, s); return h; },
  Text(c, s) { const d = el('div', 'md g-text' + (c.tone === 'muted' ? ' muted' : '')); d.innerHTML = md(str(c.text, s)); highlight(d); return d; },
  Badge(c, s) { return el('span', 'badge g-badge ' + tone(val(c.tone, s)), esc(str(c.text, s))); },
  Metric(c, s) {
    const d = el('div', 'g-metric ' + tone(val(c.tone, s)));
    const delta = str(c.delta, s), dt = /^[-−]/.test(delta) ? 'down' : delta ? 'up' : '';
    d.innerHTML = `<div class="l">${esc(str(c.label, s))}</div><div class="v">${esc(fmtNum(val(c.value, s)))}${c.unit ? `<small>${esc(c.unit)}</small>` : ''}</div>` +
      (delta ? `<div class="d ${dt}">${esc(delta)}</div>` : '') + (c.hint ? `<div class="h">${esc(str(c.hint, s))}</div>` : '');
    return d;
  },
  Progress(c, s) {
    const v = Math.max(0, Math.min(100, +val(c.value, s) || 0));
    const d = el('div', 'g-progress ' + tone(val(c.tone, s)));
    d.innerHTML = `<div class="top"><span>${esc(str(c.label, s))}</span><b>${Math.round(v)}%</b></div><div class="bar" role="progressbar" aria-valuenow="${v}" aria-valuemin="0" aria-valuemax="100"><i style="width:${v}%"></i></div>`;
    return d;
  },
  KeyValue(c, s) {
    const d = el('dl', 'g-kv');
    for (const it of arr(c.items, s)) d.append(el('dt', '', esc(str(it.key ?? it.label))), el('dd', '', esc(str(it.value))));
    return d;
  },
  Code(c, s) { return codeBlock(str(c.code, s), c.lang); },
  Diff(c, s) {
    const patch = str(c.patch, s);
    const files = patch ? unifiedFiles(patch) : [lineDiff(str(c.before, s), str(c.after, s), c.path || '')];
    const d = el('div', 'g-col g-gap-s'); files.forEach(f => d.append(diffView(f))); return d;
  },
  Log(c, s) {
    const lines = val(c.lines, s);
    const d = el('div', 'g-logbox');
    if (c.title) d.append(el('div', 'g-sub', esc(c.title)));
    const p = pre(Array.isArray(lines) ? lines.join('\n') : String(lines ?? ''), 'g-log');
    p.style.maxHeight = (+c.height || 320) + 'px';
    d.append(p); requestAnimationFrame(() => { p.scrollTop = p.scrollHeight; });
    return d;
  },
  Image(c, s) {
    const src = str(c.src, s);
    if (!/^(data:image\/|https:\/\/)/.test(src)) return el('div', 'g-broken', 'Изображение недоступно');
    const i = el('img', 'g-img'); i.src = src; i.alt = str(c.alt, s); i.loading = 'lazy'; i.referrerPolicy = 'no-referrer';
    return i;
  },
  Table: table,
  Chart: chart,
  List: list,
  Tree: tree,
  TextField(c, s, rec) {
    const p = pathOf(c.value, s);
    const f = field(c, s);
    const inp = c.multiline ? el('textarea', 'textarea') : el('input', 'input');
    if (!c.multiline) inp.type = ['number', 'password', 'email', 'date', 'url'].includes(c.type) ? c.type : 'text';
    inp.value = str(c.value, s); inp.placeholder = str(c.placeholder, s);
    if (c.multiline) inp.rows = 4;
    inp.disabled = !p;
    inp.oninput = () => { setData(p, c.type === 'number' && inp.value !== '' ? +inp.value : inp.value, rec); };
    inp.onchange = () => use('TextField');
    f.append(inp); return f;
  },
  Select(c, s, rec) {
    const p = pathOf(c.value, s), f = field(c, s), sel = el('select', 'select');
    const cur = str(c.value, s);
    for (const o of options(c.options, s)) { const op = el('option'); op.value = o.value; op.textContent = o.label; op.selected = o.value === cur; sel.append(op); }
    sel.disabled = !p;
    sel.onchange = () => { use('Select'); setData(p, sel.value, rec); };
    f.append(sel); return f;
  },
  ChoicePicker(c, s, rec) {
    const p = pathOf(c.value, s), f = field(c, s), box = el('div', 'g-choices');
    const multi = !!c.multiple, cur = multi ? arr(c.value, s).map(String) : [str(c.value, s)];
    box.setAttribute('role', multi ? 'group' : 'radiogroup');
    for (const o of options(c.options, s)) {
      const on = cur.includes(o.value);
      const b = btn('chip' + (on ? ' on' : ''), (on ? icon('check', 'xs') : '') + `<span class="t">${esc(o.label)}</span>`, () => {
        use('ChoicePicker');
        if (!p) return;
        if (multi) setData(p, on ? cur.filter(x => x !== o.value) : [...cur, o.value]);
        else setData(p, on && c.clearable ? '' : o.value);
      });
      b.setAttribute('aria-pressed', on); box.append(b);
    }
    f.append(box); return f;
  },
  CheckBox(c, s, rec) {
    const p = pathOf(c.value, s), on = !!val(c.value, s);
    const b = btn('g-check', `<span class="switch" aria-hidden="true"></span><span>${esc(str(c.label, s))}</span>`, () => { use('CheckBox'); if (p) setData(p, !on); });
    b.setAttribute('role', 'switch'); b.setAttribute('aria-checked', on); b.querySelector('.switch').setAttribute('aria-checked', on);
    return b;
  },
  Slider(c, s, rec) {
    const p = pathOf(c.value, s), f = field(c, s), row = el('div', 'g-slider');
    const inp = el('input'); inp.type = 'range';
    inp.min = c.min ?? 0; inp.max = c.max ?? 100; inp.step = c.step ?? 1; inp.value = +val(c.value, s) || 0;
    const out = el('output', '', esc(inp.value + (c.unit || '')));
    inp.oninput = () => { out.textContent = inp.value + (c.unit || ''); if (p) setData(p, +inp.value, rec); };
    inp.onchange = () => use('Slider');
    row.append(inp, out); f.append(row); return f;
  },
  Editor(c, s, rec) {
    const p = pathOf(c.value, s), f = field(c, s);
    const ta = el('textarea', 'g-editor'); ta.spellcheck = false; ta.setAttribute('autocapitalize', 'off');
    ta.value = str(c.value, s); ta.style.height = (+c.height || 280) + 'px';
    ta.onkeydown = e => {
      if (e.key === 'Tab' && !e.shiftKey) { e.preventDefault(); ta.setRangeText('  ', ta.selectionStart, ta.selectionEnd, 'end'); ta.oninput(); }
    };
    ta.oninput = () => { if (p) setData(p, ta.value, rec); };
    ta.onchange = () => use('Editor');
    f.append(ta); return f;
  },
  Button(c, s) {
    const v = ['primary', 'danger', 'ghost'].includes(c.variant) ? c.variant : '';
    const ic = BTN_ICONS[c.icon];
    const b = btn('btn ' + v, (ic ? icon(ic, 'sm') : '') + esc(str(c.label, s)), () => { use('Button'); runAction(c.action, s, {}, c, b); });
    if (c.disabled && val(c.disabled, s)) b.disabled = true;
    return b;
  },
  Terminal: terminal,
  Html: htmlFrame,
};
const BTN_ICONS = {play: 'zap', refresh: 'refresh-cw', trash: 'trash-2', save: 'check', plus: 'plus', check: 'check', download: 'download', terminal: 'terminal', search: 'search', edit: 'pencil', open: 'external-link', copy: 'copy'};

function field(c, s) {
  const f = el('label', 'g-field');
  const l = str(c.label, s);
  if (l) f.append(el('span', 'g-label', esc(l)));
  return f;
}
function options(o, s) {
  return arr(o, s).map(x => typeof x === 'object' && x ? {value: String(x.value ?? x.label), label: String(x.label ?? x.value)} : {value: String(x), label: String(x)});
}
function fmtNum(v) {
  if (typeof v === 'number' && isFinite(v)) return Math.abs(v) >= 1000 ? v.toLocaleString('ru') : String(Math.round(v * 100) / 100);
  return v == null ? '—' : String(v);
}
function codeBlock(code, lang) {
  const l = String(lang || '').toLowerCase();
  const d = el('div', 'code');
  d.innerHTML = `<div class="code-head"><span class="lang">${esc(l || 'текст')}</span><button data-act="copy" data-label="1">${icon('copy', 'xs')}<span>Копировать</span></button></div><pre><code data-lang="${esc(l)}"></code></pre>`;
  d.querySelector('code').textContent = code;
  highlight(d);
  return d;
}
function unifiedFiles(patch) {
  if (/\*\*\* Begin Patch/.test(patch)) return patchFiles(patch);
  const files = []; let cur = null;
  for (const line of patch.split('\n')) {
    let m;
    if ((m = line.match(/^\+\+\+ (?:b\/)?(.+)/))) { if (cur) cur.path = m[1]; continue; }
    if ((m = line.match(/^--- (?:a\/)?(.+)/)) || /^diff /.test(line)) { if (!cur || cur.lines.length) { cur = {op: 'Update', path: m ? m[1] : '', lines: [], add: 0, del: 0}; files.push(cur); } continue; }
    if (!cur) { cur = {op: 'Update', path: '', lines: [], add: 0, del: 0}; files.push(cur); }
    if (line.startsWith('@@')) cur.lines.push(['h', line]);
    else if (line.startsWith('+')) { cur.lines.push(['a', line.slice(1)]); cur.add++; }
    else if (line.startsWith('-')) { cur.lines.push(['d', line.slice(1)]); cur.del++; }
    else cur.lines.push(['c', line.slice(1)]);
  }
  return files;
}
/** Line diff of two texts (LCS; long texts fall back to a plain replace). */
function lineDiff(a, b, path) {
  const x = a.split('\n'), y = b.split('\n'), f = {op: 'Update', path, lines: [], add: 0, del: 0};
  if (x.length * y.length > 4e6) { x.forEach(l => f.lines.push(['d', l])); y.forEach(l => f.lines.push(['a', l])); f.del = x.length; f.add = y.length; return f; }
  const n = x.length, m = y.length, L = Array.from({length: n + 1}, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) L[i][j] = x[i] === y[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1]);
  let i = 0, j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && x[i] === y[j]) { f.lines.push(['c', x[i]]); i++; j++; }
    else if (j < m && (i >= n || L[i][j + 1] >= L[i + 1][j])) { f.lines.push(['a', y[j++]]); f.add++; }
    else { f.lines.push(['d', x[i++]]); f.del++; }
  }
  return f;
}

// ── Table ────────────────────────────────────────────────────────────────────
function rowFilter(c, s) {
  if (!c.filter?.key || !c.filter.path) return () => true;
  const want = val({path: c.filter.path}, s);
  const set = (Array.isArray(want) ? want : want == null || want === '' ? [] : [want]).map(String);
  return set.length ? r => set.includes(String(r[c.filter.key])) : () => true;
}
function table(c, s, rec) {
  const p = pathOf(c.rows, s), rows = arr(c.rows, s);
  const cols = (Array.isArray(c.columns) && c.columns.length ? c.columns : Object.keys(rows[0] || {}).map(k => ({key: k, label: k})))
    .map(x => typeof x === 'string' ? {key: x, label: x} : x);
  const st = uiState(c, s, {sort: null, dir: 1, q: '', limit: +c.pageSize || 50});
  const key = c.rowKey || (rows[0] && 'id' in rows[0] ? 'id' : null);
  const selPath = typeof c.selectable === 'string' ? c.selectable : null;
  const selected = selPath ? arr({path: selPath}, s).map(String) : [];
  const d = el('div', 'g-table');
  const head = el('div', 'g-tablehead');
  if (c.search) {
    const q = el('input', 'input'); q.type = 'search'; q.placeholder = 'Поиск'; q.value = st.q; q.setAttribute('aria-label', 'Поиск в таблице');
    q.oninput = debounce(() => { st.q = q.value; use('Table'); redraw(); }, 150);
    head.append(q);
  }
  const count = el('span', 'g-sub'); head.append(count);
  d.append(head);
  const wrap = el('div', 'tbl'), t = el('table');
  wrap.append(t); d.append(wrap);
  const more = btn('btn sm ghost', 'Показать ещё', () => { st.limit += 100; redraw(); });
  d.append(more);
  const keep = rowFilter(c, s);
  function redraw() {
    let list = rows.map((r, i) => ({r, i})).filter(({r}) => keep(r));
    if (st.q) { const q = st.q.toLowerCase(); list = list.filter(({r}) => cols.some(col => String(r[col.key] ?? '').toLowerCase().includes(q))); }
    if (st.sort) {
      const k = st.sort;
      list.sort((a, b) => {
        const x = a.r[k], y = b.r[k];
        const nx = parseFloat(x), ny = parseFloat(y);
        return st.dir * (!isNaN(nx) && !isNaN(ny) ? nx - ny : String(x ?? '').localeCompare(String(y ?? ''), 'ru', {numeric: true}));
      });
    }
    count.textContent = list.length === rows.length ? `${rows.length} ${plural(rows.length, 'строка', 'строки', 'строк')}` : `${list.length} из ${rows.length}`;
    t.innerHTML = '';
    const tr = el('tr');
    if (selPath) tr.append(el('th', 'g-selcol'));
    for (const col of cols) {
      const th = el('th'); const on = st.sort === col.key;
      const b = btn('g-sort', esc(col.label ?? col.key) + (on ? icon(st.dir > 0 ? 'arrow-up' : 'arrow-down', 'xs') : ''), () => {
        use('Table'); if (st.sort === col.key) st.dir = -st.dir; else { st.sort = col.key; st.dir = 1; } redraw();
      });
      th.setAttribute('aria-sort', on ? (st.dir > 0 ? 'ascending' : 'descending') : 'none');
      if (col.type === 'number' || col.type === 'progress') th.className = 'num';
      th.append(b); tr.append(th);
    }
    const thead = el('thead'); thead.append(tr); t.append(thead);
    const tb = el('tbody');
    for (const {r, i} of list.slice(0, st.limit)) {
      const row = el('tr');
      const rk = String(key ? r[key] : i);
      if (selPath) {
        const td = el('td', 'g-selcol'), cb = el('input'); cb.type = 'checkbox'; cb.checked = selected.includes(rk); cb.setAttribute('aria-label', 'Выбрать строку');
        cb.onclick = e => e.stopPropagation();
        cb.onchange = () => { use('Table'); setData(abs(selPath, s), cb.checked ? [...selected, rk] : selected.filter(x => x !== rk)); };
        td.append(cb); row.append(td);
      }
      for (const col of cols) {
        const td = el('td', col.type === 'number' || col.type === 'progress' ? 'num' : '');
        td.append(cell(col, r[col.key]));
        if (col.editable && p) {
          td.classList.add('edit'); td.title = 'Нажмите, чтобы изменить';
          td.onclick = e => { e.stopPropagation(); editCell(td, col, r[col.key], v => { use('Table'); setData(`${p}/${i}/${col.key}`, v); }); };
        }
        row.append(td);
      }
      if (c.rowAction) { row.classList.add('act'); row.tabIndex = 0; row.onclick = () => { use('Table'); runAction(c.rowAction, s, {row: r, index: i}, c); }; row.onkeydown = e => { if (e.key === 'Enter') row.onclick(); }; }
      tb.append(row);
    }
    t.append(tb);
    more.hidden = list.length <= st.limit;
    if (!list.length) d.querySelector('.g-empty')?.remove(), wrap.after(el('div', 'g-empty', st.q ? 'Ничего не найдено' : 'Нет данных'));
    else d.querySelector('.g-empty')?.remove();
  }
  redraw();
  return d;
}
function cell(col, v) {
  switch (col.type) {
    case 'badge': return el('span', 'badge ' + badgeTone(v), esc(v ?? ''));
    case 'progress': { const n = Math.max(0, Math.min(100, +v || 0)); return el('span', 'g-cellbar', `<i style="width:${n}%"></i><b>${Math.round(n)}%</b>`); }
    case 'number': return document.createTextNode(fmtNum(v));
    case 'link': { const u = String(v ?? ''); if (!/^https?:\/\//.test(u)) return document.createTextNode(u); const a = el('a', '', esc(u.replace(/^https?:\/\//, '').slice(0, 60))); a.href = u; a.target = '_blank'; a.rel = 'noopener noreferrer'; a.onclick = e => e.stopPropagation(); return a; }
    case 'code': return el('code', 'g-c', esc(v ?? ''));
    default: return document.createTextNode(v == null ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v));
  }
}
function badgeTone(v) {
  const t = String(v ?? '').toLowerCase();
  if (/^(ok|ок|да|yes|true|pass|passed|done|готово|active|running|работает|up|online|success|успех|healthy)/.test(t)) return 'ok';
  if (/(fail|error|ошибк|down|offline|stopped|остановлен|critical|нет$|^no$|false)/.test(t)) return 'err';
  if (/(warn|pending|ожида|wait|degraded|slow|medium|частично)/.test(t)) return 'warn';
  return '';
}
function editCell(td, col, v, save) {
  if (td.querySelector('input')) return;
  const inp = el('input', 'input sm'); inp.value = v ?? ''; inp.type = col.type === 'number' ? 'number' : 'text';
  td.innerHTML = ''; td.append(inp); inp.focus(); inp.select();
  let done = false;
  const finish = ok => { if (done) return; done = true; if (ok && inp.value !== String(v ?? '')) save(col.type === 'number' && inp.value !== '' ? +inp.value : inp.value); else { td.innerHTML = ''; td.append(cell(col, v)); } };
  inp.onkeydown = e => { if (e.key === 'Enter') finish(true); if (e.key === 'Escape') { e.stopPropagation(); finish(false); } };
  inp.onblur = () => finish(true);
}

// ── Chart (inline SVG) ───────────────────────────────────────────────────────
function chart(c, s) {
  const rows = arr(c.rows ?? c.data, s).filter(rowFilter(c, s));
  const x = c.x || Object.keys(rows[0] || {}).find(k => typeof rows[0][k] === 'string') || 'label';
  const series = (Array.isArray(c.series) && c.series.length ? c.series : Object.keys(rows[0] || {}).filter(k => k !== x && typeof rows[0][k] === 'number').map(k => ({key: k})))
    .map(z => typeof z === 'string' ? {key: z} : z).slice(0, CHART_COLORS);
  const kind = ['bar', 'hbar', 'line', 'area', 'donut'].includes(c.kind) ? c.kind : 'bar';
  const st = uiState(c, s, {off: new Set()});
  const d = el('figure', 'g-chart');
  if (c.title) d.append(el('figcaption', '', esc(str(c.title, s))));
  if (!rows.length || !series.length) { d.append(el('div', 'g-empty', 'Нет данных для графика')); return d; }
  const tip = el('div', 'g-tip'); tip.hidden = true;
  const box = el('div', 'g-plot');
  d.append(box, tip);
  const unit = c.unit || '';
  const fmt = v => fmtNum(v) + (unit ? ' ' + unit : '');
  const showTip = (e, html) => {
    tip.innerHTML = html; tip.hidden = false;
    const r = d.getBoundingClientRect(), tx = (e.clientX ?? r.left + r.width / 2) - r.left, ty = (e.clientY ?? r.top) - r.top;
    tip.style.left = Math.min(Math.max(4, tx + 12), r.width - tip.offsetWidth - 4) + 'px'; tip.style.top = Math.max(4, ty - tip.offsetHeight - 10) + 'px';
  };
  const hideTip = () => { tip.hidden = true; };
  const draw = () => {
    const on = series.map((z, i) => ({...z, i})).filter(z => !st.off.has(z.key));
    box.innerHTML = kind === 'donut' ? donut(rows, x, series[0], fmt) : kind === 'hbar' ? hbars(rows, x, on, fmt) : xy(rows, x, on, kind, +c.height || 240, fmt, box.clientWidth || 640);
    box.querySelectorAll('[data-tip]').forEach(m => {
      const h = e => showTip(e, m.dataset.tip);
      m.addEventListener('pointerenter', h); m.addEventListener('pointermove', h); m.addEventListener('pointerleave', hideTip);
      m.addEventListener('click', e => { use('Chart'); h(e); });
    });
  };
  if (series.length > 1 && kind !== 'donut') {
    const lg = el('div', 'g-legend');
    series.forEach((z, i) => {
      const b = btn('g-lg' + (st.off.has(z.key) ? ' off' : ''), `<i class="sw c${i + 1}"></i>${esc(z.label || z.key)}`, () => {
        use('Chart');
        if (st.off.has(z.key)) st.off.delete(z.key); else if (st.off.size < series.length - 1) st.off.add(z.key);
        b.classList.toggle('off', st.off.has(z.key)); b.setAttribute('aria-pressed', !st.off.has(z.key)); draw();
      }, 'Показать или скрыть: ' + (z.label || z.key));
      b.setAttribute('aria-pressed', !st.off.has(z.key));
      lg.append(b);
    });
    d.prepend(lg);
  }
  draw();
  // Draw at the real width, so labels stay readable on a phone.
  let lastW = 0;
  if (kind !== 'donut' && kind !== 'hbar') new ResizeObserver(() => { const w = box.clientWidth; if (w && Math.abs(w - lastW) > 8) { lastW = w; draw(); } }).observe(box);
  return d;
}
const niceMax = v => { if (v <= 0) return 1; const e = 10 ** Math.floor(Math.log10(v)), f = v / e; return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * e; };
const num = v => { const n = typeof v === 'number' ? v : parseFloat(String(v ?? '').replace(',', '.').replace(/[^\d.\-]/g, '')); return isFinite(n) ? n : 0; };
function xy(rows, x, series, kind, H, fmt, width) {
  const W = Math.max(260, width), padL = 44, padR = 12, padT = 12, padB = 34;
  const vals = rows.flatMap(r => series.map(z => num(r[z.key])));
  const lo = Math.min(0, ...vals), hi = niceMax(Math.max(...vals, 0));
  const yv = v => padT + (H - padT - padB) * (1 - (v - lo) / (hi - lo || 1));
  const iw = (W - padL - padR) / rows.length;
  let out = `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="График" preserveAspectRatio="none" class="g-svg">`;
  for (let k = 0; k <= 4; k++) { const v = lo + (hi - lo) * k / 4, y = yv(v); out += `<line class="grid" x1="${padL}" x2="${W - padR}" y1="${y}" y2="${y}"/><text class="ax" x="${padL - 6}" y="${y + 4}" text-anchor="end">${esc(fmtShort(v))}</text>`; }
  const chars = Math.max(4, Math.floor(iw / 7)), step = Math.ceil(rows.length / Math.max(2, Math.floor((W - padL) / 64)));
  rows.forEach((r, i) => {
    const label = String(r[x] ?? ''), short = label.length > chars * step ? label.slice(0, chars * step - 1) + '…' : label;
    if (i % step === 0) out += `<text class="ax" x="${padL + iw * (i + .5)}" y="${H - 12}" text-anchor="middle"><title>${esc(label)}</title>${esc(short)}</text>`;
  });
  if (kind === 'bar') {
    const gw = iw * .72, bw = Math.max(2, gw / series.length - 2);
    rows.forEach((r, i) => series.forEach((z, j) => {
      const v = num(r[z.key]), y0 = yv(Math.max(0, lo)), y = yv(v), bx = padL + iw * i + (iw - gw) / 2 + j * (bw + 2);
      const h = Math.abs(y0 - y);
      out += `<path class="mk c${z.i + 1}" d="${roundBar(bx, Math.min(y, y0), bw, h, v >= 0)}" data-tip="${esc(`<b>${esc(r[x])}</b><br>${esc(z.label || z.key)}: ${esc(fmt(r[z.key]))}`)}"/>`;
    }));
  } else {
    series.forEach(z => {
      const pts = rows.map((r, i) => [padL + iw * (i + .5), yv(num(r[z.key]))]);
      const line = pts.map((p, i) => (i ? 'L' : 'M') + p[0].toFixed(1) + ' ' + p[1].toFixed(1)).join(' ');
      if (kind === 'area') out += `<path class="area c${z.i + 1}" d="${line} L${pts.at(-1)[0]} ${yv(Math.max(0, lo))} L${pts[0][0]} ${yv(Math.max(0, lo))} Z"/>`;
      out += `<path class="ln c${z.i + 1}" d="${line}"/>`;
      if (pts.length <= 40) pts.forEach(p => { out += `<circle class="dot c${z.i + 1}" cx="${p[0]}" cy="${p[1]}" r="4"/>`; });
    });
    // One hit column per x: all series in the tooltip (crosshair-like).
    rows.forEach((r, i) => {
      const tipHtml = `<b>${esc(r[x])}</b>` + series.map(z => `<br><i class="sw c${z.i + 1}"></i>${esc(z.label || z.key)}: ${esc(fmt(r[z.key]))}`).join('');
      out += `<rect class="hit" x="${padL + iw * i}" y="${padT}" width="${iw}" height="${H - padT - padB}" data-tip="${esc(tipHtml)}"/>`;
    });
  }
  return out + '</svg>';
}
function roundBar(x, y, w, h, up) {
  const r = Math.min(4, w / 2, h);
  if (h <= 0) return `M${x} ${y}h${w}`;
  return up
    ? `M${x} ${y + h}V${y + r}Q${x} ${y} ${x + r} ${y}H${x + w - r}Q${x + w} ${y} ${x + w} ${y + r}V${y + h}Z`
    : `M${x} ${y}V${y + h - r}Q${x} ${y + h} ${x + r} ${y + h}H${x + w - r}Q${x + w} ${y + h} ${x + w} ${y + h - r}V${y}Z`;
}
function hbars(rows, x, series, fmt) {
  const max = niceMax(Math.max(0, ...rows.flatMap(r => series.map(z => num(r[z.key])))));
  return '<div class="g-hbars">' + rows.map(r => `<div class="hb"><div class="hl">${esc(r[x])}</div><div class="hv">` +
    series.map(z => { const v = num(r[z.key]); return `<div class="hr" data-tip="${esc(`<b>${esc(r[x])}</b><br>${esc(z.label || z.key)}: ${esc(fmt(r[z.key]))}`)}"><i class="c${z.i + 1}" style="width:${Math.max(0.5, 100 * v / max)}%"></i><span>${esc(fmt(r[z.key]))}</span></div>`; }).join('') +
    '</div></div>').join('') + '</div>';
}
function donut(rows, x, z, fmt) {
  const vals = rows.map(r => Math.max(0, num(r[z.key])));
  const total = vals.reduce((a, b) => a + b, 0) || 1;
  const top = rows.slice(0, CHART_COLORS - 1), rest = vals.slice(CHART_COLORS - 1).reduce((a, b) => a + b, 0);
  const items = top.map((r, i) => [String(r[x] ?? ''), vals[i], r[z.key]]);
  if (rest > 0) items.push(['Другое', rest, rest]);
  let a = -Math.PI / 2, out = '<div class="g-donut"><svg viewBox="0 0 120 120" role="img" aria-label="Круговая диаграмма">';
  items.forEach(([label, v, raw], i) => {
    const sweep = 2 * Math.PI * v / total, a2 = a + Math.max(0, sweep - .02);
    const p = (ang, rad) => [60 + rad * Math.cos(ang), 60 + rad * Math.sin(ang)];
    const [x1, y1] = p(a, 54), [x2, y2] = p(a2, 54), [x3, y3] = p(a2, 34), [x4, y4] = p(a, 34), big = a2 - a > Math.PI ? 1 : 0;
    if (v > 0) out += `<path class="mk c${i + 1}" d="M${x1} ${y1}A54 54 0 ${big} 1 ${x2} ${y2}L${x3} ${y3}A34 34 0 ${big} 0 ${x4} ${y4}Z" data-tip="${esc(`<b>${esc(label)}</b><br>${esc(fmt(raw))} · ${Math.round(100 * v / total)}%`)}"/>`;
    a += sweep;
  });
  out += '</svg><ul>' + items.map(([label, v, raw], i) => `<li><i class="sw c${i + 1}"></i><span>${esc(label)}</span><b>${esc(fmt(raw))}</b></li>`).join('') + '</ul></div>';
  return out;
}
const fmtShort = v => Math.abs(v) >= 1e6 ? (v / 1e6).toFixed(1).replace(/\.0$/, '') + 'M' : Math.abs(v) >= 1e3 ? (v / 1e3).toFixed(1).replace(/\.0$/, '') + 'k' : String(Math.round(v * 100) / 100);

// ── List (check, reorder, remove) ────────────────────────────────────────────
function list(c, s) {
  const p = pathOf(c.items, s), items = arr(c.items, s);
  const ul = el('ul', 'g-list');
  const write = next => { use('List'); if (p) setData(p, next); };
  items.forEach((raw, i) => {
    const it = typeof raw === 'object' && raw ? raw : {title: String(raw)};
    const li = el('li', it.done ? 'done' : '');
    if (c.reorderable && p) {
      const h = el('span', 'g-handle', icon('menu', 'sm')); h.title = 'Перетащите, чтобы изменить порядок'; h.setAttribute('aria-hidden', 'true');
      dragSort(h, li, ul, (from, to) => { const next = items.slice(); next.splice(to, 0, next.splice(from, 1)[0]); write(next); });
      li.append(h);
    }
    if (c.checkable) {
      const cb = el('input'); cb.type = 'checkbox'; cb.checked = !!it.done; cb.setAttribute('aria-label', 'Готово: ' + (it.title || ''));
      cb.onchange = () => { if (typeof raw === 'object') write(items.map((x, j) => j === i ? {...x, done: cb.checked} : x)); };
      li.append(cb);
    }
    const body = el('div', 'g-li', `<b>${esc(it.title ?? it.label ?? '')}</b>` + (it.subtitle ? `<small>${esc(it.subtitle)}</small>` : ''));
    li.append(body);
    if (it.badge) li.append(el('span', 'badge ' + (tone(it.tone) === 'danger' ? 'err' : tone(it.tone) || badgeTone(it.badge)), esc(it.badge)));
    if (c.itemAction) { body.classList.add('act'); body.tabIndex = 0; body.onclick = () => { use('List'); runAction(c.itemAction, s, {item: it, index: i}, c); }; body.onkeydown = e => { if (e.key === 'Enter') body.onclick(); }; }
    if (c.reorderable && p) {
      const up = btn('icon-btn sm g-move', icon('arrow-up', 'xs'), () => { if (i > 0) { const n = items.slice(); n.splice(i - 1, 0, n.splice(i, 1)[0]); write(n); } }, 'Выше');
      li.append(up);
    }
    if (c.removable && p) li.append(btn('icon-btn sm', icon('x', 'sm'), () => write(items.filter((_, j) => j !== i)), 'Убрать'));
    ul.append(li);
  });
  if (!items.length) ul.append(el('li', 'g-empty', 'Пусто'));
  return ul;
}
/** Drag by the handle (mouse and touch) to reorder siblings. */
function dragSort(handle, li, ul, done) {
  handle.style.touchAction = 'none';
  handle.onpointerdown = e => {
    e.preventDefault();
    const items = [...ul.children], from = items.indexOf(li);
    li.classList.add('dragging'); handle.setPointerCapture(e.pointerId);
    let to = from;
    const move = ev => {
      const y = ev.clientY;
      to = items.findIndex(x => { const r = x.getBoundingClientRect(); return y < r.top + r.height / 2; });
      if (to < 0) to = items.length - 1; else if (to > from) to--;
      items.forEach((x, j) => x.classList.toggle('drop-before', j === (to >= from ? to + 1 : to) && j !== from));
    };
    const up = () => {
      handle.onpointermove = handle.onpointerup = handle.onpointercancel = null;
      li.classList.remove('dragging'); items.forEach(x => x.classList.remove('drop-before'));
      if (to !== from) done(from, to);
    };
    handle.onpointermove = move; handle.onpointerup = up; handle.onpointercancel = up;
  };
}

// ── Tree ─────────────────────────────────────────────────────────────────────
function tree(c, s) {
  const nodes = arr(c.nodes, s);
  const st = uiState(c, s, {open: new Set()});
  const root = el('ul', 'g-tree'); root.setAttribute('role', 'tree');
  const add = (parent, list, trail, depth) => {
    for (const n of list.slice(0, 500)) {
      const path = trail ? trail + '/' + n.name : String(n.name ?? '');
      const li = el('li'); li.setAttribute('role', 'treeitem');
      const hasKids = Array.isArray(n.children) && n.children.length;
      const open = st.open.has(path) || (depth === 0 && list.length === 1);
      const row = btn('g-node', (hasKids ? icon('chevron-right', 'xs chev' + (open ? ' open' : '')) : '<span class="g-pad"></span>') +
        icon(hasKids ? 'folder' : 'file', 'sm') + `<span class="n">${esc(n.name ?? '')}</span>` +
        (n.badge ? `<span class="badge">${esc(n.badge)}</span>` : '') + (n.hint ? `<small>${esc(n.hint)}</small>` : ''));
      li.append(row);
      if (hasKids) {
        li.setAttribute('aria-expanded', open);
        const sub = el('ul'); sub.setAttribute('role', 'group');
        if (open) add(sub, n.children, path, depth + 1);
        li.append(sub);
        row.onclick = () => {
          use('Tree');
          if (st.open.has(path) || (depth === 0 && list.length === 1 && !st.open.has('!' + path))) { st.open.delete(path); st.open.add('!' + path); sub.innerHTML = ''; li.setAttribute('aria-expanded', false); row.querySelector('.chev')?.classList.remove('open'); }
          else { st.open.add(path); st.open.delete('!' + path); sub.innerHTML = ''; add(sub, n.children, path, depth + 1); li.setAttribute('aria-expanded', true); row.querySelector('.chev')?.classList.add('open'); }
        };
        if (depth === 0 && list.length === 1 && st.open.has('!' + path)) { sub.innerHTML = ''; li.setAttribute('aria-expanded', false); row.querySelector('.chev')?.classList.remove('open'); }
      } else if (c.itemAction) {
        row.onclick = () => { use('Tree'); runAction(c.itemAction, s, {node: {name: n.name, path, badge: n.badge}}, c); };
      }
      parent.append(li);
    }
  };
  add(root, nodes, '', 0);
  return root;
}

// ── Terminal: commands the person types, run on the machine ─────────────────
function terminal(c, s) {
  const p = pathOf(c.log, s) || `/__term_${c.id}`;
  const d = el('div', 'g-term');
  const lines = arr({path: p}, s);
  const out = pre(lines.join('\n') || 'Введите команду и нажмите Enter.', 'g-log');
  out.style.maxHeight = (+c.height || 300) + 'px';
  const row = el('form', 'g-termrow');
  const inp = el('input', 'input'); inp.placeholder = c.placeholder || 'команда…'; inp.setAttribute('autocapitalize', 'off'); inp.spellcheck = false;
  inp.setAttribute('aria-label', 'Команда');
  row.append(el('span', 'g-prompt', '$'), inp, btn('btn sm', icon('terminal', 'sm') + 'Выполнить', () => row.requestSubmit()));
  row.onsubmit = async e => {
    e.preventDefault();
    const cmd = inp.value.trim(); if (!cmd) return;
    use('Terminal'); inp.value = '';
    const args = {command: cmd}; if (c.cwd) args.workdir = c.cwd;
    // Typed by the person: that is the confirmation.
    await runTool({name: 'shell', args, target: p}, s, {confirmed: true, label: '$ ' + cmd});
  };
  d.append(out, row);
  requestAnimationFrame(() => { out.scrollTop = out.scrollHeight; });
  return d;
}

// ── Html: the agent's own code in a sandbox ──────────────────────────────────
function htmlFrame(c, s) {
  const f = el('iframe', 'g-frame');
  f.setAttribute('sandbox', 'allow-scripts');
  f.setAttribute('referrerpolicy', 'no-referrer');
  f.title = c.title || 'Встроенный интерфейс';
  f.style.height = (+c.height || 360) + 'px';
  const cs = getComputedStyle(document.documentElement);
  const vars = ['--fg', '--fg-2', '--bg', '--surface', '--surface-2', '--accent', '--accent-fg', '--line', '--muted', '--ok', '--warn', '--danger', '--font', '--mono', '--r-md']
    .map(v => `${v}:${cs.getPropertyValue(v)}`).join(';');
  const boot = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:">
<style>:root{${vars};color-scheme:${matchMedia('(prefers-color-scheme: dark)').matches && document.documentElement.dataset.theme !== 'light' || document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light'}}html,body{margin:0;background:transparent;color:var(--fg);font:15px/1.5 var(--font)}</style>
<script>(()=>{const post=m=>parent.postMessage(Object.assign({kron:1},m),'*');
window.kron={data:${JSON.stringify(G.data).replace(/</g, '\\u003c')},action:(name,context)=>post({type:'action',name:String(name),context:context||{}}),set:(path,value)=>post({type:'set',path:String(path),value}),onData:null};
addEventListener('message',e=>{if(e.source===parent&&e.data&&e.data.kron==='data'){kron.data=e.data.data;kron.onData&&kron.onData(kron.data);}});
const size=()=>post({type:'height',value:document.documentElement.scrollHeight});addEventListener('load',size);new ResizeObserver(size).observe(document.documentElement);
addEventListener('error',e=>post({type:'error',message:String(e.message)}));})();<\/script>`;
  f.srcdoc = boot + String(c.html || '');
  G.frames.add(f);
  f.addEventListener('load', () => { f.dataset.ok = '1'; });
  return f;
}
addEventListener('message', e => {
  const f = [...G.frames].find(x => x.contentWindow === e.source);
  if (!f || !e.data || e.data.kron !== 1) return;
  const m = e.data, id = f.dataset.gid;
  if (m.type === 'height') f.style.height = Math.min(Math.max(80, +m.value || 0), 2400) + 'px';
  else if (m.type === 'set' && typeof m.path === 'string' && m.path.startsWith('/')) { use('Html'); setData(m.path, m.value); }
  else if (m.type === 'action') { use('Html'); runAction({event: {name: m.name, context: m.context}}, '', {}, G.byId.get(id) || {id, component: 'Html'}); }
  else if (m.type === 'error') console.warn('Html component', id, m.message);
});

// ── Actions ──────────────────────────────────────────────────────────────────
async function runAction(action, scope, extra, comp, button) {
  if (!action || G.draft) return;
  if (action.event) {
    const ev = action.event;
    const context = {...resolve(ev.context || {}, scope), ...extra};
    if (comp?.variant === 'danger' && !await dialog({title: str(comp.label) || 'Подтвердите действие', text: 'Это действие может что-то изменить или удалить. Продолжить?', ok: 'Продолжить', danger: true})) return;
    return sendAction({name: String(ev.name || 'action'), context, component: comp?.id || ''});
  }
  if (action.functionCall) {
    const {call, args = {}} = action.functionCall;
    const a = resolve(args, scope);
    if (call === 'set' && typeof args.path === 'string') return setData(abs(args.path, scope), a.value);
    if (call === 'toggle' && typeof args.path === 'string') { const p = abs(args.path, scope); return setData(p, !ptr.get(G.data, p)); }
    if (call === 'openUrl' && /^https?:\/\//.test(a.url || '')) return window.open(a.url, '_blank', 'noopener');
    if (call === 'copy') return copy(String(a.text ?? ''), button);
    if (call === 'refresh') return refreshLive(a.source ? String(a.source) : null);
    return toast('Неизвестное действие: ' + call, {err: true});
  }
  if (action.tool) return runTool(action.tool, scope, {danger: comp?.variant === 'danger', label: str(comp?.label), button});
}
/** A tool straight from the interface, without the model. The hub enforces the
    thread's permissions; anything that changes things is confirmed here first. */
async function runTool(t, scope, {confirmed = false, danger = false, label = '', button} = {}) {
  if (!G.surface?.id) return;
  const name = String(t.name || ''), args = resolve(t.args || {}, scope);
  const target = typeof t.target === 'string' ? abs(t.target, scope) : null;
  const what = name === 'shell' ? String(args.command || '') : `${toolName(name)}: ${toolArg(name, args)}`;
  if (danger && !confirmed) {
    if (!await confirmTool(label, what, true)) return;
    confirmed = true;
  }
  if (button) { button.disabled = true; button.classList.add('busy'); }
  const append = lines => {
    if (!target) return;
    const cur = ptr.get(G.data, target);
    const next = Array.isArray(cur) ? cur.slice(-400) : cur ? [String(cur)] : [];
    setData(target, next.concat(lines));
  };
  try {
    let r;
    for (;;) {
      try { r = await postJSON(`/api/surfaces/${G.surface.id}/exec`, {tool: name, args, confirmed}); break; }
      catch (e) {
        if (/needs confirmation/.test(e.message) && !confirmed) {
          if (!await confirmTool(label, what, false)) return;
          confirmed = true; continue;
        }
        throw e;
      }
    }
    const output = name === 'shell' ? stripShell(r.output) : r.output;
    append([...(label ? [label] : name === 'shell' ? ['$ ' + what] : []), ...output.replace(/\n$/, '').split('\n')]);
    if (!target) toast(r.ok ? 'Готово' : 'Команда завершилась с ошибкой', {ic: r.ok ? 'check' : 'circle-alert', err: !r.ok});
    else if (!r.ok) toast('Команда завершилась с ошибкой', {err: true});
  } catch (e) {
    append(['ошибка: ' + e.message]);
    fail(e);
  } finally { if (button) { button.disabled = false; button.classList.remove('busy'); } }
}
function confirmTool(label, what, danger) {
  return dialog({title: label || 'Выполнить на машине?', text: `${what}\n\nКоманда выполнится на машине «${S.node || 'хаб'}».`, ok: 'Выполнить', danger});
}
/** A click the agent should handle: a new turn in the surface's thread. */
function sendAction(a) {
  if (S.busy) return toast('Агент ещё работает. Дождитесь или остановите его.');
  saveData();
  send('[ui-action] ' + JSON.stringify(a));
}

// ── Live data: sources the hub runs while the interface is open ─────────────
const L = {timers: [], paused: false, busy: new Set(), need: [], declined: false, last: new Map(), errors: new Map(), seen: new Map()};
const liveBox = el('div', 'g-livebox'); liveBox.hidden = true;
function stopLive() { L.timers.forEach(clearInterval); L.timers = []; }
/** (Re)start the timers. After the agent changed the surface, only new or changed sources run at once. */
function startLive(same) {
  stopLive();
  const list = G.surface?.id && !G.draft ? G.surface.sources || [] : [];
  if (!same) { L.need = []; L.declined = false; L.last.clear(); L.errors.clear(); L.seen.clear(); }
  for (const src of list) {
    const key = JSON.stringify(src);
    if (L.seen.get(src.id) !== key) { L.seen.set(src.id, key); runSource(src); }
    if (src.every > 0) L.timers.push(setInterval(() => { if (!L.paused && !document.hidden) runSource(src); }, src.every * 1000));
  }
  liveUI();
}
async function runSource(src) {
  const id = G.surface?.id;
  if (!id || L.busy.has(src.id) || L.need.includes(src.id)) return;
  L.busy.add(src.id);
  try {
    const r = await postJSON(`/api/surfaces/${id}/live`, {source: src.id, tz: new Date().getTimezoneOffset()});
    if (G.surface?.id !== id) return;
    if (r.ok) { applyData(r.path, r.value); L.last.set(src.id, Date.now()); L.errors.delete(src.id); }
    else L.errors.set(src.id, r.error);
  } catch (e) {
    if (G.surface?.id !== id) return;
    if (/needs approval/.test(e.message)) { if (!L.need.includes(src.id)) L.need.push(src.id); }
    else L.errors.set(src.id, /read-only/.test(e.message) ? 'режим «Только чтение»: команды не запускаются' : e.message);
  } finally { L.busy.delete(src.id); liveUI(); }
}
/** Run one source (or all) now. */
function refreshLive(id) {
  const list = (G.surface?.sources || []).filter(x => !id || x.id === id);
  if (!list.length) return toast('Нечего обновлять');
  list.forEach(runSource);
}
async function approveLive() {
  const ids = L.need.slice();
  try { await postJSON(`/api/surfaces/${G.surface.id}/approve`, {sources: ids}); } catch (e) { return fail(e); }
  L.need = [];
  (G.surface.sources || []).filter(x => ids.includes(x.id)).forEach(runSource);
}
const srcWhat = src => src.tool?.name === 'shell' ? String(src.tool.args?.command || '') : `${toolName(src.tool?.name)}: ${toolArg(src.tool?.name, src.tool?.args || {})}`;
const every = n => !n ? 'при открытии' : n < 60 ? `каждые ${n} с` : n < 3600 ? `каждые ${Math.round(n / 60)} мин` : `каждые ${Math.round(n / 3600)} ч`;
function liveUI() {
  liveBox.innerHTML = '';
  const need = L.declined ? [] : (G.surface?.sources || []).filter(x => L.need.includes(x.id));
  liveBox.hidden = !need.length;
  if (need.length) {
    liveBox.append(el('div', 'g-livehead', icon('activity', 'sm') + `<b>Интерфейс может обновляться сам</b>`),
      el('p', '', `Разрешите ${need.length > 1 ? 'эти команды' : 'эту команду'} на машине «${esc(S.node || 'хаб')}». Они только читают данные; изменится команда — kron спросит снова.`));
    const ul = el('ul');
    for (const x of need) ul.append(el('li', '', `<code>${esc(srcWhat(x))}</code><small>${esc(every(x.every))}</small>`));
    const row = el('div', 'row-end');
    row.append(btn('btn sm', 'Не сейчас', () => { L.declined = true; liveUI(); }), btn('btn sm primary', icon('check', 'sm') + 'Разрешить', () => approveLive()));
    liveBox.append(ul, row);
  }
  genBar();
}
function liveMenu() {
  const items = (G.surface?.sources || []).map(x => {
    const err = L.errors.get(x.id), t = L.last.get(x.id);
    return {icon: err ? 'circle-x' : L.need.includes(x.id) ? 'shield-alert' : 'circle-check',
      label: x.label || srcWhat(x).slice(0, 70),
      sub: err ? 'ошибка: ' + err.slice(0, 120) : L.need.includes(x.id) ? 'ждёт разрешения' : `${every(x.every)}${t ? ' · обновлено ' + new Date(t).toLocaleTimeString('ru') : ''}`,
      onClick: () => { if (L.need.includes(x.id)) { L.declined = false; liveUI(); } else runSource(x); }};
  });
  items.push({sep: true},
    {icon: 'refresh-cw', label: 'Обновить всё сейчас', onClick: () => { L.declined = false; liveUI(); refreshLive(); }},
    {icon: L.paused ? 'play' : 'pause', label: L.paused ? 'Продолжить обновление' : 'Пауза', onClick: () => { L.paused = !L.paused; genBar(); if (!L.paused) refreshLive(); }});
  if ((G.surface?.alerts || []).length && 'Notification' in window && Notification.permission === 'default') {
    items.push({icon: 'bell', label: 'Уведомлять об оповещениях', sub: 'Когда интерфейс не на экране', onClick: () => Notification.requestPermission()});
  }
  return items;
}

// ── Alerts: conditions the person wants to know about ───────────────────────
const alertBox = el('div', 'g-alerts'); alertBox.setAttribute('role', 'status');
function checkAlerts() {
  const list = G.surface && !G.draft ? G.surface.alerts || [] : [];
  const on = [];
  for (const a of list) {
    let hit = false, msg = '';
    try { hit = truthy(expr(a.when)); msg = hit ? str(a.text) || a.when : ''; } catch (e) { console.warn('alert', a.id, e.message); }
    if (hit) on.push({...a, msg});
    if (hit && !G.alertOn.has(a.id) && G.alertsSeen) notifyAlert(a, msg);
    if (hit) G.alertOn.add(a.id); else G.alertOn.delete(a.id);
  }
  G.alertsSeen = true;
  alertBox.innerHTML = '';
  for (const a of on) {
    const t = a.tone === 'danger' ? 'err' : a.tone === 'info' || a.tone === 'ok' ? a.tone : 'warn';
    alertBox.append(el('div', 'g-alert ' + t, icon(t === 'err' ? 'circle-alert' : t === 'ok' ? 'circle-check' : 'bell', 'sm') + `<span>${esc(a.msg)}</span>`));
  }
  alertBox.hidden = !on.length;
}
function notifyAlert(a, msg) {
  const title = G.surface?.title || 'kron';
  navigator.vibrate?.(200);
  if (document.hidden && 'Notification' in window && Notification.permission === 'granted') {
    try { new Notification(title, {body: msg, tag: 'kron-' + G.surface.id + '-' + a.id, icon: '/icon-192.png'}); } catch {}
  } else if (!document.hidden) toast(msg, {ic: 'bell', err: a.tone === 'danger'});
}

// ── Editing: the person rearranges the interface or points at a part ───────
function editFrame(c, node) {
  const box = el('div', 'g-editbox');
  const bar = el('div', 'g-edittools');
  const name = c.title || c.label || c.text;
  bar.append(el('span', 'g-editname', esc(COMP_NAMES[c.component] || c.component) + (typeof name === 'string' ? ' · ' + esc(name.slice(0, 30)) : '')));
  bar.append(
    btn('icon-btn sm', icon('arrow-up', 'xs'), () => layout('move', c.id, -1), 'Выше'),
    btn('icon-btn sm', icon('arrow-down', 'xs'), () => layout('move', c.id, 1), 'Ниже'),
    btn('icon-btn sm', icon('eye-off', 'sm'), () => layout('hide', c.id), 'Скрыть'),
    btn('btn sm', icon('sparkles', 'xs') + 'Изменить', () => askEdit(c), 'Сказать агенту, что сделать с этим блоком'));
  box.append(bar, node);
  box.dataset.gid = c.id;
  return box;
}
const COMP_NAMES = {Column: 'Колонка', Row: 'Ряд', Grid: 'Сетка', Card: 'Карточка', Tabs: 'Вкладки', Collapsible: 'Раскрывающийся блок', Divider: 'Разделитель',
  Heading: 'Заголовок', Text: 'Текст', Badge: 'Метка', Metric: 'Показатель', Progress: 'Прогресс', KeyValue: 'Свойства', Code: 'Код', Diff: 'Изменения', Log: 'Журнал',
  Image: 'Картинка', Table: 'Таблица', Chart: 'График', List: 'Список', Tree: 'Дерево', TextField: 'Поле', Select: 'Выбор', ChoicePicker: 'Варианты', CheckBox: 'Переключатель',
  Slider: 'Ползунок', Editor: 'Редактор', Button: 'Кнопка', Terminal: 'Терминал', Html: 'Свой блок'};
function setEditing(v) {
  G.editing = v;
  if (G.surface) draw();
  genBar();
}
async function layout(op, id, dir = 1) {
  await saveData();
  try {
    const s = await postJSON(`/api/surfaces/${G.surface.id}/layout`, {op, id, dir});
    showSurface(s);
    if (op === 'hide') toast('Блок скрыт', {ic: 'eye-off', action: 'Вернуть', onAction: () => undo()});
  } catch (e) { fail(e); }
}
async function askEdit(c) {
  if (S.busy) return toast('Агент ещё работает. Дождитесь или остановите его.');
  const what = COMP_NAMES[c.component] || c.component;
  const request = await dialog({title: `Что изменить: ${what.toLowerCase()}?`, input: true, placeholder: 'Например: покажи как график по дням, добавь колонку «цена»', ok: 'Изменить'});
  if (!request?.trim()) return;
  setEditing(false);
  saveData();
  send('[ui-edit] ' + JSON.stringify({component: c.id, type: c.component, request: request.trim()}));
}

// ── Header: activity, undo, versions, feedback ───────────────────────────────
const bar = el('div', 'g-bar'); bar.id = 'genBar';
$('filesBtn').before(bar);
function genBar() {
  bar.innerHTML = '';
  const on = !!G.surface;
  bar.hidden = !on;
  document.body.classList.toggle('activity', on && G.activity);
  if (!on) return;
  const act = btn('icon-btn' + (G.activity ? ' on' : ''), icon(S.busy ? 'loader-circle' : 'list-checks', S.busy ? 'spin' : ''), () => setActivity(!G.activity), 'Действия агента и переписка');
  act.setAttribute('aria-pressed', G.activity);
  const undoBtn = btn('icon-btn', icon('undo-2'), () => undo(), 'Отменить последнее изменение (Ctrl+Z)');
  undoBtn.disabled = !G.surface.id || (G.surface.version || 0) < 2;
  const more = btn('icon-btn', icon('ellipsis'), () => menu(more, surfaceMenu), 'Интерфейс');
  more.setAttribute('aria-haspopup', 'menu');
  bar.append(undoBtn);
  if ((G.surface.sources || []).length && G.surface.id) {
    const bad = L.errors.size || L.need.length;
    const live = btn('icon-btn g-live' + (L.paused ? ' paused' : bad ? ' bad' : ' on'), icon(L.paused ? 'pause' : 'activity'), () => menu(live, liveMenu, {width: 340}),
      L.paused ? 'Живые данные на паузе' : bad ? 'Живые данные: есть проблемы' : 'Живые данные обновляются');
    live.setAttribute('aria-haspopup', 'menu');
    bar.append(live);
  }
  if (G.surface.id) {
    const ed = btn('icon-btn g-edit' + (G.editing ? ' on' : ''), icon('pencil'), () => setEditing(!G.editing), G.editing ? 'Закончить настройку' : 'Настроить интерфейс: двигать, скрывать, изменить блок');
    ed.setAttribute('aria-pressed', G.editing);
    bar.append(ed);
  }
  bar.append(act, more);
}
function surfaceMenu() {
  const s = G.surface;
  return [
    {icon: 'thumbs-up', label: 'Удобно, запомнить этот вид', sub: 'Похожие задачи будут строиться так же', onClick: () => feedback('like')},
    {icon: 'thumbs-down', label: 'Не то — сделать по-другому', sub: 'Агент перестроит интерфейс', onClick: () => feedback('dislike')},
    {sep: true},
    {icon: 'pencil', label: G.editing ? 'Закончить настройку' : 'Настроить интерфейс', sub: 'Двигать и скрывать блоки, сказать агенту, что изменить', onClick: () => setEditing(!G.editing)},
    {icon: s.pinned ? 'x' : 'pin', label: s.pinned ? 'Убрать с главного экрана' : 'Закрепить на главном экране', onClick: () => pin(!s.pinned)},
    {icon: 'history', label: 'История версий…', onClick: () => versions()},
    {icon: 'message-square', label: G.activity ? 'Скрыть переписку и шаги' : 'Показать переписку и шаги', onClick: () => setActivity(!G.activity)},
    {icon: 'shield-check', label: 'Журнал команд', sub: `${(s.audit || []).length} запусков из интерфейса`, onClick: () => auditLog()},
    {icon: 'folder', label: 'Файлы на машине', onClick: () => openFiles(S.cwd() || '')},
    {icon: 'ellipsis', label: 'Чат: переименовать, копия, экспорт…', onClick: () => setTimeout(() => threadMenu($('genBar').lastChild, S.threads.find(x => x.name === S.thread) || {name: S.thread, title: S.title}), 0)},
    {sep: true},
    {icon: 'trash-2', label: 'Удалить интерфейс', sub: 'Переписка останется', danger: true, onClick: () => removeSurface()},
  ];
}
function setActivity(v) {
  G.activity = v;
  document.body.classList.toggle('activity', v && document.body.classList.contains('surface'));
  if (v) { stick = true; toBottom(true); }
  genBar();
}
async function undo(version) {
  if (!G.surface?.id) return;
  await saveData();
  try {
    const s = await postJSON(`/api/surfaces/${G.surface.id}/undo`, version ? {version} : {});
    showSurface(s);
    toast(version ? `Возвращена версия ${version}` : 'Изменение отменено', {ic: 'history', action: 'Вернуть', onAction: () => undo(s.version - 1)});
  } catch (e) { fail(e); }
}
async function versions() {
  try {
    const list = await getJSON(`/api/surfaces/${G.surface.id}/versions`);
    const who = {agent: 'агент', user: 'вы', undo: 'отмена'};
    menu($('genBar').lastChild, () => list.map(v => ({
      icon: v.by === 'agent' ? 'bot' : v.by === 'undo' ? 'history' : 'pencil',
      label: `Версия ${v.version} · ${who[v.by] || v.by}`, sub: `${fmtTime(v.ts)}${v.note ? ' · ' + v.note : ''}`,
      on: v.version === G.surface.version, onClick: () => v.version !== G.surface.version && undo(v.version),
    })), {search: list.length > 8 ? 'Найти версию' : undefined, width: 320});
  } catch (e) { fail(e); }
}
function auditLog() {
  const items = (G.surface.audit || []).slice().reverse();
  menu($('genBar').lastChild, () => items.length ? items.map(a => ({
    icon: a.ok ? 'circle-check' : 'circle-x', label: toolArg(a.tool, a.args || {}).slice(0, 80) || a.tool, sub: `${fmtTime(a.ts)} · ${toolName(a.tool)}`,
    onClick: () => copy(toolArg(a.tool, a.args || {})),
  })) : [{icon: 'info', label: 'Из этого интерфейса ещё ничего не запускалось'}], {width: 340});
}
async function pin(v) {
  try { const r = await postJSON(`/api/surfaces/${G.surface.id}/signal`, {kind: v ? 'pin' : 'unpin'}); G.surface.pinned = r.pinned; toast(v ? 'Закреплено на главном экране' : 'Откреплено', {ic: 'check'}); }
  catch (e) { fail(e); }
}
async function feedback(kind) {
  try { await postJSON(`/api/surfaces/${G.surface.id}/signal`, {kind}); } catch (e) { return fail(e); }
  if (kind === 'like') toast('Запомнил: похожие задачи начну с этого вида', {ic: 'check'});
  else send('Этот интерфейс мне не подходит. Перестрой его по-другому: другая компоновка и другие компоненты, удобнее для этой задачи.');
}
async function removeSurface() {
  const s = G.surface;
  if (!await dialog({title: 'Удалить интерфейс?', text: `«${s.title}» и его версии будут удалены. Переписка с агентом останется.`, ok: 'Удалить', danger: true})) return;
  try { await api('/api/surfaces/' + s.id, {method: 'DELETE'}); closeSurface(); setActivity(false); toast('Интерфейс удалён'); renderHome(); } catch (e) { fail(e); }
}
document.addEventListener('visibilitychange', () => {
  // Back on screen: catch up with what changed meanwhile.
  if (!document.hidden && G.surface?.id && !L.paused) (G.surface.sources || []).filter(x => x.every > 0).forEach(runSource);
});
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && G.editing && !document.querySelector('.dialog, .menu')) { setEditing(false); return; }
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z' && G.surface?.id && !S.busy &&
    !/INPUT|TEXTAREA|SELECT/.test(document.activeElement?.tagName)) { e.preventDefault(); undo(); }
});

// ── Live status while the agent works ────────────────────────────────────────
const statusEl = el('div', 'g-status'); statusEl.hidden = true;
statusEl.setAttribute('role', 'status');
$('composerWrap').prepend(statusEl);
function setStatus(text) {
  G.step = text;
  statusEl.hidden = !text || !document.body.classList.contains('surface');
  statusEl.innerHTML = '';
  if (!text) return;
  statusEl.append(el('span', '', icon('loader-circle', 'sm spin') + `<span class="t">${esc(text)}</span>`));
  statusEl.append(btn('btn sm ghost', 'Шаги', () => setActivity(true)), btn('btn sm ghost', icon('square', 'xs') + 'Стоп', () => stop()));
}

// ── Hooks called by app.js ───────────────────────────────────────────────────
const GEN = {
  /** A thread was opened: show its surface, if it has one. */
  async open(id) {
    matchBox.hidden = true;
    if (!id) { closeSurface(); return; }
    try {
      const s = await getJSON('/api/surfaces/' + id);
      showSurface(s);
      if (!G.surfaceOpened?.has(id)) { (G.surfaceOpened ??= new Set()).add(id); postJSON(`/api/surfaces/${id}/signal`, {kind: 'open'}).catch(() => {}); }
    } catch (e) { closeSurface(); }
  },
  close: closeSurface,
  /** A run event. */
  event(e) {
    switch (e.type) {
      case 'surface':
        if (e.draft && G.surface?.id) break; // a real surface is already there
        showSurface(e.surface, {draft: !!e.draft});
        if (!e.draft) { loadThreads().catch(() => {}); if (isMobile()) setActivity(false); }
        break;
      case 'tool_start':
        if (!e.parent && !['render_ui', 'patch_ui', 'update_data', 'remember'].includes(e.name)) setStatus(`${toolName(e.name)} · ${toolArg(e.name, parseArgs(e.args)).split('\n')[0].slice(0, 90)}`);
        else if (e.name === 'render_ui') setStatus('Собираю интерфейс…');
        else if (e.name === 'patch_ui') setStatus('Меняю интерфейс…');
        else if (e.name === 'update_data') setStatus('Обновляю данные…');
        break;
      case 'text': if (!G.step) setStatus('Отвечает…'); break;
      case 'ask': if (G.surface) setActivity(true); break;
      case 'done': case 'error':
        setStatus('');
        if (G.draft) { // the agent did not build a real one: drop the preview
          closeSurface();
        } else if (e.type === 'error' && G.surface) setActivity(true);
        genBar();
        break;
    }
  },
  started() { matchBox.hidden = true; if (document.body.classList.contains('surface')) setStatus('Думаю…'); genBar(); },
  stopped() { setStatus(''); genBar(); },
};
TOOLS.render_ui = ['sparkles', 'Интерфейс'];
TOOLS.patch_ui = ['pencil', 'Правка интерфейса'];
TOOLS.update_data = ['refresh-cw', 'Данные интерфейса'];
TOOLS.remember = ['brain', 'Запомнить'];

// ── Home: input first, then your interfaces ──────────────────────────────────
const INTENT_ICON = {compare: 'list-checks', project: 'folder-open', server: 'server', code: 'file-code', data: 'list-checks', plan: 'list-checks', write: 'pencil', chat: 'message-square'};
const EXAMPLES = [
  ['list-checks', 'Сравни Claude и Codex', 'Сравни Claude и Codex для повседневной разработки: качество кода, скорость, цена, лимиты.'],
  ['folder-open', 'Разберись в моём проекте', 'Разберись в моём проекте: структура, зависимости, как собрать и что стоит сделать.'],
  ['server', 'Что с машиной', 'Покажи состояние машины: диск, память, нагрузка, самые тяжёлые процессы. С кнопками для обновления.'],
  ['file-code', 'Помоги написать код', 'Помоги написать код: '],
];
let homeSurfaces = [];
async function renderHome() {
  if (S.mode !== 'ui' || S.thread) return;
  const box = $('sugg');
  try { homeSurfaces = await getJSON('/api/surfaces'); } catch { homeSurfaces = []; }
  if (S.mode !== 'ui' || S.thread) return;
  box.innerHTML = ''; box.className = 'sugg g-home';
  const mine = homeSurfaces.slice().sort((a, b) => (b.pinned - a.pinned) || (b.updated - a.updated)).slice(0, 6);
  if (mine.length) {
    const h = el('div', 'g-hhead', '<span>Ваши интерфейсы</span>');
    if (homeSurfaces.length > mine.length) h.append(btn('btn sm ghost', `Все (${homeSurfaces.length})`, e => allSurfaces(e.currentTarget)));
    box.append(h);
    const g = el('div', 'g-tiles');
    for (const s of mine) g.append(tile(s));
    box.append(g);
  } else {
    for (const [ic, title, prompt] of EXAMPLES) box.append(btn('', icon(ic) + `<span>${esc(title)}</span>`, () => {
      const t = $('text'); t.value = prompt; grow(); t.focus(); t.setSelectionRange(t.value.length, t.value.length);
    }));
  }
}
function tile(s) {
  const b = btn('g-tile', `${icon(INTENT_ICON[s.intent] || 'sparkles', 'sm')}<span class="t">${esc(s.title || 'Без названия')}</span><small>${s.pinned ? icon('pin', 'xs') + 'закреплён · ' : ''}${esc(fmtTime(s.updated))}</small>`, () => openThread(s.thread));
  b.title = s.title;
  return b;
}
function allSurfaces(anchor) {
  menu(anchor, () => homeSurfaces.map(s => ({icon: INTENT_ICON[s.intent] || 'sparkles', label: s.title || 'Без названия', sub: `${s.intent || ''} · ${fmtTime(s.updated)}`, on: s.pinned || undefined, onClick: () => openThread(s.thread)})), {search: 'Найти интерфейс', width: 340});
}
// While typing on the start screen: an earlier interface that fits.
const matchBox = el('div', 'g-match'); matchBox.hidden = true;
$('composer').before(matchBox);
const checkMatch = debounce(async () => {
  const q = $('text').value.trim();
  if (S.mode !== 'ui' || S.thread || q.length < 6) { matchBox.hidden = true; return; }
  try {
    const m = await getJSON('/api/genui/match?q=' + encodeURIComponent(q));
    if ($('text').value.trim() !== q || S.thread) return;
    matchBox.innerHTML = '';
    const s = m.surfaces[0];
    if (s) {
      matchBox.append(el('span', '', icon('history', 'sm') + `Похоже на «${esc(s.title)}»`), btn('btn sm', 'Открыть', () => { matchBox.hidden = true; openThread(s.thread); }));
    } else if (m.template) {
      matchBox.append(el('span', 'muted', icon('sparkles', 'sm') + `Начну с вашего вида «${esc(m.template.title)}»`));
    }
    matchBox.hidden = !matchBox.children.length;
  } catch { matchBox.hidden = true; }
}, 350);
$('text').addEventListener('input', checkMatch);

// ── Voice input ──────────────────────────────────────────────────────────────
const Rec = window.SpeechRecognition || window.webkitSpeechRecognition;
const mic = btn('icon-btn', icon('mic'), () => toggleVoice(), 'Голосовой ввод');
mic.id = 'micBtn';
$('send').before(mic);
mic.hidden = !Rec && !android?.listen;
let rec = null;
function toggleVoice() {
  if (rec) { rec.stop(); return; }
  const t = $('text'), base = t.value ? t.value.replace(/\s*$/, ' ') : '';
  if (!Rec && android?.listen) {
    mic.classList.add('rec');
    window.kronVoice = (text, err) => { mic.classList.remove('rec'); if (err) return toast(err, {err: true}); t.value = base + text; grow(); t.focus(); };
    return android.listen(navigator.language || 'ru-RU');
  }
  rec = new Rec();
  rec.lang = navigator.language || 'ru-RU'; rec.interimResults = true; rec.continuous = false;
  rec.onresult = e => { t.value = base + [...e.results].map(r => r[0].transcript).join(''); grow(); };
  rec.onerror = e => { if (e.error !== 'no-speech' && e.error !== 'aborted') toast(e.error === 'not-allowed' ? 'Нет доступа к микрофону' : 'Голосовой ввод: ' + e.error, {err: true}); };
  rec.onend = () => { rec = null; mic.classList.remove('rec'); mic.setAttribute('aria-pressed', false); checkMatch(); t.focus(); };
  try { rec.start(); mic.classList.add('rec'); mic.setAttribute('aria-pressed', true); }
  catch (e) { rec = null; fail(e); }
}

// ── Settings: what kron knows about you ──────────────────────────────────────
TABS.splice(1, 0, ['memory', 'brain', 'Память и интерфейсы']);
TAB_FNS.memory = async body => {
  body.append(el('p', 'lead', 'kron подстраивает интерфейсы под вас: запоминает ваши пожелания, какие элементы вы используете и какие интерфейсы оказались удобными. Всё это хранится на хабе и доступно с любого устройства.'));
  let p;
  try { p = await getJSON('/api/genui/profile'); } catch (e) { return body.append(el('p', 'msg err', esc(e.message))); }
  body.append(el('h4', '', 'Ваши пожелания'));
  if (!p.notes.length) body.append(el('p', 'hint-text', 'Пока ничего. Скажите агенту, например: «запомни, что я предпочитаю таблицы».'));
  for (const n of p.notes) body.append(card('brain', esc(n.text), fmtTime(n.ts), btn('icon-btn sm', icon('trash-2', 'sm'), async () => { await api('/api/genui/notes/' + n.id, {method: 'DELETE'}).catch(fail); openSettings('memory'); }, 'Забыть')));
  const add = el('form', 'g-addnote'), inp = el('input', 'input'); inp.placeholder = 'Например: цены в рублях, без графиков';
  add.append(inp, btn('btn sm', 'Добавить', () => add.requestSubmit()));
  add.onsubmit = async e => { e.preventDefault(); if (!inp.value.trim()) return; await postJSON('/api/genui/notes', {text: inp.value}).catch(fail); openSettings('memory'); };
  body.append(add);

  body.append(el('h4', '', 'Запомненные виды'));
  if (!p.templates.length) body.append(el('p', 'hint-text', 'Когда интерфейс окажется удобным (вы им пользуетесь, закрепили или отметили «Удобно»), его вид станет основой для похожих задач.'));
  for (const t of p.templates) body.append(card(INTENT_ICON[t.intent] || 'sparkles', esc(t.title), `${t.intent} · использован ${t.uses} раз · ${t.components} элементов`, btn('icon-btn sm', icon('trash-2', 'sm'), async () => { await api('/api/genui/templates/' + t.id, {method: 'DELETE'}).catch(fail); openSettings('memory'); }, 'Забыть этот вид')));

  const aff = Object.entries(p.affinity || {}).filter(([, m]) => Object.keys(m).length);
  if (aff.length) {
    body.append(el('h4', '', 'Чем вы пользуетесь'));
    for (const [intent, m] of aff) {
      const top = Object.entries(m).sort((a, b) => b[1] - a[1]).slice(0, 5).map(([k, w]) => `${k} ${w >= 0 ? '' : '(−)'}`).join(', ');
      body.append(card(INTENT_ICON[intent] || 'sparkles', esc(intent), top));
    }
  }
  body.append(el('h4', '', 'Сбросить'));
  body.append(btn('btn danger', 'Забыть всё, кроме пожеланий', async () => {
    if (!await dialog({title: 'Сбросить обучение?', text: 'Запомненные виды и статистика будут удалены. Интерфейсы и пожелания останутся.', ok: 'Сбросить', danger: true})) return;
    await postJSON('/api/genui/reset', {notes: false}).catch(fail); openSettings('memory');
  }));
};

window.GEN = GEN;
window.renderHome = renderHome;
