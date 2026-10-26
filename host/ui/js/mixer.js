// SPDX-License-Identifier: GPL-3.0-or-later
// Mixer tab: per-app volume, mute, hide and pin order. Polls GET /api/mixer while visible.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';

let st = null;        // last GET /api/mixer
let failed = false;
let timer = 0;
let busy = false;
const dragging = new Set();   // keys whose slider the user is holding
const pending = new Map();    // key -> last volume sent, shown until the poll catches up
const sendAt = new Map();     // key -> timestamp of the last POST

const root = () => $('#view-mixer');
const pct = (v) => `${Math.round((v || 0) * 100)}`;

function row(label, hint, control) {
  return h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, label), hint ? h('small', null, hint) : null), control);
}

function postApp(body) {
  return api.mixerApp(body).catch((e) => { if (e.message !== '404') toast(`Mixer: ${e.message}`, 'error'); });
}

function sendVolume(key, v, force) {
  const now = Date.now();
  if (!force && now - (sendAt.get(key) || 0) < 100) return;
  sendAt.set(key, now);
  postApp({ key, volume: v });
}

async function putConfig(cfg) {
  if (busy) return;
  busy = true;
  try {
    await api.saveMixerConfig(cfg);
    st = { ...st, config: cfg };
  } catch (e) {
    toast(`Could not save the mixer settings: ${e.message}`, 'error');
  }
  busy = false;
  await load();
}

const cfgCopy = () => ({ order: [...(st.config?.order ?? [])], hidden: [...(st.config?.hidden ?? [])], show_system: !!st.config?.show_system });

function toggleHidden(key) {
  const cfg = cfgCopy();
  cfg.hidden = cfg.hidden.includes(key) ? cfg.hidden.filter((k) => k !== key) : [...cfg.hidden, key];
  return putConfig(cfg);
}

/** Reordering pins every visible app in its on-screen order, then swaps the clicked one with its neighbour. */
function moveApp(key, d) {
  const cfg = cfgCopy();
  cfg.order = st.apps.filter((a) => !a.hidden).map((a) => a.key);
  const i = cfg.order.indexOf(key);
  const j = i + d;
  if (i < 0 || j < 0 || j >= cfg.order.length) return;
  [cfg.order[i], cfg.order[j]] = [cfg.order[j], cfg.order[i]];
  return putConfig(cfg);
}

function appRow(a, i, visibleCount) {
  const key = a.key;
  const shown = pending.has(key) ? pending.get(key) : a.volume;
  const out = h('output', null, pct(shown));
  const slider = h('input', {
    type: 'range', min: 0, max: 100, step: 1, value: Math.round((shown || 0) * 100), 'aria-label': `Volume for ${a.display_name}`,
    dataset: { key },
    onpointerdown: () => dragging.add(key),
    oninput: (e) => {
      const v = e.target.value / 100;
      out.textContent = e.target.value;
      pending.set(key, v);
      dragging.add(key);
      sendVolume(key, v, false);
    },
    onchange: (e) => {
      const v = e.target.value / 100;
      sendVolume(key, v, true);
      dragging.delete(key);
      setTimeout(() => pending.delete(key), 1500);
    },
  });
  const addEnd = () => { if (dragging.has(key)) { dragging.delete(key); setTimeout(() => pending.delete(key), 1500); } };
  slider.addEventListener('pointerup', addEnd);
  slider.addEventListener('pointercancel', addEnd);

  const pos = a.hidden ? -1 : i;
  const pinnedNow = (st.config?.order ?? []).includes(key);
  return h('li', { class: `mx-app${a.hidden ? ' hidden-app' : ''}`, dataset: { key } },
    h('div', { class: 'mx-ico', 'aria-hidden': 'true' },
      a.icon_hash ? h('img', { src: api.iconUrl(a.icon_hash), alt: '', draggable: 'false' }) : (a.display_name || '?').trim().slice(0, 1).toUpperCase()),
    h('div', { class: 'mx-name' },
      h('b', null, h('span', { class: `dot${a.active ? ' ok' : ''}`, title: a.active ? 'Playing' : 'Idle' }), h('span', null, a.display_name || a.key)),
      h('small', null, [a.is_system ? 'System sounds' : a.key, a.active ? 'playing' : 'idle'].join(' · '))),
    h('div', { class: 'mx-vol' }, slider, out),
    h('div', { class: 'mx-tools' },
      a.slot != null ? h('span', { class: 'mx-badge' }, `Slot ${a.slot + 1}`) : null,
      h('button', { type: 'button', class: 'btn sm', 'aria-pressed': a.muted ? 'true' : 'false', onclick: () => { postApp({ key, muted: !a.muted }).then(load); } }, a.muted ? 'Muted' : 'Mute')),
    h('div', { class: 'mx-tools' },
      pos >= 0 ? h('button', { type: 'button', class: 'btn sm', 'aria-label': `Move ${a.display_name} up`, disabled: i === 0 || busy, onclick: () => moveApp(key, -1) }, '↑') : null,
      pos >= 0 ? h('button', { type: 'button', class: 'btn sm', 'aria-label': `Move ${a.display_name} down`, disabled: i === visibleCount - 1 || busy, onclick: () => moveApp(key, 1) }, '↓') : null,
      pinnedNow ? h('span', { class: 'mx-badge', title: 'Pinned in config order' }, 'Pinned') : null,
      h('button', { type: 'button', class: 'btn sm', 'aria-pressed': a.hidden ? 'true' : 'false', onclick: () => toggleHidden(key) }, a.hidden ? 'Show' : 'Hide')));
}

