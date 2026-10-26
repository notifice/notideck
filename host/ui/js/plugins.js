// SPDX-License-Identifier: GPL-3.0-or-later
// Plugins tab: list, permissions, enable/approve/restart/uninstall, install from .zip, log viewer.
// Also the shared GET /api/plugins cache that the inspector reads (docs/PLUGINS.md).

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { iconView } from './iconview.js';
import { onTab } from './shell.js';
import { setPluginLabeler } from './model.js';

let st = null;        // last GET /api/plugins
let failed = false;   // the agent is not answering
let missing = false;  // the agent has no plugin routes (404)
let timer = 0;
let fetchedAt = 0;
let busy = false;
const watchers = new Set();

const root = () => $('#view-plugins');

// ---- shared info for the inspector ----

/** Latest list of plugins, or null when unknown / the agent has no plugin support. */
export const pluginsInfo = () => (missing || !st || st.available === false ? null : st.plugins || []);
/** False once the agent said it has no plugin support. Unknown (not fetched yet) counts as true. */
export const pluginsAvailable = () => !missing && !(st && st.available === false);

/** Fetch (at most every 4 s unless forced) and tell the watchers. Resolves with the list or null. */
export async function refreshPlugins(force = false) {
  if (!force && Date.now() - fetchedAt < 4000 && (st || missing)) return pluginsInfo();
  fetchedAt = Date.now();
  try {
    st = await api.plugins();
    missing = false;
    failed = false;
  } catch (e) {
    if (e.status === 404) { missing = true; failed = false; } else failed = true;
  }
  watchers.forEach((f) => f());
  return pluginsInfo();
}

/** Subscribe to list refreshes; returns an unsubscribe. */
export function watchPlugins(fn) {
  watchers.add(fn);
  return () => watchers.delete(fn);
}

export const findPlugin = (id) => (pluginsInfo() || []).find((p) => p.id === id) || null;
export const pluginUsable = (p) => p.status === 'running' || p.status === 'starting';

/** "<Plugin name>: <Action name>", falling back to the stored ids while the plugin is unknown. */
export function pluginActionLabel(pluginId, actionId) {
  const p = findPlugin(pluginId);
  const a = p?.actions.find((x) => x.id === actionId);
  return p && a ? `${p.name}: ${a.name}` : `${pluginId}: ${actionId}`;
}

setPluginLabeler((a) => pluginActionLabel(a.plugin, a.action));

/** The icon for a plugin or one of its actions: an <img> from the plugin folder, else the plug glyph. */
export function pluginIcon(p, path, cls = 'plg-ico') {
  const box = h('span', { class: cls, 'aria-hidden': 'true' });
  const fallback = () => box.replaceChildren(icon('plug'));
  const src = path || p.icon;
  if (!src) fallback();
  else {
    const v = iconView(api.pluginFileUrl(p.id, src));
    v.addEventListener?.('error', fallback);
    box.append(v);
  }
  return box;
}

// ---- text ----

const STATUS = {
  running: ['Running', 'ok', 'Working normally.'],
  starting: ['Starting', '', 'Starting up. This takes a few seconds.'],
  disabled: ['Turned off', '', 'Turned off. Its keys do nothing until you turn it on.'],
  needs_approval: ['Needs your approval', 'warn', 'Review what it can do, then approve it to let it run.'],
  crashed: ['Stopped after crashing', 'warn', 'It crashed too many times, so Notideck stopped restarting it.'],
  invalid: ['Invalid plugin', 'warn', 'Its manifest is missing or broken, so Notideck cannot use it.'],
  unsupported: ['Not supported here', '', "This plugin has no version for this computer's system."],
};
export const statusText = (s) => (STATUS[s] || [s, '', ''])[0];

const PERMS = {
  network: 'Uses the internet',
  exec: 'Runs other programs',
  filesystem: 'Reads and writes your files',
};
const permText = (p) => PERMS[p] || p;

// ---- dialogs ----

let dlg = null;

function closeDialog() {
  dlg?.close();
  dlg?.remove();
  dlg = null;
}

