// Browser test of the generative UI renderer, without a model: a fixture
// surface goes into a temporary kron home, the hub serves it, Chromium drives it.
//
//   cargo build && node tests/ui/surface.test.cjs
//
// Needs Playwright with Chromium (npm i -g playwright && npx playwright install chromium).

const {chromium} = require('playwright');
const {spawn} = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const assert = require('assert/strict');

const root = path.resolve(__dirname, '../..');
const bin = process.env.KRON_BIN ? path.resolve(process.env.KRON_BIN) : path.join(root, 'target/debug', process.platform === 'win32' ? 'kron.exe' : 'kron');
const port = 7790 + Math.floor(Math.random() * 100);
const base = `http://127.0.0.1:${port}`;
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'kron-ui-'));
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'kron-work-'));

const surface = {
  id: 's-test', thread: 't1', title: 'Тестовая панель', intent: 'server', request: 'test', root: 'root', version: 1,
  created: Date.now(), updated: Date.now(), pinned: false,
  components: [
    {id: 'root', component: 'Column', children: ['h', 'metrics', 'sum', 'load', 'picked', 'filter', 'tbl', 'tabs', 'todo', 'name', 'flag', 'set', 'run', 'log', 'frame', 'bad']},
    {id: 'h', component: 'Heading', text: {path: '/title'}, level: 1},
    {id: 'metrics', component: 'Grid', children: {path: '/metrics', componentId: 'metric'}},
    {id: 'metric', component: 'Metric', label: {path: 'label'}, value: {path: 'value'}, hint: {expr: "upper(@label) + ' / ' + len(/metrics)"}},
    {id: 'sum', component: 'Metric', label: 'Всего CPU', value: {expr: 'sum(/rows/*/cpu)'}, hint: {expr: "count(/rows/*/status, 'ok') + ' из ' + len(/rows) + ' в норме'"}},
    {id: 'load', component: 'Metric', label: 'Нагрузка', value: {path: '/load'}},
    {id: 'picked', component: 'Badge', text: {expr: "'Выбрано: ' + join(/show)"}, visible: {expr: 'len(/show) > 0'}},
    {id: 'filter', component: 'ChoicePicker', label: 'Статус', value: {path: '/show'}, options: ['ok', 'down'], multiple: true},
    {id: 'tbl', component: 'Table', rows: {path: '/rows'}, rowKey: 'id', search: true, filter: {key: 'status', path: '/show'},
      columns: [{key: 'name', label: 'Сервис', editable: true}, {key: 'cpu', label: 'CPU', type: 'number'}, {key: 'status', label: 'Статус', type: 'badge'}]},
    {id: 'tabs', component: 'Tabs', tabs: [{label: 'Первая', children: ['t1']}, {label: 'Вторая', children: ['t2']}]},
    {id: 't1', component: 'Text', text: 'первая вкладка'},
    {id: 't2', component: 'Chart', kind: 'bar', rows: {path: '/rows'}, x: 'name', series: [{key: 'cpu', label: 'CPU'}, {key: 'mem', label: 'RAM'}]},
    {id: 'todo', component: 'List', items: {path: '/todo'}, checkable: true, reorderable: true, removable: true},
    {id: 'name', component: 'TextField', label: 'Имя', value: {path: '/form/name'}},
    {id: 'flag', component: 'CheckBox', label: 'Включено', value: {path: '/form/on'}},
    {id: 'set', component: 'Button', label: 'Сбросить имя', action: {functionCall: {call: 'set', args: {path: '/form/name', value: 'сброшено'}}}},
    {id: 'run', component: 'Button', label: 'Привет', action: {tool: {name: 'shell', args: {command: 'echo hello-from-ui'}, target: '/out'}}},
    {id: 'log', component: 'Log', lines: {path: '/out'}},
    {id: 'frame', component: 'Html', height: 80, html: '<button id=b onclick="kron.set(\'/fromFrame\', 42)">go</button><script>document.getElementById(\'b\').click()</script>'},
    {id: 'bad', component: 'Table', rows: {path: '/rows'}, columns: 'not-an-array'},
  ],
  data: {
    title: 'Сервер', show: [],
    metrics: [{label: 'Нагрузка', value: 0.5}, {label: 'Память', value: '3 ГБ'}],
    rows: [{id: 'a', name: 'nginx', cpu: 12, mem: 3, status: 'ok'}, {id: 'b', name: 'db', cpu: 40, mem: 9, status: 'down'}, {id: 'c', name: 'cache', cpu: 3, mem: 1, status: 'ok'}],
    todo: [{id: '1', title: 'Первое'}, {id: '2', title: 'Второе'}],
    form: {name: 'исходное', on: false}, out: [],
  },
  stats: {}, audit: [],
  sources: [{id: 'load', tool: {name: 'shell', args: {command: 'echo 42'}}, target: '/load', every: 2, parse: 'number'}],
  alerts: [{id: 'hi', when: '/load > 40', text: {expr: "'Нагрузка ' + /load"}, tone: 'danger'}],
};

