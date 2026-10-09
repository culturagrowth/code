import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createUiServer } from '../tools/serve.mjs';

test('preview only serves the interface, never repository files or credentials', async t => {
  const server = createUiServer();
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const main = await fetch(origin);
  assert.equal(main.status, 200); assert.match(await main.text(), /<html lang="pt-BR">/);
  const script = await fetch(`${origin}/src/app.js`);
  assert.match(script.headers.get('content-type'), /javascript/);
  for (const path of ['/worker/.dev.vars', '/.git/config', '/package.json', '/%2e%2e/worker/.dev.vars', '/src/../../worker/.dev.vars'])
    assert.equal((await fetch(origin + path)).status, 404);
});
