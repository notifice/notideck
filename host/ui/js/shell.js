// SPDX-License-Identifier: GPL-3.0-or-later
// App shell: navigation rail, hash routing, theme, problems banner, native-shell bridge.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S, refreshStatus, refreshHost, subscribe } from './store.js';

// ---- native shell bridge ----

export const hasShell = () => !!window.__NOTIDECK_SHELL__;

export function postShell(msg) {
  try {
    if (hasShell() && window.ipc?.postMessage) window.ipc.postMessage(JSON.stringify(msg));
  } catch { /* shell gone */ }
}

export function openExternal(url) {
  if (hasShell()) postShell({ cmd: 'open_external', url });
  else window.open(url, '_blank', 'noopener');
}

// ---- theme ----

const mq = matchMedia('(prefers-color-scheme: light)');
let themePref = 'system';

export function applyTheme(pref, notify = true) {
  themePref = pref === 'dark' || pref === 'light' ? pref : 'system';
  const dark = themePref === 'dark' || (themePref === 'system' && !mq.matches);
  document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
  try { localStorage.setItem('nd-theme', themePref); } catch { /* storage blocked */ }
  if (notify) postShell({ cmd: 'theme', value: themePref });
}
mq.addEventListener('change', () => { if (themePref === 'system') applyTheme('system', false); });

const PHONE_THEMES = ['black', 'graphite', 'cream', 'lavender', 'mint', 'dusk', 'tide'];
export function applyPhoneTheme(v) {
  document.documentElement.setAttribute('data-phone', PHONE_THEMES.includes(v) ? v : 'black');
}

// ---- routing and navigation ----

export const TABS = [
  { id: 'deck', label: 'Deck' },
  { id: 'devices', label: 'Devices' },
  { id: 'camera', label: 'Camera' },
  { id: 'mic', label: 'Microphone' },
  { id: 'mixer', label: 'Mixer' },
  { id: 'obs', label: 'OBS' },
  { id: 'autoswitch', label: 'Auto-switch' },
  { id: 'plugins', label: 'Plugins', glyph: 'plug' },
  { id: 'settings', label: 'Settings' },
];

export let tab = 'deck';
const listeners = new Set();
export const onTab = (fn) => listeners.add(fn);

function fromHash() {
  const id = location.hash.replace(/^#/, '');
  return TABS.some((t) => t.id === id) ? id : 'deck';
}

function show(id) {
  tab = id;
  for (const t of TABS) $(`#view-${t.id}`).hidden = t.id !== id;
  renderNav();
  listeners.forEach((f) => f(id));
}

export function go(id) {
  if (location.hash === `#${id}`) show(id);
  else location.hash = id; // hashchange does the rest
}

export function initRouting() {
  addEventListener('hashchange', () => show(fromHash()));
  show(fromHash());
}

function renderNav() {
  const nav = $('#nav');
  const hadFocus = nav.contains(document.activeElement);
  nav.replaceChildren(...TABS.map((t) => h('li', null,
    h('a', {
      href: `#${t.id}`, class: 'navitem', 'aria-current': t.id === tab ? 'page' : null,
      tabindex: t.id === tab ? 0 : -1, dataset: { tab: t.id },
    }, icon(t.glyph || t.id), h('span', null, t.label)))));
  if (hadFocus) nav.querySelector('[aria-current]')?.focus({ preventScroll: true });
}

function bindNavKeys() {
  $('#nav').addEventListener('keydown', (e) => {
    const dir = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[e.key];
    const items = [...$('#nav').querySelectorAll('a')];
    let i = items.indexOf(document.activeElement);
    if (e.key === 'Home') i = 0;
    else if (e.key === 'End') i = items.length - 1;
    else if (dir && i >= 0) i = (i + dir + items.length) % items.length;
    else return;
    e.preventDefault();
    items.forEach((a, n) => { a.tabIndex = n === i ? 0 : -1; });
    items[i].focus();
  });
}

// ---- connection indicator ----

export function renderConn() {
  const box = $('#conn');
  const dev = (S.status?.devices ?? []).find((d) => d.connected);
  const sig = dev ? `${dev.id}|${dev.name}|${dev.transport}` : S.status ? 'none' : 'offline';
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  box.classList.toggle('on', !!dev);
  box.replaceChildren(
    h('span', { class: 'dot', 'aria-hidden': 'true' }),
    h('span', { class: 'conn-text' },
      h('b', null, dev ? (dev.name || dev.id) : S.status ? 'No phone' : 'Agent offline'),
      dev ? h('small', null, dev.transport === 'lan' ? 'Wi-Fi (LAN)' : 'USB') : null));
}

// ---- problems banner ----

const dismissed = new Set();
const fixing = new Set();
let probSig = '';

export function renderProblems(force = false) {
  const list = (S.status?.problems ?? []).filter((p) => !dismissed.has(p.code));
  const sig = JSON.stringify([list, [...fixing]]);
  if (!force && sig === probSig) return;
  probSig = sig;
  $('#problems').replaceChildren(...list.map((p) => h('div', { class: 'problem', role: 'alert' },
    icon('warn'),
    h('p', null, p.message),
    p.fix ? fixButton(p.fix, 'Fix…', 'btn sm primary') : null,
    h('button', { type: 'button', class: 'iconbtn', 'aria-label': 'Dismiss', title: 'Dismiss for this session', onclick: () => { dismissed.add(p.code); renderProblems(); } }, icon('close')))));
}

/** A button that runs POST /api/fix/:id with a pending state (the fix can take a while). */
export function fixButton(fixId, label, cls = 'btn') {
  const busy = fixing.has(fixId);
  return h('button', {
    type: 'button', class: cls, disabled: busy, 'aria-busy': busy ? 'true' : null,
    onclick: async () => {
      fixing.add(fixId);
      renderProblems(true);
      rerenderAll();
      try {
        await api.fix(fixId);
        toast('Done. Re-checking…');
      } catch (e) {
        toast(`Fix failed: ${e.message}`, 'error');
      }
      fixing.delete(fixId);
      await Promise.all([refreshStatus(), refreshHost()]);
      renderProblems(true);
      rerenderAll();
    },
  }, busy ? 'Working…' : label);
}

const rerenderers = new Set();
export const onRerender = (fn) => rerenderers.add(fn);
function rerenderAll() { rerenderers.forEach((f) => f()); }

export function initShell() {
  bindNavKeys();
  subscribe(() => { renderConn(); renderProblems(); });
}