function setup() {
  fs.writeFileSync(path.join(home, 'config.toml'), `[hub]\nlisten = "127.0.0.1:${port}"\nnode = "test"\nworkdir = ${JSON.stringify(work)}\n`);
  const sdir = path.join(home, 'genui/surfaces'), tdir = path.join(home, 'threads');
  fs.mkdirSync(sdir, {recursive: true}); fs.mkdirSync(tdir, {recursive: true});
  fs.writeFileSync(path.join(sdir, 's-test.json'), JSON.stringify(surface));
  const v = {version: 1, ts: Date.now(), by: 'agent', note: '', title: surface.title, root: 'root', components: surface.components, data: surface.data};
  fs.writeFileSync(path.join(sdir, 's-test.versions.jsonl'), JSON.stringify(v) + '\n');
  fs.writeFileSync(path.join(tdir, 't1.jsonl'), JSON.stringify({id: 'r1', ts: Date.now(), role: 'user', content: 'test'}) + '\n');
  fs.writeFileSync(path.join(tdir, 't1.meta.json'), JSON.stringify({surface: 's-test', settings: {mode: 'ui', access: 'ask', node: 'test'}}));
}

const get = p => fetch(base + p).then(r => r.json());
const wait = ms => new Promise(r => setTimeout(r, ms));

