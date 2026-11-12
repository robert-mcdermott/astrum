// Headless Chrome, driven over the DevTools protocol with Node's own WebSocket: enough to
// load a page and photograph part of it.

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const CANDIDATES = [
  process.env.CHROME,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome',
  '/usr/bin/google-chrome-stable',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
].filter(Boolean);

export async function launch() {
  const bin = CANDIDATES.find((p) => existsSync(p));
  if (!bin) throw new Error('no Chrome or Chromium found; name one with CHROME=/path/to/chrome');
  if (typeof WebSocket === 'undefined') throw new Error('this Node has no WebSocket; use Node 22 or newer');
  const profile = mkdtempSync(join(tmpdir(), 'astrum-chrome-'));
  const child = spawn(bin, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--hide-scrollbars', '--force-color-profile=srgb', '--font-render-hinting=none', `--user-data-dir=${profile}`, '--remote-debugging-port=0', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
  const wsUrl = await new Promise((ok, no) => {
    let err = '';
    const timer = setTimeout(() => no(new Error(`Chrome did not start: ${err}`)), 20000);
    child.stderr.on('data', (d) => {
      err += d;
      const m = /DevTools listening on (ws:\/\/\S+)/.exec(err);
      if (m) {
        clearTimeout(timer);
        ok(m[1]);
      }
    });
    child.on('exit', (code) => no(new Error(`Chrome stopped (${code}): ${err}`)));
  });
  const port = new URL(wsUrl).port;
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = targets.find((t) => t.type === 'page');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok, no) => {
    ws.onopen = ok;
    ws.onerror = no;
  });
  let id = 0;
  const waiting = new Map();
  const listeners = new Map();
  ws.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.id && waiting.has(msg.id)) {
      const { ok, no } = waiting.get(msg.id);
      waiting.delete(msg.id);
      msg.error ? no(new Error(`${msg.error.message}`)) : ok(msg.result);
    } else if (msg.method && listeners.has(msg.method)) {
      for (const f of listeners.get(msg.method).splice(0)) f(msg.params);
    }
  };
  const send = (method, params = {}) =>
    new Promise((ok, no) => {
      waiting.set(++id, { ok, no });
      ws.send(JSON.stringify({ id, method, params }));
    });
  const once = (method) => new Promise((ok) => listeners.set(method, [...(listeners.get(method) ?? []), ok]));
  await send('Page.enable');

  return {
    /// Load `url` at `width` × `height` CSS pixels, and photograph the element `selector`.
    async shoot(url, { width, height, selector = 'body', format = 'webp', quality = 88, scale = 2 }) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: scale, mobile: false });
      const loaded = once('Page.loadEventFired');
      await send('Page.navigate', { url });
      await loaded;
      const { result } = await send('Runtime.evaluate', {
        expression: `document.fonts.ready.then(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return JSON.stringify([r.x, r.y, r.width, r.height]); })`,
        awaitPromise: true,
        returnByValue: true,
      });
      const [x, y, w, h] = JSON.parse(result.value);
      const shot = await send('Page.captureScreenshot', { format, ...(format === 'png' ? {} : { quality }), clip: { x, y, width: w, height: h, scale: 1 }, captureBeyondViewport: true });
      return Buffer.from(shot.data, 'base64');
    },
    async close() {
      try {
        await send('Browser.close');
      } catch {}
      const gone = new Promise((ok) => child.once('exit', ok));
      child.kill();
      await Promise.race([gone, new Promise((ok) => setTimeout(ok, 3000))]);
      rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    },
  };
}
