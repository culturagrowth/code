import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { createUiServer } from './serve.mjs';

const output = fileURLToPath(new URL('../../../test-output/interface-inicial/', import.meta.url));
const run = join(output, `run-${Date.now()}`);
await mkdir(run, { recursive: true });
const edge = process.env.DUOCLIP_EDGE_PATH || 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
await access(edge);
const media = join(run, 'highlight.mp4');
const generated = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'color=c=0x7962a7:s=640x360:r=30', '-t', '1', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-y', media], { windowsHide: true, encoding: 'utf8' });
assert.equal(generated.status, 0, generated.error?.message || generated.stderr);
const server = createUiServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = spawn(edge, ['--headless=new', '--disable-gpu', '--disable-extensions', '--disable-background-networking', '--no-first-run', '--no-default-browser-check', '--remote-debugging-port=0', `--user-data-dir=${join(run, 'profile')}`, 'about:blank'], { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
let stderr = ''; browser.stderr.on('data', chunk => { stderr += chunk; });
browser.on('error', error => { stderr += error.message; });
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
let ws;
const failures = [], unexpectedRequests = [];
try {
  let port;
  for (let attempt = 0; attempt < 60; attempt++) {
    try { port = (await readFile(join(run, 'profile', 'DevToolsActivePort'), 'utf8')).split('\n')[0]; break; }
    catch { if (browser.exitCode !== null) break; await delay(200); }
  }
  assert.ok(port, `Edge não iniciou o DevTools. ${stderr.slice(-1800)}`);
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find(item => item.type === 'page'); assert.ok(target);
  ws = new WebSocket(target.webSocketDebuggerUrl); await once(ws, 'open');
  const pending = new Map(); let sequence = 0;
  ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const entry = pending.get(message.id); if (!entry) return;
      pending.delete(message.id); clearTimeout(entry.timeout);
      if (message.error) entry.reject(new Error(JSON.stringify(message.error))); else entry.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') failures.push(message.params.exceptionDetails.text);
    else if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') failures.push('Console error');
    else if (message.method === 'Network.requestWillBeSent') {
      const url = message.params.request.url;
      if (!url.startsWith(origin + '/') && !url.startsWith(`blob:${origin}/`) && !url.startsWith('data:')) unexpectedRequests.push(url);
    }
  });
  const cdp = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timeout = setTimeout(() => { pending.delete(id); reject(new Error(`Timeout: ${method}`)); }, 8000);
    pending.set(id, { resolve, reject, timeout }); ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async expression => {
    const result = await cdp('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
  };
  const until = async expression => {
    for (let i = 0; i < 50; i++) { if (await evaluate(expression)) return; await delay(100); }
    throw new Error(`Condition failed: ${expression}`);
  };
  const screenshot = async name => {
    const result = await cdp('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
    await writeFile(join(output, name), Buffer.from(result.data, 'base64'));
  };
  const version = await cdp('Browser.getVersion');
  console.log(`Browser: ${version.product}`);
  await cdp('Page.enable'); await cdp('Runtime.enable'); await cdp('Network.enable');
  await cdp('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: run });
  await cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await cdp('Page.navigate', { url: origin });
  await until(`document.querySelector('#home-hotkey')?.textContent === 'AltF10'`);
  assert.equal(await evaluate(`document.querySelectorAll('[aria-current="page"]').length`), 1);
  await screenshot('inicio.png');
  await evaluate(`location.hash='clipes'`); await until(`!document.querySelector('#page-clipes').hidden`);
  const { root } = await cdp('DOM.getDocument');
  const { nodeId } = await cdp('DOM.querySelector', { nodeId: root.nodeId, selector: '#clip-input' });
  await cdp('DOM.setFileInputFiles', { nodeId, files: [media] });
  await until(`document.querySelectorAll('.clip-card').length === 1 && document.querySelector('.clip-preview video').readyState >= 1`);
  assert.equal(await evaluate(`document.querySelector('.clip-preview video').videoWidth`), 640);
  await evaluate(`document.querySelector('.clip-preview').click()`);
  await until(`document.querySelector('#player-dialog').open && document.querySelector('#clip-player').currentTime > 0`);
  await evaluate(`document.querySelector('#close-player').click()`);
  await until(`document.querySelector('#clip-player').getAttribute('src') === null`);
  assert.equal(await evaluate(`document.querySelector('#clip-player').getAttribute('src')`), null);
  await screenshot('biblioteca.png');
  await evaluate(`document.querySelector('#clip-search').value='inexistente'; document.querySelector('#clip-search').dispatchEvent(new Event('input'))`);
  assert.equal(await evaluate(`document.querySelectorAll('.clip-card').length`), 0);
  await evaluate(`document.querySelector('#clip-search').value=''; document.querySelector('#clip-search').dispatchEvent(new Event('input'))`);
  assert.equal(await evaluate(`document.querySelectorAll('.clip-card').length`), 1);
  // A hostile filename must remain text; it must never become DOM/HTML.
  await evaluate(`{
    const input=document.querySelector('#clip-input'), data=new DataTransfer();
    data.items.add(new File(['invalid mp4'], '<img src=x onerror=alert(1)>.mp4', {type:'video/mp4'}));
    input.files=data.files; input.dispatchEvent(new Event('change'));
  }`);
  assert.equal(await evaluate(`document.querySelectorAll('.clip-body img').length`), 0);
  await evaluate(`document.querySelectorAll('.clip-meta button')[1].click()`);
  await evaluate(`document.querySelector('.clip-meta button').click()`);
  assert.equal(await evaluate(`document.querySelectorAll('.clip-card').length`), 0);
  await access(media);
  await evaluate(`location.hash='configuracoes'`); await until(`!document.querySelector('#page-configuracoes').hidden`);
  await evaluate(`{ const form=document.querySelector('#settings-form'); form.elements.before.value=45; form.elements.after.value=0; form.elements.hotkey.value='F10'; form.dispatchEvent(new Event('input',{bubbles:true})); form.requestSubmit(); }`);
  assert.equal(await evaluate(`JSON.parse(localStorage.getItem('duoclip.preferences.v1')).config.before`), 45);
  await evaluate(`document.querySelector('#export-config').click()`);
  let downloaded;
  for (let i = 0; i < 30; i++) { try { downloaded = await readFile(join(run, 'config.toml'), 'utf8'); break; } catch { await delay(100); } }
  assert.match(downloaded || '', /segundos_antes = 45\nsegundos_depois = 0/);
  assert.match(downloaded, /atalho = "F10"/);
  await writeFile(join(output, 'config-exportada.toml'), downloaded);
  await screenshot('configuracoes.png');
  await cdp('Page.reload'); await until(`document.querySelector('#settings-form')?.elements.before.value === '45'`);
  assert.equal(await evaluate(`document.querySelector('#summary-duration').textContent`), '45');
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  for (const route of ['inicio', 'clipes', 'amigos', 'configuracoes']) {
    await evaluate(`location.hash=${JSON.stringify(route)}`); await until(`!document.querySelector('#page-${route}').hidden`);
    assert.ok(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), `Overflow on ${route}`);
  }
  await screenshot('mobile.png');
  assert.deepEqual(failures, []); assert.deepEqual(unexpectedRequests, []);
  await writeFile(join(output, 'browser-result.json'), JSON.stringify({ ok: true, checks: ['routing', 'real-mp4-playback', 'search', 'filename-as-text', 'remove-preserves-file', 'config-export', 'saved-settings-reload', 'mobile-overflow', 'no-external-requests'], screenshots: ['inicio.png', 'biblioteca.png', 'configuracoes.png', 'mobile.png'] }, null, 2));
  console.log(`Interface validada. Evidências: ${output}`);
  await cdp('Browser.close').catch(() => {});
} finally {
  await writeFile(join(run, 'edge-stderr.log'), stderr);
  ws?.close(); if (browser.exitCode === null) browser.kill();
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
}
