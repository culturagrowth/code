export const DEFAULTS = Object.freeze({
  quality: 'alta', encoder: 'auto', resolution: '1920x1080', fps: 60, bitrate: 30,
  before: 30, after: 10, hotkey: 'Alt+F10',
  gameAudio: true, discordAudio: true, microphone: false,
  gameVolume: 100, discordVolume: 100, micVolume: 100,
  ignoredDiscord: 'DiscordCanary.exe', sounds: true, clipSound: 'padrao',
  saveSound: 'padrao', soundVolume: 80, gameExe: '', clipFolder: '',
});
export const PRESETS = Object.freeze({
  baixa: ['1280x720', 30, 6], media: ['1920x1080', 60, 15],
  alta: ['1920x1080', 60, 30], muito_alta: ['2560x1440', 60, 45],
});

export function validate(config) {
  const errors = [];
  const range = (key, min, max, label) => {
    if (!Number.isInteger(config[key]) || config[key] < min || config[key] > max)
      errors.push(`${label}: use um número inteiro de ${min} a ${max}.`);
  };
  if (![...Object.keys(PRESETS), 'personalizada'].includes(config.quality)) errors.push('Escolha uma qualidade válida.');
  if (!['auto', 'placa_de_video', 'software'].includes(config.encoder)) errors.push('Escolha um encoder válido.');
  range('before', 5, 120, 'Segundos antes');
  range('after', 0, 60, 'Segundos depois');
  if (config.quality === 'personalizada') {
    const match = /^(\d+)x(\d+)$/.exec(config.resolution);
    const width = Number(match?.[1]), height = Number(match?.[2]);
    if (!match || width < 640 || width > 3840 || height < 360 || height > 2160 || width % 2 || height % 2)
      errors.push('Resolução: use lados pares de 640x360 a 3840x2160.');
    if (![30, 60, 120, 144].includes(config.fps)) errors.push('FPS: escolha 30, 60, 120 ou 144.');
    range('bitrate', 2, 100, 'Bitrate');
  }
  for (const key of ['gameVolume', 'discordVolume', 'micVolume']) range(key, 0, 200, 'Volume do áudio');
  range('soundVolume', 0, 100, 'Volume do aviso sonoro');
  for (const key of ['gameAudio', 'discordAudio', 'microphone', 'sounds'])
    if (typeof config[key] !== 'boolean') errors.push('Escolha válida necessária para as opções de áudio.');
  for (const key of ['hotkey', 'resolution', 'ignoredDiscord', 'clipSound', 'saveSound', 'gameExe', 'clipFolder'])
    if (typeof config[key] !== 'string' || /[\x00-\x1f\x7f]/.test(config[key])) errors.push('Os campos de texto não podem conter caracteres de controle.');
  if (!validHotkey(config.hotkey)) errors.push('Atalho inválido. Use, por exemplo, F10 ou Ctrl+F9. Letras, números e Space precisam de Ctrl, Alt ou Win.');
  if (config.gameExe && !/^[^\\/:*?"<>|]+\.exe$/i.test(config.gameExe)) errors.push('Jogo: informe somente o nome do executável, como javaw.exe.');
  for (const key of ['clipSound', 'saveSound'])
    if (typeof config[key] !== 'string' || !(/^(padrao|padrão)$/i.test(config[key]) || /\.wav$/i.test(config[key])))
      errors.push('Som: use padrao ou o caminho de um arquivo .wav.');
  return errors;
}

function validHotkey(value) {
  if (typeof value !== 'string') return false;
  const parts = value.split('+').map(part => part.trim().toLowerCase());
  const key = parts.pop();
  if (new Set(parts).size !== parts.length || parts.some(part => !['ctrl', 'alt', 'shift', 'win'].includes(part))) return false;
  const simple = /^[a-z0-9]$/.test(key) || key === 'space';
  const named = ['insert', 'ins', 'delete', 'del', 'home', 'end', 'pageup', 'pgup', 'pagedown', 'pgdn', 'pause', 'scrolllock', 'printscreen', 'prtsc', 'up', 'down', 'left', 'right'];
  const accepted = simple || /^f([1-9]|1\d|2[0-4])$/.test(key) || /^num[0-9]$/.test(key) || named.includes(key);
  return accepted && (!simple || parts.some(part => ['ctrl', 'alt', 'win'].includes(part)));
}

export function videoSettings(config) {
  const preset = PRESETS[config.quality];
  return preset ? { resolution: preset[0], fps: preset[1], bitrate: preset[2] }
    : { resolution: config.resolution, fps: config.fps, bitrate: config.bitrate };
}

export function toToml(config) {
  const errors = validate(config);
  if (errors.length) throw new Error(errors.join('\n'));
  const quote = value => JSON.stringify(value);
  const lines = [
    '# DuoClip — preferências exportadas pela interface',
    '# Confira antes de substituir a configuração do gravador.',
    `qualidade = ${quote(config.quality)}`,
    `encoder = ${quote(config.encoder)}`,
    `segundos_antes = ${config.before}`, `segundos_depois = ${config.after}`,
    `atalho = ${quote(config.hotkey)}`, `pasta_clipes = ${quote(config.clipFolder)}`,
  ];
  if (config.quality === 'personalizada') lines.push(`resolucao = ${quote(config.resolution)}`, `fps = ${config.fps}`, `bitrate_mbps = ${config.bitrate}`);
  lines.push('', '[aviso_sonoro]', `ativado = ${config.sounds}`, `som_ao_clipar = ${quote(config.clipSound)}`,
    `som_ao_salvar = ${quote(config.saveSound)}`, `volume = ${config.soundVolume}`,
    '', '[audio]', `jogo = ${config.gameAudio}`, `discord = ${config.discordAudio}`, `microfone = ${config.microphone}`,
    `volume_jogo = ${config.gameVolume}`, `volume_discord = ${config.discordVolume}`, `volume_microfone = ${config.micVolume}`,
    `ignorar_discord = ${quote(config.ignoredDiscord)}`, '', '[jogo]', `exe = ${quote(config.gameExe)}`, '');
  return lines.join('\n');
}

export function restoreDraft(raw) {
  try {
    const parsed = JSON.parse(raw);
    if (parsed?.version !== 1 || !parsed.config || Array.isArray(parsed.config)) return { ...DEFAULTS };
    const config = Object.fromEntries(Object.keys(DEFAULTS).map(key => [key, parsed.config[key] ?? DEFAULTS[key]]));
    return validate(config).length ? { ...DEFAULTS } : config;
  } catch { return { ...DEFAULTS }; }
}
