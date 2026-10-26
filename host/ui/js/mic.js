// SPDX-License-Identifier: GPL-3.0-or-later
// Microphone tab: the phone as a microphone. Status + level, setup help, Discord hint, output/processing/gain settings.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S } from './store.js';
import { openExternal } from './shell.js';

const CABLE_URL = 'https://vb-audio.com/Cable/';

let mic = null;        // last GET /api/mic
let sig = '';
let timer = 0;
let failed = false;
let busy = false;      // a request is in flight; skip overlapping polls

const root = () => $('#view-mic');
const linkLabel = (l) => (l === 'lan' ? 'Wi-Fi' : 'USB');
const micSettings = () => ({
  device: S.settings?.mic_device ?? '',
  voice: S.settings?.mic_voice_processing ?? true,
  gain: S.settings?.mic_gain_db ?? 0,
  guard: S.settings?.mic_guard_defaults ?? true,
});
const isWin = (m) => m?.platform === 'windows';
const supported = (m) => !!m && m.state !== 'unsupported';

export function statusText(m) {
  if (!m) return 'Checking the microphone…';
  if (m.state === 'unsupported') return m.message || 'The phone microphone is not available in this agent.';
  if (m.message) return m.message;
  switch (m.state) {
    case 'off': return 'The phone microphone is off';
    case 'waiting': return 'Waiting for a phone that can send its microphone';
    case 'starting': return 'Starting the phone microphone…';
    case 'live': return 'The phone microphone is live';
    case 'no_output': return isWin(m) ? 'Install the free VB-Cable to use the phone as a microphone' : 'No audio output found';
    case 'no_permission': return 'Allow microphone access on the phone';
    default: return 'The phone microphone reported an error';
  }
}

const dotClass = (m) => (!m ? '' : m.state === 'live' ? 'ok' : m.state === 'error' || m.state === 'no_output' || m.state === 'no_permission' ? 'warn' : '');

async function patch(p) {
  try {
    S.settings = { ...S.settings, ...p };
    const saved = await api.saveSettings(p);
    if (saved && typeof saved === 'object' && 'theme' in saved) S.settings = saved;
  } catch (e) {
    toast(`Could not save: ${e.message}`, 'error');
    try { S.settings = await api.settings(); } catch { /* ignore */ }
  }
  await load();
}

async function control(c) {
  try { mic = await api.micControl(c); failed = false; } catch (e) { toast(e.message, 'error'); }
  render(true);
}

function row(label, hint, control_) {
  return h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, label), hint ? h('small', null, hint) : null), control_);
}

function switchBtn(id, on, onclick, disabled = false) {
  return h('button', { id, type: 'button', class: 'switch', role: 'switch', 'aria-checked': on ? 'true' : 'false', disabled, onclick }, h('i'));
}

function extLink(url, text) {
  return h('a', { href: url, class: 'link', onclick: (e) => { e.preventDefault(); openExternal(url); } }, text, icon('external', 'ico-svg sm'));
}

function phoneLine(m) {
  const p = m.phone;
  if (!p) return 'No phone connected';
  const who = `${p.name || 'Phone'} · ${linkLabel(p.link)}`;
  if (p.supported === false) return `${who} · this phone app version cannot send its microphone`;
  if (p.state === 'no_permission' || m.state === 'no_permission') return `${who} · Allow microphone access on the phone (Notideck asks when the app is open)`;
  return who;
}

function setupCard(m) {
  if (!(isWin(m) && m.cable_found === false)) return null;
  return h('section', { class: 'card mic-setup' },
    h('h2', null, 'One-time setup: VB-Cable'),
    h('p', { class: 'muted' }, 'Windows only lets a signed driver add a microphone, so Notideck plays the phone mic into a free virtual cable. It is just one cable, not a mixer, and it does not change your other audio settings.'),
    h('ol', { class: 'mic-steps' },
      h('li', null, 'Download VB-Cable and unzip it.'),
      h('li', null, 'Run ', h('code', null, 'VBCABLE_Setup_x64.exe'), ' as administrator.'),
      h('li', null, 'Restart Windows if the installer asks.')),
    h('p', { class: 'muted' }, 'Notideck finds the cable by itself, no restart of Notideck needed.'),
    h('div', null, h('button', { id: 'mic-download', type: 'button', class: 'btn primary', onclick: () => openExternal(CABLE_URL) }, 'Download VB-Cable', icon('external', 'ico-svg sm'))));
}

