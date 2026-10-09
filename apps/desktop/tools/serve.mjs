import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, extname } from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const mime = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.svg': 'image/svg+xml' };
const allowed = new Set(['index.html', 'styles.css', 'assets/logo.svg', 'src/app.js', 'src/config.js']);
export function createUiServer() {
  return createServer(async (request, response) => {
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('Referrer-Policy', 'no-referrer');
    response.setHeader('Cache-Control', 'no-store');
    response.setHeader('Content-Security-Policy', "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self'; media-src 'self' blob:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'");
    if (!['GET', 'HEAD'].includes(request.method)) { response.writeHead(405); response.end(); return; }
    let path;
    try { path = decodeURIComponent(new URL(request.url, 'http://127.0.0.1').pathname).slice(1) || 'index.html'; }
    catch { response.writeHead(400); response.end(); return; }
    if (!allowed.has(path)) { response.writeHead(404); response.end('Não encontrado'); return; }
    try {
      const bytes = await readFile(resolve(root, path));
      response.writeHead(200, { 'Content-Type': mime[extname(path)] });
      response.end(request.method === 'HEAD' ? undefined : bytes);
    } catch { response.writeHead(404); response.end('Não encontrado'); }
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const server = createUiServer();
  const port = Number(process.env.DUOCLIP_UI_PORT || 1420);
  server.once('error', error => { console.error(`Não foi possível abrir a interface: ${error.message}`); process.exitCode = 1; });
  server.listen(port, '127.0.0.1', () => console.log(`DuoClip: http://127.0.0.1:${server.address().port}\nCtrl+C para encerrar. Sem gravação ou acesso à nuvem.`));
  process.on('SIGINT', () => server.close());
}
