// Browser test of kron's kernel, without a model: the empty start, an
// interface written into ~/.kron/ui (as the agent would), a lock confirmed with
// the person's password, and the service screens.
//
//   cargo build && node tests/ui/kernel.test.cjs
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
const wait = ms => new Promise(r => setTimeout(r, ms));

// What an agent might write: a page that keeps a secret behind a lock.
const APP = `<!doctype html><meta charset="utf-8"><title>Мой kron</title>
<script src="/kron/sdk.js"></script>
<h1>Сейф</h1><p id="out">—</p>
<button id="lock" onclick="go()">Запереть и открыть</button>
<script>
async function go() {
  await kron.data.set('vault', {pin: '4321'});
  await kron.api('/api/locks', {json: {title: 'Открыть сейф', method: 'GET', path: '/api/ui/data/vault'}});
  const v = await kron.data.get('vault');
  document.getElementById('out').textContent = v.pin;
}
</script>`;

(async () => {
  fs.writeFileSync(path.join(home, 'config.toml'), `[hub]\nlisten = "127.0.0.1:${port}"\nnode = "test"\nworkdir = ${JSON.stringify(home)}\n`);
  const hub = spawn(bin, ['serve'], {env: {...process.env, KRON_HOME: home}, stdio: ['ignore', 'inherit', 'inherit']});
  let ok = false;
  try {
    for (let i = 0; i < 50 && !ok; i++) { await wait(200); ok = await fetch(base + '/api/info').then(r => r.ok, () => false); }
    assert.ok(ok, 'hub did not start');
    const browser = await chromium.launch();
    const ctx = await browser.newContext({viewport: {width: 390, height: 844}, hasTouch: true});
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));

    // Nothing written yet: / is the rescue page with one input.
    await page.goto(base + '/');
    await page.waitForSelector('#msg');
    assert.match(await page.textContent('#lead'), /пока ничего нет/);
    assert.ok(await page.locator('#open').isHidden());
    // Without a model the turn fails, and says so.
    await page.fill('#msg', 'сделай мне канбан');
    await page.click('#send');
    await page.waitForSelector('.out .err');
    assert.doesNotMatch(await page.textContent('.out .err'), /связь/); // the hub's own error, not a broken stream

    // The agent wrote an interface: / serves it, unknown routes fall back to it, .git stays hidden.
    fs.mkdirSync(path.join(home, 'ui'), {recursive: true});
    fs.writeFileSync(path.join(home, 'ui/index.html'), APP);
    fs.mkdirSync(path.join(home, 'ui/.git'), {recursive: true});
    fs.writeFileSync(path.join(home, 'ui/.git/config'), 'secret');
    assert.equal((await fetch(base + '/.git/config')).status, 404);
    assert.equal((await fetch(base + '/../config.toml')).status, 404);
    assert.match(await (await fetch(base + '/board/42')).text(), /Сейф/);
    await page.goto(base + '/');
    assert.equal(await page.textContent('h1'), 'Сейф');
    // The SDK talks to the agent; without a model it gets the hub's error, not a broken stream.
    assert.doesNotMatch(await page.evaluate(() => kron.send('привет').then(() => 'ok', e => e.message)), /connection|^ok$/);

    // A lock: the call waits for the person; kron's own page asks for the password.
    const popupP = page.waitForEvent('popup');
    await page.click('#lock');
    const popup = await popupP;
    await popup.waitForSelector('#form:not([hidden])');
    assert.equal(await popup.textContent('#title'), 'Открыть сейф');
    assert.match(await popup.textContent('#what'), /GET \/api\/ui\/data\/vault/);
    // The interface cannot reach into kron's page.
    assert.equal(await page.evaluate(() => { try { return String(window.open('', 'kron-confirm').document.querySelector('#pw')); } catch (e) { return 'blocked'; } }).catch(() => 'blocked') !== '[object HTMLInputElement]', true);
    await popup.fill('#pw', 'my-secret');
    await popup.fill('#pw2', 'my-secret');
    await popup.click('#ok');
    await page.waitForFunction(() => document.getElementById('out').textContent === '4321', null, {timeout: 10000});

    // The next read is locked again; without the password nothing gets through.
    const again = await page.evaluate(() => fetch('/api/ui/data/vault').then(r => r.status));
    assert.equal(again, 423);
    const locks = await (await fetch(base + '/api/locks')).json();
    assert.equal(locks.password, true);
    assert.equal(locks.locks.length, 1);
    // Taking a lock off is locked itself, and a wrong password does not confirm.
    const off = await fetch(base + '/api/locks/' + locks.locks[0].id, {method: 'DELETE'});
    assert.equal(off.status, 423);
    const {challenge} = await off.json();
    const wrong = await fetch(base + '/api/presence/' + challenge, {method: 'POST', headers: {'content-type': 'application/json'}, body: '{"password":"nope"}'});
    assert.equal(wrong.status, 403);
    assert.equal((await fetch(base + '/api/locks/' + locks.locks[0].id, {method: 'DELETE'})).status, 423);
    // The agent's ui_data does not see it either (checked in Rust); data stays on disk outside the interface.
    assert.ok(fs.existsSync(path.join(home, 'ui-data/vault.json')));

    // kron's pages refuse to be framed and keep their own browsing context.
    const conf = await fetch(base + '/kron/confirm?c=x');
    assert.equal(conf.headers.get('x-frame-options'), 'DENY');
    assert.equal(conf.headers.get('cross-origin-opener-policy'), 'same-origin');

    // The service app (sign-in, settings) still works.
    await page.goto(base + '/kron/app');
    await page.waitForSelector('#text');
    // The rescue page knows there is an interface now.
    await page.goto(base + '/kron/');
    await page.waitForSelector('#open:not([hidden])');

    assert.deepEqual(errors, []);
    await browser.close();
    console.log('ok: kernel');
  } finally {
    hub.kill();
    fs.rmSync(home, {recursive: true, force: true});
  }
})().catch(e => { console.error(e); process.exit(1); });