(async () => {
  setup();
  const hub = spawn(bin, ['serve'], {env: {...process.env, KRON_HOME: home}, cwd: work, stdio: ['ignore', 'inherit', 'inherit']});
  let ok = false;
  try {
    for (let i = 0; i < 50 && !ok; i++) { await wait(200); ok = await fetch(base + '/api/info').then(r => r.ok, () => false); }
    assert.ok(ok, 'hub did not start');
    const browser = await chromium.launch();
    const page = await browser.newPage({viewport: {width: 390, height: 844}, hasTouch: true});
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.goto(base + '/?thread=t1');
    await page.waitForSelector('body.surface .g-root');

    // Bindings and templates.
    assert.equal(await page.textContent('.g-h1'), 'Сервер');
    assert.equal(await page.locator('.g-grid .g-metric').count(), 2);
    // Expressions: computed values, relative paths in templates, visibility.
    assert.equal(await page.textContent('[data-gid=sum] .v'), '55');
    assert.equal(await page.textContent('[data-gid=sum] .h'), '2 из 3 в норме');
    assert.equal(await page.locator('.g-grid .g-metric .h').first().textContent(), 'НАГРУЗКА / 2');
    assert.ok(await page.locator('[data-gid=picked]').isHidden());
    assert.deepEqual(await page.evaluate(() => [
      expr('2 + 3 * 4'), expr('max(/rows/*/cpu) / 2'), expr("pluck(where(/rows, 'status', 'down'), 'name')"),
      expr("/form/name == 'исходное' && !/form/on ? 'да' : 'нет'"), expr('/missing ?? -1'), expr('pct(1, 3)'),
    ]), [14, 20, ['db'], 'да', -1, 33.3]);
    assert.match(await page.evaluate(() => { try { expr('1 +'); } catch (e) { return e.message; } }), /формула/);
    // A broken component does not take the surface down.
    assert.equal(await page.locator('.g-table').count(), 2);

    // Table: sort, filter through the choice picker, search, edit a cell.
    const names = () => page.$$eval('.g-table >> nth=0 >> tbody tr td:first-child', tds => tds.map(t => t.textContent));
    await page.locator('.g-table').first().locator('th', {hasText: 'CPU'}).locator('button').click();
    assert.deepEqual(await names(), ['cache', 'nginx', 'db']);
    await page.click('.g-choices .chip:has-text("down")');
    assert.deepEqual(await names(), ['db']);
    assert.equal(await page.textContent('[data-gid=picked]'), 'Выбрано: down');
    await page.click('.g-choices .chip:has-text("down")');
    assert.ok(await page.locator('[data-gid=picked]').isHidden());
    await page.locator('.g-table').first().locator('input[type=search]').fill('ngi');
    await wait(300);
    assert.deepEqual(await names(), ['nginx']);
    await page.locator('.g-table').first().locator('td.edit').first().click();
    await page.keyboard.press('End');
    await page.keyboard.type('-new');
    await page.keyboard.press('Enter');

    // Tabs and the chart behind them, with a working legend.
    await page.click('.g-tabbar button:has-text("Вторая")');
    assert.equal(await page.locator('.g-svg .mk').count(), 6);
    await page.click('.g-lg:has-text("RAM")');
    assert.equal(await page.locator('.g-svg .mk').count(), 3);

    // List: check, move, remove.
    await page.locator('.g-list input[type=checkbox]').first().check();
    await page.locator('.g-list li').nth(1).locator('button[title="Убрать"]').click();

    // Inputs and a local action.
    await page.fill('.g-field:has-text("Имя") input', 'Вася');
    await page.click('.g-check');
    await page.click('button:has-text("Сбросить имя")');
    assert.equal(await page.inputValue('.g-field:has-text("Имя") input'), 'сброшено');

    // The sandbox wrote through kron.set.
    await page.waitForFunction(() => G.data.fromFrame === 42);

    // A tool from a button: access "ask" needs the person's confirmation.
    await page.click('button:has-text("Привет")');
    await page.waitForSelector('.dialog');
    assert.match(await page.textContent('.dialog'), /echo hello-from-ui/);
    await page.click('.dialog .btn.primary');
    await page.waitForFunction(() => (G.data.out || []).some(l => l.includes('hello-from-ui')));

    // Live data: a shell source waits for the person's approval, then keeps the value fresh.
    await page.waitForSelector('.g-livebox:not([hidden])');
    assert.match(await page.textContent('.g-livebox'), /echo 42/);
    await page.click('.g-livebox .btn.primary');
    await page.waitForFunction(() => G.data.load === 42);
    assert.equal(await page.textContent('[data-gid=load] .v'), '42');
    // The alert's condition became true.
    await page.waitForSelector('.g-alert.err');
    assert.equal(await page.textContent('.g-alert'), 'Нагрузка 42');
    await page.evaluate(() => applyData('/load', 1));
    assert.ok(await page.locator('.g-alerts').isHidden());
    await page.waitForFunction(() => G.data.load === 42, null, {timeout: 6000}); // the timer ran again
    const live = await get('/api/surfaces/s-test');
    assert.equal(live.data.load, 42);

    // Everything was saved on the hub.
    await page.evaluate(() => saveData());
    await wait(400);
    const s = await get('/api/surfaces/s-test');
    assert.equal(s.data.rows.find(r => r.id === 'a').name, 'nginx-new');
    assert.equal(s.data.todo.length, 1);
    assert.equal(s.data.todo[0].done, true);
    assert.equal(s.data.form.name, 'сброшено');
    assert.equal(s.data.form.on, true);
    assert.equal(s.data.fromFrame, 42);
    assert.ok(s.data.out.some(l => l.includes('hello-from-ui')));
    assert.equal(s.audit.length, 1);

    // Undo goes back through the versions.
    const before = s.version;
    await page.evaluate(() => undo(1));
    await page.waitForFunction(() => G.data.form.name === 'исходное');
    assert.ok((await get('/api/surfaces/s-test')).version > before);

    // The person rearranges the interface: move, hide, and undo.
    await page.click('.g-bar button[title="Интерфейс"]');
    await page.click('.menu button:has-text("Настроить интерфейс")');
    await page.waitForSelector('.g-editbox');
    await page.locator('.g-editbox[data-gid=h] button[title="Ниже"]').click();
    await page.waitForFunction(() => G.byId.get('root').children[0] === 'metrics');
    await page.locator('.g-editbox[data-gid=bad] button[title="Скрыть"]').click();
    await page.waitForFunction(() => !G.byId.has('bad'));
    assert.equal(await page.locator('.g-table').count(), 1);
    const laid = await get('/api/surfaces/s-test');
    assert.deepEqual(laid.components.find(c => c.id === 'root').children.slice(0, 2), ['metrics', 'h']);
    await page.evaluate(() => undo());
    await page.waitForFunction(() => G.byId.has('bad'));
    // Pointing at a block and saying what to change goes to the agent as [ui-edit].
    await page.evaluate(() => { window.sent = []; window.send = t => sent.push(t); });
    await page.locator('.g-editbox[data-gid=tbl] button:has-text("Изменить")').click();
    await page.fill('.dialog input', 'покажи как график');
    await page.click('.dialog .btn.primary');
    const sent = await page.evaluate(() => window.sent);
    assert.equal(sent.length, 1);
    assert.deepEqual(JSON.parse(sent[0].replace('[ui-edit] ', '')), {component: 'tbl', type: 'Table', request: 'покажи как график'});
    assert.equal(await page.locator('.g-editbox').count(), 0);

    // Use is reported and learned.
    await page.evaluate(() => flushSignals());
    await wait(300);
    const prof = await get('/api/genui/profile');
    assert.ok(prof.affinity.server && prof.affinity.server.Table >= 1, JSON.stringify(prof.affinity));

    assert.deepEqual(errors, []);
    await browser.close();
    console.log('ok: generative UI renderer');
  } finally {
    hub.kill();
    fs.rmSync(home, {recursive: true, force: true});
    fs.rmSync(work, {recursive: true, force: true});
  }
})().catch(e => { console.error(e); process.exit(1); });
