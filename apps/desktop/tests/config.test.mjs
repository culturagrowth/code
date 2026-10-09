import test from 'node:test';
import assert from 'node:assert/strict';
import { DEFAULTS, validate, toToml, restoreDraft } from '../src/config.js';

test('exports every recorder section, with clip folder at the top level and escaped Windows paths', () => {
  const config = { ...DEFAULTS, clipFolder: 'C:\\Vídeos\\Meus "clipes"', gameExe: 'javaw.exe', microphone: true, clipSound: 'C:\\Sons\\clip.wav' };
  const toml = toToml(config);
  assert.ok(toml.indexOf('pasta_clipes =') < toml.indexOf('[aviso_sonoro]'));
  assert.ok(toml.includes(`pasta_clipes = ${JSON.stringify(config.clipFolder)}`));
  assert.ok(toml.includes(`som_ao_clipar = ${JSON.stringify(config.clipSound)}`));
  assert.match(toml, /\[audio\][\s\S]*microfone = true/);
  assert.match(toml, /\[jogo\]\nexe = "javaw.exe"/);
  assert.ok(!toml.includes('resolucao ='));
});
test('rejects invalid timing and custom encoder inputs before a file is produced', () => {
  for (const config of [
    { ...DEFAULTS, before: 121 }, { ...DEFAULTS, after: -1 }, { ...DEFAULTS, before: 5.5 },
    { ...DEFAULTS, quality: 'personalizada', resolution: '1921x1080' },
    { ...DEFAULTS, quality: 'personalizada', resolution: '3840x2162' },
    { ...DEFAULTS, quality: 'personalizada', fps: 59 },
    { ...DEFAULTS, quality: 'personalizada', bitrate: 101 },
    { ...DEFAULTS, gameExe: 'C:\\Games\\game.exe' },
    { ...DEFAULTS, hotkey: 'Shift+A' }, { ...DEFAULTS, hotkey: 'F25' },
  ]) assert.throws(() => toToml(config));
  assert.deepEqual(validate({ ...DEFAULTS, quality: 'personalizada', resolution: '3840x2160', fps: 144, bitrate: 100, hotkey: 'Ctrl+Shift+A' }), []);
});
test('corrupt or incompatible saved drafts recover to defaults without trusting unknown keys', () => {
  for (const raw of ['{broken', 'null', JSON.stringify({ version: 2, config: DEFAULTS }), JSON.stringify({ version: 1, config: { before: '30' } }), JSON.stringify({ version: 1, config: { hotkey: null, quality: 12 } })])
    assert.deepEqual(restoreDraft(raw), DEFAULTS);
  const recovered = restoreDraft(JSON.stringify({ version: 1, config: { ...DEFAULTS, after: 0, microphone: true, secret: 'ignored' } }));
  assert.equal(recovered.after, 0); assert.equal(recovered.microphone, true); assert.ok(!Object.hasOwn(recovered, 'secret'));
});
