import { DEFAULTS, restoreDraft, validate, videoSettings, toToml } from './config.js';

const $ = selector => document.querySelector(selector);
const storageKey = 'duoclip.preferences.v1';
const clips = [];
let savedConfig;
try { savedConfig = restoreDraft(localStorage.getItem(storageKey)); }
catch { savedConfig = { ...DEFAULTS }; }
const form = $('#settings-form');
let toastTimer;
let nextClipId = 1;
let lastRoute;

function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  const use = document.createElementNS(svg.namespaceURI, 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.setAttribute('aria-hidden', 'true');
  svg.append(use);
  return svg;
}
function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function notify(message) {
  clearTimeout(toastTimer);
  $('#toast').textContent = message;
  $('#toast').hidden = false;
  toastTimer = setTimeout(() => { $('#toast').hidden = true; }, 4500);
}
function navigate() {
  const names = { inicio: 'Início', clipes: 'Meus clipes', amigos: 'Amigos e grupos', configuracoes: 'Configurações' };
  const requested = location.hash.slice(1) || 'inicio';
  const route = Object.hasOwn(names, requested) ? requested : 'inicio';
  document.querySelectorAll('.page').forEach(page => { page.hidden = page.id !== `page-${route}`; });
  document.querySelectorAll('[data-route]').forEach(link => {
    if (link.dataset.route === route) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  });
  $('#page-name').textContent = names[route];
  document.title = `${names[route]} — DuoClip`;
  if (lastRoute && route !== lastRoute) { $('#main').focus({ preventScroll: true }); window.scrollTo(0, 0); }
  lastRoute = route;
}

const audioSources = [
  ['gameAudio', 'gameVolume', 'Áudio do jogo', 'Só o som do jogo e dos seus processos.'],
  ['discordAudio', 'discordVolume', 'Discord', 'A conversa que faz parte da jogada.'],
  ['microphone', 'micVolume', 'Microfone', 'Sua voz. Desligado por padrão.'],
];
for (const [enabled, volume, title, description] of audioSources) {
  const row = element('div', 'audio-row');
  const label = element('label', 'switch-row');
  const text = element('span'); text.append(element('strong', '', title), element('small', '', description));
  const toggle = element('input'); toggle.type = 'checkbox'; toggle.name = enabled; toggle.setAttribute('role', 'switch');
  label.append(text, toggle);
  const volumeLabel = element('label', 'volume-field', `Volume · ${title.toLowerCase()}`);
  const control = element('span');
  const range = element('input'); range.type = 'range'; range.name = volume; range.min = '0'; range.max = '200';
  const output = element('output'); control.append(range, output); volumeLabel.append(control);
  row.append(label, volumeLabel); $('#audio-settings').append(row);
}

function populate(config) {
  for (const [key, value] of Object.entries(config)) {
    const field = form.elements.namedItem(key);
    if (!field) continue;
    if (field.type === 'checkbox') field.checked = value;
    else field.value = String(value);
  }
  updateSettings();
}
function readConfig() {
  return Object.fromEntries(Object.entries(DEFAULTS).map(([key, initial]) => {
    const field = form.elements.namedItem(key);
    let value = field.type === 'checkbox' ? field.checked : field.value;
    if (typeof initial === 'number') value = Number(value);
    return [key, value];
  }));
}
function updateSettings() {
  const config = readConfig();
  $('#custom-quality').hidden = config.quality !== 'personalizada';
  for (const name of ['resolution', 'fps', 'bitrate']) form.elements.namedItem(name).disabled = config.quality !== 'personalizada';
  for (const range of form.querySelectorAll('input[type="range"]')) range.parentElement.querySelector('output').textContent = `${range.value}%`;
  const before = Math.max(0, config.before), after = Math.max(0, config.after);
  $('#before-label').textContent = `${before} s antes`;
  $('#after-label').textContent = `${after} s depois`;
  $('.before-bar').style.flex = String(before || .001);
  $('.after-bar').style.flex = String(after || .001);
  $('#summary-duration').textContent = String(before + after);
  const video = videoSettings(config);
  const audio = [config.gameAudio && 'Jogo', config.discordAudio && 'Discord', config.microphone && 'Mic'].filter(Boolean).join(' + ') || 'Desligado';
  $('#summary-list').replaceChildren();
  for (const [label, value] of [['Resolução', video.resolution], ['Quadros', `${video.fps} FPS`], ['Bitrate', `${video.bitrate} Mbps`], ['Atalho', config.hotkey || '—'], ['Áudio', audio]]) {
    const row = element('div'); row.append(element('span', '', label), element('strong', '', value)); $('#summary-list').append(row);
  }
  $('#draft-state').textContent = JSON.stringify(config) === JSON.stringify(savedConfig) ? 'Preferências locais' : 'Alterações não salvas';
}
function showErrors(config) {
  const errors = validate(config);
  const container = $('#settings-errors');
  container.replaceChildren(...errors.map(message => element('p', '', message)));
  container.hidden = errors.length === 0;
  return errors.length > 0;
}
form.addEventListener('input', () => { updateSettings(); $('#settings-errors').hidden = true; });
form.addEventListener('change', updateSettings);
form.addEventListener('submit', event => {
  event.preventDefault();
  const config = readConfig();
  if (showErrors(config)) return;
  try {
    localStorage.setItem(storageKey, JSON.stringify({ version: 1, config }));
    savedConfig = config; updateSettings(); renderHomeStats();
    notify('Preferências salvas neste navegador. Baixe o arquivo para aplicar ao gravador.');
  } catch { notify('Não foi possível guardar as preferências. Você ainda pode baixar o arquivo.'); }
});
$('#export-config').addEventListener('click', () => {
  const config = readConfig();
  if (!form.reportValidity() || showErrors(config)) return;
  const url = URL.createObjectURL(new Blob([toToml(config)], { type: 'application/toml;charset=utf-8' }));
  const link = element('a'); link.href = url; link.download = 'config.toml'; document.body.append(link); link.click(); link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  notify('Configuração exportada. A gravação não foi iniciada.');
});