function openDialog(id, title, body, footer, cls = '') {
  closeDialog();
  dlg = h('dialog', { class: `modal plg-modal ${cls}`.trim(), 'aria-labelledby': `${id}-title` },
    h('header', null, h('h2', { id: `${id}-title` }, title),
      h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Close', onclick: closeDialog }, icon('close', 'ico-svg sm'))),
    body,
    h('footer', { class: 'plg-foot' }, footer));
  dlg.addEventListener('cancel', (e) => { e.preventDefault(); closeDialog(); });
  document.body.append(dlg);
  dlg.showModal();
  return dlg;
}

function permList(p) {
  return p.permissions.length
    ? h('ul', { class: 'plg-perms' }, p.permissions.map((x) => h('li', null, icon('warn', 'ico-svg sm'), permText(x))))
    : h('p', { class: 'muted' }, 'It asks for no special permissions.');
}

export function approveDialog(p) {
  const go = h('button', { type: 'button', class: 'btn primary', id: 'plg-approve-go', onclick: async () => {
    closeDialog();
    await run(p, 'approve', `${p.name} approved.`);
  } }, 'Approve and run');
  openDialog('plg-approve', `Approve ${p.name}?`,
    h('div', { class: 'stack' },
      h('p', null, `${p.name}${p.version ? ` ${p.version}` : ''}${p.author ? ` by ${p.author}` : ''} wants to:`),
      permList(p),
      h('p', { class: 'warn' }, "Plugins run with your user's rights and are not sandboxed. These permissions describe what the plugin says it does, they are not enforced. Only approve plugins you trust.")),
    [h('button', { type: 'button', class: 'btn', onclick: closeDialog }, 'Cancel'), go]);
  go.focus();
}

function confirmDialog(id, title, text, label, onYes) {
  const go = h('button', { type: 'button', class: 'btn danger', id: `${id}-go`, onclick: () => { closeDialog(); onYes(); } }, label);
  openDialog(id, title, h('p', null, text), [h('button', { type: 'button', class: 'btn', onclick: closeDialog }, 'Cancel'), go]);
}

function logDialog(p) {
  const pre = h('pre', { class: 'plg-log', id: 'plg-log', tabindex: '0' }, 'Loading…');
  const load = async () => {
    try {
      const r = await api.pluginLog(p.id);
      const lines = r?.lines || [];
      pre.textContent = lines.length ? lines.join('\n') : 'The log is empty.';
      pre.scrollTop = pre.scrollHeight;
    } catch (e) {
      pre.textContent = `Could not read the log: ${e.message}`;
    }
  };
  openDialog('plg-log-dlg', `${p.name} log`, pre,
    [h('button', { type: 'button', class: 'btn', id: 'plg-log-refresh', onclick: load }, 'Refresh'),
      h('button', { type: 'button', class: 'btn', onclick: closeDialog }, 'Close')], 'plg-wide');
  load();
}

// ---- commands ----

async function run(p, cmd, okMsg) {
  if (busy) return;
  busy = true;
  try {
    if (cmd === 'uninstall') await api.pluginUninstall(p.id);
    else await api.pluginCmd(p.id, cmd);
    if (okMsg) toast(okMsg);
  } catch (e) {
    toast(`${p.name}: ${e.message}`, 'error');
  }
  busy = false;
  await refreshPlugins(true);
  render(true);
}

async function install(file) {
  if (busy || !file) return;
  busy = true;
  render(true);
  try {
    const p = await api.pluginInstall(file);
    busy = false;
    await refreshPlugins(true);
    render(true);
    if (p && p.status === 'needs_approval') approveDialog(p);
    else toast(`Installed ${p?.name || 'the plugin'}.`);
  } catch (e) {
    busy = false;
    toast(e.status === 400 ? e.message : `Install failed: ${e.message}`, 'error');
    render(true);
  }
}

async function reload() {
  if (busy) return;
  busy = true;
  try {
    await api.pluginsReload();
    toast('Plugins reloaded.');
  } catch (e) {
    toast(`Reload failed: ${e.message}`, 'error');
  }
  busy = false;
  await refreshPlugins(true);
  render(true);
}

async function openFolder() {
  try { await api.open('plugins'); } catch (e) { toast(`Could not open the folder: ${e.message}`, 'error'); }
}

// ---- view ----

function buttons(p) {
  const btn = (label, onclick, cls = 'btn sm', id = null) => h('button', { type: 'button', class: cls, id, disabled: busy, onclick }, label);
  const out = [];
  switch (p.status) {
    case 'running':
      out.push(btn('Disable', () => run(p, 'disable'), 'btn sm', `plg-disable-${p.id}`), btn('Restart', () => run(p, 'restart', 'Restarting…'), 'btn sm', `plg-restart-${p.id}`));
      break;
    case 'starting':
      out.push(btn('Disable', () => run(p, 'disable'), 'btn sm', `plg-disable-${p.id}`));
      break;
    case 'disabled':
      out.push(btn('Enable', () => run(p, 'enable'), 'btn sm primary', `plg-enable-${p.id}`));
      break;
    case 'needs_approval':
      out.push(btn('Approve…', () => approveDialog(p), 'btn sm primary', `plg-approve-${p.id}`));
      break;
    case 'crashed':
      out.push(btn('Restart', () => run(p, 'restart', 'Restarting…'), 'btn sm primary', `plg-restart-${p.id}`), btn('Disable', () => run(p, 'disable'), 'btn sm', `plg-disable-${p.id}`));
      break;
    default:
  }
  out.push(btn('Show log', () => logDialog(p), 'btn sm', `plg-logbtn-${p.id}`));
  if (!p.bundled) {
    out.push(btn('Uninstall', () => confirmDialog('plg-uninstall', `Uninstall ${p.name}?`,
      'This deletes the plugin from your computer. Keys that use it stay on your decks but do nothing.', 'Uninstall', () => run(p, 'uninstall', `${p.name} uninstalled.`)),
    'btn sm danger', `plg-uninstall-${p.id}`));
  }
  return h('div', { class: 'plg-btns' }, out);
}

function card(p) {
  const [label, tone, hint] = STATUS[p.status] || [p.status, '', ''];
  return h('section', { class: `card plg s-${p.status}`, id: `plg-${p.id}`, 'data-id': p.id },
    h('header', { class: 'plg-head' },
      pluginIcon(p, null, 'plg-ico big'),
      h('div', { class: 'plg-title' },
        h('b', null, p.name, p.bundled ? h('span', { class: 'chip plg-tag' }, 'Bundled') : null),
        h('small', null, [p.version ? `Version ${p.version}` : null, p.author ? `by ${p.author}` : null].filter(Boolean).join(' · '))),
      h('span', { class: `chip plg-status ${tone}`, title: hint }, h('span', { class: `dot ${tone}`, 'aria-hidden': 'true' }), label)),
    p.description ? h('p', { class: 'plg-desc' }, p.description) : null,
    p.error && p.status !== 'running' ? h('p', { class: 'warn plg-err' }, p.error) : (hint && tone === 'warn' ? h('p', { class: 'muted obs-small' }, hint) : null),
    p.permissions.length ? h('ul', { class: 'plg-chips', 'aria-label': 'Permissions' }, p.permissions.map((x) => h('li', { class: 'chip plg-perm' }, permText(x)))) : null,
    p.actions.length ? h('div', { class: 'plg-acts' },
      h('small', { class: 'muted' }, `Provides ${p.actions.length} action${p.actions.length === 1 ? '' : 's'}`),
      h('ul', { class: 'plg-actlist' }, p.actions.map((a) => h('li', { title: a.description || null }, pluginIcon(p, a.icon, 'plg-ico sm'), h('span', null, a.name))))) : null,
    buttons(p));
}

function header() {
  const file = h('input', { type: 'file', accept: '.zip,application/zip', hidden: true, id: 'plg-file', onchange: (e) => {
    const f = e.target.files[0];
    e.target.value = '';
    install(f);
  } });
  const avail = !!st && !missing && st.available !== false;
  return h('header', { class: 'page-head plg-pagehead' },
    h('div', null, h('h1', null, 'Plugins'),
      h('small', { class: 'muted' }, 'Plugins add keys and dials made by other people: clocks, web requests, smart-home and more.')),
    avail ? h('div', { class: 'plg-top' },
      h('button', { type: 'button', class: 'btn primary', id: 'plg-install', disabled: busy, onclick: () => file.click() }, busy ? 'Working…' : 'Install from .zip…'),
      h('button', { type: 'button', class: 'btn', id: 'plg-folder', onclick: openFolder }, 'Open plugins folder'),
      h('button', { type: 'button', class: 'btn', id: 'plg-reload', disabled: busy, onclick: reload }, 'Reload'),
      file) : null);
}

let sig = '';

export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  const s = JSON.stringify([st, failed, missing, busy]);
  if (!force && s === sig) return;
  sig = s;
  const focusId = el.contains(document.activeElement) ? document.activeElement.id : '';
  const top = el.scrollTop;
  if (!st && !missing) {
    el.replaceChildren(header(), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…'));
    return;
  }
  if (missing || st.available === false) {
    el.replaceChildren(header(),
      h('section', { class: 'card obs-unavailable', id: 'plg-unavailable' },
        h('b', null, 'This build has no plugin support'),
        h('p', { class: 'muted' }, 'This version of the Notideck agent was built without plugins, so there is nothing to manage here. Everything else works as usual.')));
    return;
  }
  const list = st.plugins || [];
  el.replaceChildren(...[header(),
    failed ? h('p', { class: 'warn' }, 'The agent is not answering. Showing the last known state.') : null,
    list.length ? h('div', { class: 'plg-list' }, list.map(card))
      : h('section', { class: 'card empty', id: 'plg-empty' },
        h('span', { class: 'dico big' }, icon('plug')),
        h('b', null, 'No plugins installed'),
        h('p', { class: 'muted' }, 'Install one from a .zip, or copy its folder into the plugins folder and press Reload.'))].filter(Boolean));
  el.scrollTop = top;
  if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
}

async function load() {
  await refreshPlugins(true);
  render();
}

export function startPlugins() {
  stopPlugins();
  sig = '';
  root()?.replaceChildren();
  render(true);
  load();
  timer = setInterval(() => { if (!document.hidden) load(); }, 2000);
}

export function stopPlugins() {
  clearInterval(timer);
  timer = 0;
}

onTab((id) => { if (id === 'plugins') startPlugins(); else stopPlugins(); });

export const renderPlugins = render;