function useCard(m) {
  if (!m.output) return null;
  const win = isWin(m);
  const name = m.output.capture_name || (win ? 'CABLE Output (VB-Audio Virtual Cable)' : 'Notideck Microphone');
  return h('section', { class: 'card sect mic-use' },
    h('h2', null, 'Use it in Discord'),
    h('p', { class: 'muted' }, 'Discord → User Settings → Voice & Video → Input Device → ', h('b', null, name)),
    h('p', { class: 'muted' }, `In OBS, Zoom or any other app, pick ${win ? 'the same device' : '“Notideck Microphone”'} as its microphone.`));
}

function deviceOptions(m, cur) {
  const opts = [['', isWin(m) ? 'Auto (first virtual cable)' : 'Auto']];
  for (const d of m.devices ?? []) opts.push([d.id, d.name]);
  if (cur && !opts.some(([v]) => v === cur)) opts.push([cur, 'Saved device (not found)']);
  return opts;
}

function gainRow(set) {
  const fmt = (v) => `${v > 0 ? '+' : ''}${v} dB`;
  const out = h('output', { id: 'mic-gain-out' }, fmt(set.gain));
  const range = h('input', {
    id: 'mic-gain', type: 'range', min: -12, max: 24, step: 1, value: set.gain, 'aria-label': 'Gain in dB',
    oninput: (e) => { out.textContent = fmt(parseInt(e.target.value, 10)); },
    onchange: (e) => patch({ mic_gain_db: parseInt(e.target.value, 10) }),
  });
  return row('Gain', 'Louder or quieter, applied on the PC before the cable.', h('div', { class: 'cam-rate' }, range, out));
}