function renderHomeStats() {
  const video = videoSettings(savedConfig);
  const resolution = video.resolution.split('x')[1] || '—';
  const audioCount = [savedConfig.gameAudio, savedConfig.discordAudio, savedConfig.microphone].filter(Boolean).length;
  const stats = [['clips', String(clips.length), 'Clipes na biblioteca'], ['clock', `${savedConfig.before + savedConfig.after} s`, 'Duração configurada'], ['monitor', `${resolution}p · ${video.fps}`, 'Qualidade · FPS'], ['audio', `${audioCount} fontes`, 'Áudio configurado']];
  $('#stats').replaceChildren(...stats.map(([symbol, value, label]) => {
    const card = element('div', 'stat-card'), image = element('span', 'stat-icon'), text = element('span');
    image.append(icon(symbol)); text.append(element('strong', '', value), element('small', '', label)); card.append(image, text); return card;
  }));
  const keys = savedConfig.hotkey.split('+');
  $('#home-hotkey').replaceChildren(...keys.map(key => element('kbd', '', key)));
  $('#nav-count').textContent = String(clips.length);
}
function formatSize(bytes) { return `${(bytes / 1048576).toLocaleString('pt-BR', { maximumFractionDigits: 1 })} MB`; }
function formatDate(timestamp) { return new Date(timestamp).toLocaleDateString('pt-BR', { day: '2-digit', month: 'short' }); }
function formatDuration(seconds) { return Number.isFinite(seconds) ? `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}` : 'MP4'; }
function emptyState(title, description, actionable = true) {
  const wrapper = element('div', 'empty-state'), image = element('div', 'empty-icon'); image.append(icon('clips'));
  wrapper.append(image, element('h3', '', title), element('p', '', description));
  if (actionable) {
    const button = element('button', 'button secondary', 'Adicionar meu primeiro clipe'); button.addEventListener('click', selectClips); wrapper.append(button);
  }
  return wrapper;
}
function selectClips() { $('#clip-input').click(); }
document.querySelectorAll('.import-button').forEach(button => button.addEventListener('click', selectClips));
$('#clip-input').addEventListener('change', event => { importClips(event.target.files); event.target.value = ''; });

function importClips(files) {
  let added = 0, invalid = 0;
  for (const file of files) {
    if (!/\.mp4$/i.test(file.name) || file.size === 0) { invalid++; continue; }
    if (clips.some(clip => clip.name === file.name && clip.size === file.size && clip.modified === file.lastModified)) continue;
    clips.push({ id: nextClipId++, name: file.name, size: file.size, modified: file.lastModified, url: URL.createObjectURL(file), duration: null });
    added++;
  }
  if (added) { renderClips(); renderHomeStats(); location.hash = 'clipes'; }
  notify(added ? `${added} clipe${added === 1 ? '' : 's'} adicionado${added === 1 ? '' : 's'}.${invalid ? ' Alguns arquivos não são MP4 válidos.' : ''}`
    : invalid ? 'Escolha arquivos .mp4 com conteúdo.' : 'Esses clipes já estão na biblioteca.');
}
const dropZone = $('#drop-zone');
dropZone.addEventListener('dragover', event => { event.preventDefault(); dropZone.classList.add('dragging'); });
dropZone.addEventListener('dragleave', event => { if (!dropZone.contains(event.relatedTarget)) dropZone.classList.remove('dragging'); });
dropZone.addEventListener('drop', event => { event.preventDefault(); dropZone.classList.remove('dragging'); importClips(event.dataTransfer.files); });
// Prevent dropped media outside the drop zone from navigating away from the app.
window.addEventListener('dragover', event => { event.preventDefault(); });
window.addEventListener('drop', event => { event.preventDefault(); });
$('#clip-search').addEventListener('input', renderLibrary);
$('#clip-sort').addEventListener('change', renderLibrary);

