/* kron SDK: optional helpers for the interface the agent writes.
   kron.api(path, {method, json, body, headers}) → parsed JSON (or text); waits for
     the person's confirmation when the call is locked.
   kron.send(text, {thread, onEvent, ...run options}) → Promise<{run, thread}> after the turn;
     onEvent(event, {run, thread}) gets every event as it comes.
   kron.data.get(key) / set(key, value) / delete(key) / keys(). */
(() => {
  'use strict';
  const wait = ms => new Promise(r => setTimeout(r, ms));

  async function confirm(info) {
    const w = window.open(info.confirm, 'kron-confirm', 'popup,width=480,height=640');
    if (!w) { location.href = info.confirm; return new Promise(() => {}); }
    for (let i = 0; i < 240; i++) {
      await wait(1000);
      const r = await fetch('/api/presence/' + info.challenge);
      if (!r.ok) break;
      if ((await r.json()).approved) return true;
    }
    throw new Error('Действие не подтверждено');
  }

  async function api(path, opts = {}) {
    const method = opts.method || (opts.json !== undefined || opts.body !== undefined ? 'POST' : 'GET');
    const headers = {...(opts.headers || {})};
    let body = opts.body;
    if (opts.json !== undefined) { headers['content-type'] = 'application/json'; body = JSON.stringify(opts.json); }
    for (;;) {
      const r = await fetch(path, {method, headers, body});
      if (r.status === 423) { await confirm(await r.json()); continue; }
      if (!r.ok) throw new Error((await r.text()) || r.statusText);
      if (r.status === 204) return null;
      const type = r.headers.get('content-type') || '';
      return type.includes('json') ? r.json() : r.text();
    }
  }

  function send(text, opts = {}) {
    const {onEvent, ...rest} = opts;
    return api('/api/runs', {json: {mode: 'ui', interactive: true, thread: '', ...rest, text}}).then(run => new Promise((resolve, reject) => {
      const es = new EventSource(`/api/runs/${run.run}/events`);
      es.onmessage = m => {
        const e = JSON.parse(m.data);
        try { onEvent?.(e, run); } catch (err) { console.error(err); }
        if (e.type === 'done') { es.close(); resolve(run); }
        if (e.type === 'error') { es.close(); reject(Object.assign(new Error(e.message), {run})); }
      };
      es.onerror = () => { es.close(); reject(new Error('connection to the hub lost')); };
    }));
  }

  const data = {
    keys: () => api('/api/ui/data'),
    get: key => api('/api/ui/data/' + encodeURIComponent(key)).catch(e => (/no such key/.test(e.message) ? undefined : Promise.reject(e))),
    set: (key, value) => api('/api/ui/data/' + encodeURIComponent(key), {method: 'PUT', json: value}),
    delete: key => api('/api/ui/data/' + encodeURIComponent(key), {method: 'DELETE'}),
  };

  window.kron = {api, send, data, answer: (run, ask, answer) => api(`/api/runs/${run}/answer`, {json: {ask, answer}}), stop: run => api(`/api/runs/${run}/cancel`, {method: 'POST'})};
})();