function guardInfo(m) {
  const g = m.guard;
  if (!g) return null;
  const restored = (g.restored ?? []).slice(-5).reverse();
  if (!g.warning && !restored.length) return null;
  const when = (t) => new Date(t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const flow = (f) => (f === 'capture' ? 'recording' : 'playback');
  return h('div', { class: 'mic-guard' },
    g.warning ? h('p', { class: 'warn', role: 'alert' }, g.warning) : null,
    restored.map((r) => h('small', { class: 'muted' }, `Switched ${flow(r.flow)} back from ${r.from} to ${r.to}, ${when(r.at)}`)));
}

function settingsCard(m) {
  const set = micSettings();
  const win = isWin(m);
  const sel = h('select', { id: 'mic-device', 'aria-label': 'Output device', onchange: (e) => patch({ mic_device: e.target.value }) },
    deviceOptions(m, set.device).map(([v, l]) => h('option', { value: v, selected: v === set.device }, l)));
  return h('section', { class: 'card sect' },
    h('h2', null, 'Settings'),
    row('Output device', win ? 'Where the phone sound is played. Automatic picks the first virtual cable.' : 'The virtual microphone other apps can pick.', sel),
    row('Noise suppression and echo cancellation', 'Cleans up the sound. Processed on the phone.', switchBtn('mic-voice', set.voice, () => patch({ mic_voice_processing: !set.voice }))),
    gainRow(set),
    win ? row('Keep my default speaker and mic', 'Windows sometimes makes the cable your default device when it is installed. Notideck switches your defaults back. Turn this off if you want the phone as your default mic everywhere.',
      switchBtn('mic-guard', set.guard, () => patch({ mic_guard_defaults: !set.guard }))) : null,
    win ? guardInfo(m) : null);
}

function statsText(m) {
  const s = m?.stats;
  if (!s || (m.state !== 'live' && m.state !== 'starting')) return '';
  return `buffer ${Math.round(s.buffer_ms)} ms · underruns ${s.underruns} · dropped ${Math.round(s.dropped_ms)} ms${s.kbps ? ` · ${s.kbps} kbit/s` : ''}`;
}

function build(m) {
  const head = h('header', { class: 'page-head' }, h('h1', null, 'Microphone'), h('small', { class: 'muted' }, 'Use the phone as a microphone for Discord, OBS, Zoom and more.'));
  if (!supported(m)) {
    return [head, h('section', { class: 'card cam-note' }, icon('mic'), h('p', null, statusText(m)))];
  }
  const on = !!m.on;
  return [
    head,
    h('section', { class: 'card mic-status' },
      h('div', { class: 'mic-main' },
        switchBtn('mic-on', on, () => control({ on: !on })),
        h('div', { class: 'cam-status-text' },
          h('span', { class: 'statusline' }, h('i', { id: 'mic-dot', class: `dot ${dotClass(m)}` }), h('b', { id: 'mic-status' }, statusText(m))),
          h('small', { class: 'muted' }, phoneLine(m))),
        h('button', { id: 'mic-mute', type: 'button', class: 'btn', 'aria-pressed': m.muted ? 'true' : 'false', disabled: !on, onclick: () => control({ muted: !m.muted }) },
          icon(m.muted ? 'mic-off' : 'mic', 'ico-svg sm'), m.muted ? 'Unmute' : 'Mute')),
      h('div', { class: 'mic-meter', role: 'meter', 'aria-label': 'Input level', 'aria-valuemin': 0, 'aria-valuemax': 100, id: 'mic-meter' }, h('i', { id: 'mic-level' })),
      h('small', { id: 'mic-stats', class: 'muted' }, statsText(m))),
    setupCard(m),
    useCard(m),
    settingsCard(m),
  ].filter(Boolean);
}

/** Update only the live bits, without rebuilding (keeps the slider and focus). */
function updateLive() {
  if (!mic) return;
  const set = (id, fn) => { const el = document.getElementById(id); if (el) fn(el); };
  const lvl = mic.state === 'live' && !mic.muted ? Math.max(0, Math.min(1, Number(mic.level) || 0)) : 0;
  set('mic-status', (el) => { el.textContent = statusText(mic); });
  set('mic-stats', (el) => { el.textContent = statsText(mic); });
  set('mic-dot', (el) => { el.className = `dot ${dotClass(mic)}`; });
  set('mic-level', (el) => { el.style.width = `${Math.round(lvl * 100)}%`; });
  set('mic-meter', (el) => { el.setAttribute('aria-valuenow', String(Math.round(lvl * 100))); });
}

export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  const next = JSON.stringify([
    mic?.state, mic?.platform, mic?.on, mic?.muted, mic?.phone, mic?.output, mic?.devices, mic?.cable_found, mic?.guard, micSettings(),
  ]);
  const active = document.activeElement;
  const editing = el.contains(active) && ['INPUT', 'SELECT'].includes(active.tagName) && active.type === 'range';
  if (force || (next !== sig && !editing)) {
    sig = next;
    const focusId = el.contains(active) ? active.id : '';
    el.replaceChildren(...(mic ? build(mic) : [h('header', { class: 'page-head' }, h('h1', null, 'Microphone')), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…')]));
    if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  }
  updateLive();
}

async function load() {
  if (busy) return;
  busy = true;
  try {
    const [m] = await Promise.all([api.mic(), S.settings ? null : api.settings().then((v) => { S.settings = v; })]);
    mic = m;
    failed = false;
  } catch {
    failed = true;
  } finally {
    busy = false;
  }
  render();
}

export function startMicPolling() {
  stopMicPolling();
  load();
  timer = setInterval(() => { if (!document.hidden) load(); }, 250);
}

export function stopMicPolling() {
  clearInterval(timer);
  timer = 0;
}

export const renderMic = render;