function cardFor(clip) {
  const card = element('article', 'clip-card'); card.dataset.clipId = String(clip.id);
  const preview = element('button', 'clip-preview'); preview.setAttribute('aria-label', `Reproduzir ${clip.name}`);
  const video = element('video'); video.src = clip.url; video.preload = 'metadata'; video.muted = true; video.tabIndex = -1; video.setAttribute('aria-hidden', 'true');
  const overlay = element('span', 'clip-overlay'); overlay.append(icon('play'));
  const duration = element('span', 'clip-duration', formatDuration(clip.duration));
  video.addEventListener('loadedmetadata', () => { clip.duration = video.duration; duration.textContent = formatDuration(video.duration); });
  video.addEventListener('error', () => { duration.textContent = 'Não reproduzível'; });
  preview.append(video, overlay, duration); preview.addEventListener('click', () => openClip(clip));
  const body = element('div', 'clip-body'); const title = element('h3', '', clip.name); title.title = clip.name;
  const meta = element('div', 'clip-meta'); meta.append(element('span', '', formatDate(clip.modified)), element('span', '', '·'), element('span', '', formatSize(clip.size)));
  const remove = element('button', 'icon-button'); remove.setAttribute('aria-label', `Retirar ${clip.name} da lista (mantém o arquivo)`); remove.title = 'Retirar da lista; mantém o arquivo'; remove.append(icon('close'));
  remove.addEventListener('click', () => {
    const index = clips.findIndex(item => item.id === clip.id);
    if (index >= 0) { clips.splice(index, 1); renderClips(); renderHomeStats(); URL.revokeObjectURL(clip.url); notify('Clipe retirado da lista. O arquivo original foi mantido.'); }
  });
  meta.append(remove); body.append(title, meta); card.append(preview, body); return card;
}
function clearPreviews() {
  $('#library-grid').querySelectorAll('video').forEach(video => { video.removeAttribute('src'); video.load(); });
}
function renderLibrary() {
  const query = $('#clip-search').value.trim().toLocaleLowerCase('pt-BR');
  const filtered = clips.filter(clip => clip.name.toLocaleLowerCase('pt-BR').includes(query));
  const sort = $('#clip-sort').value;
  filtered.sort((a, b) => sort === 'name' ? a.name.localeCompare(b.name, 'pt-BR') : sort === 'size' ? b.size - a.size : b.modified - a.modified || b.id - a.id);
  $('#library-count').textContent = `${filtered.length}${query ? ` de ${clips.length}` : ''} clipe${filtered.length === 1 ? '' : 's'}`;
  clearPreviews();
  $('#library-grid').replaceChildren(...(filtered.length ? filtered.map(cardFor) : [emptyState(query ? 'Nenhum clipe encontrado' : 'Sua biblioteca começa com uma boa jogada.', query ? 'Tente buscar por outro nome.' : 'Escolha um MP4 salvo pelo DuoClip ou arraste o arquivo aqui.', !query)]));
}
function renderClips() {
  renderLibrary();
  const recent = [...clips].sort((a, b) => b.modified - a.modified || b.id - a.id).slice(0, 3);
  if (!recent.length) { $('#recent-clips').replaceChildren(emptyState('O primeiro highlight vem aí.', 'Adicione um clipe para começar sua coleção.')); return; }
  const list = element('div', 'recent-list');
  for (const clip of recent) {
    const row = element('div', 'recent-row'), button = element('button'), thumb = element('span', 'recent-thumb'), text = element('span', 'recent-text');
    thumb.append(icon('play')); text.append(element('strong', '', clip.name), element('small', '', formatDate(clip.modified)));
    button.append(thumb, text); button.addEventListener('click', () => openClip(clip)); row.append(button, element('span', 'recent-size', formatSize(clip.size))); list.append(row);
  }
  $('#recent-clips').replaceChildren(list);
}
function openClip(clip) {
  $('#player-title').textContent = clip.name;
  $('#player-details').textContent = `${formatDate(clip.modified)} · ${formatSize(clip.size)} · Arquivo local`;
  const player = $('#clip-player'); player.src = clip.url;
  $('#player-dialog').showModal();
  player.play().catch(() => { /* Native controls remain available if autoplay is denied. */ });
}
$('#clip-player').addEventListener('error', () => { $('#player-details').textContent = 'Não foi possível reproduzir este arquivo. Confira se é um MP4 válido e se o codec é compatível.'; });
$('#close-player').addEventListener('click', () => $('#player-dialog').close());
$('#player-dialog').addEventListener('close', () => { const player = $('#clip-player'); player.pause(); player.removeAttribute('src'); player.load(); });
window.addEventListener('hashchange', navigate);
window.addEventListener('pagehide', event => { if (!event.persisted) clips.forEach(clip => URL.revokeObjectURL(clip.url)); });
populate(savedConfig); renderHomeStats(); renderClips(); navigate();