function build() {
  const apps = st.apps ?? [];
  const visible = apps.filter((a) => !a.hidden);
  const hiddenApps = apps.filter((a) => a.hidden);
  const notes = [];
  if (!st.available) {
    notes.push(h('div', { class: 'as-banner', role: 'status' }, icon('warn'),
      h('p', null, h('b', null, "The mixer isn't available on this system"),
        h('small', null, `Backend: ${st.backend || 'none'}${st.error ? ` · ${st.error}` : ''}`))));
  }
  return [
    h('header', { class: 'page-head' }, h('h1', null, 'Mixer'),
      h('small', { class: 'muted' }, `Per-app volume and mute. Apps fill the mixer slots in the order shown${st.backend ? ` · Backend: ${st.backend}` : ''}.`)),
    ...notes,
    st.available && !st.has_peaks ? h('p', { class: 'muted', id: 'mx-nopeaks', style: 'padding: 0 0 12px' }, "Level meters aren't available on this system.") : null,
    h('section', { class: 'card sect' },
      h('h2', null, 'General'),
      row('Show system sounds', 'Include the operating system’s own sounds as a mixer app.',
        h('button', { id: 'mx-system', type: 'button', class: 'switch', role: 'switch', 'aria-checked': st.config?.show_system ? 'true' : 'false', disabled: busy,
          onclick: () => { const cfg = cfgCopy(); cfg.show_system = !cfg.show_system; putConfig(cfg); } }, h('i')))),
    h('section', { class: 'card sect' },
      h('h2', null, 'Apps'),
      visible.length ? h('ul', { class: 'mx-list', id: 'mx-list' }, visible.map((a, i) => appRow(a, i, visible.length)))
        : h('p', { class: 'muted', style: 'padding: 10px 0 14px' }, st.available ? 'No apps are playing sound. Start some audio and it will show up here.' : 'Nothing to show.')),
    hiddenApps.length ? h('section', { class: 'card sect' },
      h('h2', null, 'Hidden'),
      h('ul', { class: 'mx-list', id: 'mx-hidden' }, hiddenApps.map((a, i) => appRow(a, i, hiddenApps.length)))) : null,
  ].filter(Boolean);
}

let sig = '';
export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  if (!st) {
    el.replaceChildren(h('header', { class: 'page-head' }, h('h1', null, 'Mixer')), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…'));
    return;
  }
  // Structure signature ignores volume/mute of rows being dragged; those update in place.
  const next = JSON.stringify([st.available, st.backend, st.error, st.has_peaks, st.config, st.apps.map((a) => [a.key, a.display_name, a.hidden, a.slot, a.active, a.muted, a.icon_hash])]);
  if (force || next !== sig) {
    if (dragging.size && !force) { updateLive(); return; }
    sig = next;
    const focusId = el.contains(document.activeElement) ? document.activeElement.id : '';
    el.replaceChildren(...build());
    if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  }
  updateLive();
}

/** Refresh slider positions without rebuilding, skipping any slider being dragged. */
function updateLive() {
  for (const a of st?.apps ?? []) {
    if (dragging.has(a.key) || pending.has(a.key)) continue;
    const li = root().querySelector(`.mx-app[data-key="${CSS.escape(a.key)}"]`);
    if (!li) continue;
    const r = li.querySelector('input[type=range]');
    if (r && document.activeElement !== r) r.value = Math.round((a.volume || 0) * 100);
    const o = li.querySelector('output');
    if (o) o.textContent = pct(a.volume);
  }
}

async function load() {
  try {
    st = await api.mixer();
    failed = false;
  } catch {
    failed = true;
  }
  render();
}

export function startMixer() {
  stopMixer();
  sig = '';
  load();
  timer = setInterval(() => { if (!document.hidden) load(); }, 1000);
}

export function stopMixer() {
  clearInterval(timer);
  timer = 0;
  dragging.clear();
  pending.clear();
}

export const renderMixer = render;
