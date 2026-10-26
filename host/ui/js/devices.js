// SPDX-License-Identifier: GPL-3.0-or-later
// Devices tab: one card per phone, connection help, adb status.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S } from './store.js';
import { tab, onTab, go } from './shell.js';

// ---- pairing state (own 2 s poll while the Devices tab is visible) ----
let pending = [];
let paired = [];
let sig = '';

export const lanOn = () => S.settings?.lan_enabled !== false;

function relTime(sec) {
  if (!sec) return 'never connected';
  const d = Math.max(0, Date.now() / 1000 - sec);
  if (d < 60) return 'just now';
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  return `${Math.floor(d / 86400)} d ago`;
}

export async function poll() {
  try {
    const [pe, pa] = await Promise.all([api.pending(), api.paired()]);
    pending = Array.isArray(pe) ? pe : [];
    paired = Array.isArray(pa) ? pa : [];
  } catch { return; }
  checkPairedGrowth();
  // relative times only need a repaint when the minute changes
  const s = JSON.stringify([pending, paired, Math.floor(Date.now() / 60000)]);
  if (s !== sig) {
    sig = s;
    if (tab === 'devices') renderDevices();
  }
}

onTab((id) => { if (id === 'devices') { if (!S.settings) api.settings().then((v) => { S.settings = v; renderDevices(); }).catch(() => {}); poll(); } });
setInterval(() => { if (!document.hidden && tab === 'devices') poll(); }, 2000);

async function resolve(p, allow) {
  try {
    await api.resolvePending(p.session_id, allow);
    toast(allow ? `${p.name || 'Phone'} allowed.` : `${p.name || 'Phone'} denied.`);
  } catch (e) { toast(`Could not ${allow ? 'allow' : 'deny'}: ${e.message}`, 'error'); }
  poll();
}

async function unpair(p) {
  if (!confirm(`Remove ${p.name || 'this phone'}?\n\nThe phone will need to pair again.`)) return;
  try {
    await api.unpair(p.id);
    toast(`${p.name || 'Phone'} removed.`);
  } catch (e) { toast(`Could not remove: ${e.message}`, 'error'); }
  poll();
}

function pendingCard(p) {
  const who = [p.name || 'Unnamed phone', p.model ? `(${p.model})` : ''].filter(Boolean).join(' ');
  return h('section', { class: 'card pend', role: 'group', 'aria-label': 'New phone waiting for approval' },
    h('span', { class: 'dico' }, icon('usb')),
    h('div', { class: 'pend-text' },
      h('h3', null, `New phone: ${who} wants to connect`),
      h('small', { class: 'muted' }, 'Plugged in over USB · fp ', h('span', { class: 'mono' }, p.fp_short))),
    h('div', { class: 'pend-btns' },
      h('button', { type: 'button', class: 'btn primary', onclick: () => resolve(p, true) }, 'Allow'),
      h('button', { type: 'button', class: 'btn', onclick: () => resolve(p, false) }, 'Deny')));
}

function pairedBits(p) {
  const link = p.connected
    ? h('span', { class: 'chip' }, icon(p.transport === 'lan' ? 'wifi' : 'usb', 'ico-svg sm'), p.transport === 'lan' ? 'Wi-Fi' : 'USB')
    : h('span', { class: 'chip off' }, 'offline');
  return { link, seen: p.connected ? 'connected now' : `last seen ${relTime(p.last_seen)}` };
}

function removeBtn(p) {
  return h('button', { type: 'button', class: 'btn sm danger', onclick: () => unpair(p) }, 'Remove');
}

function pairedRow(p) {
  const { link, seen } = pairedBits(p);
  return h('li', { class: 'prow' },
    h('span', { class: 'dico' }, icon('devices')),
    h('div', { class: 'dtitle' }, h('h3', null, p.name || 'Unnamed phone'),
      h('small', null, [p.model, seen].filter(Boolean).join(' · '), ' · ', h('span', { class: 'mono' }, p.fp_short))),
    link, removeBtn(p));
}

/** Pair a live card with a paired record only when the match is unambiguous (id, else unique name). */
function matchPaired(d, list) {
  const byId = list.find((p) => p.id === d.id);
  if (byId) return byId;
  const nm = (d.name || '').trim().toLowerCase();
  if (!nm) return null;
  const live = (S.status?.devices ?? []).filter((x) => (x.name || '').trim().toLowerCase() === nm);
  const cands = list.filter((p) => (p.name || '').trim().toLowerCase() === nm);
  return live.length === 1 && cands.length === 1 ? cands[0] : null;
}

// ---- pairing modal ----
let dlg = null;
let ticket = null;
let tickTimer = 0;
let baseline = new Set();
let counter = null;

function fmt(ms) {
  const t = Math.max(0, Math.ceil(ms / 1000));
  return `${String(Math.floor(t / 60)).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`;
}

function tick() {
  if (!dlg || !ticket) return;
  const left = ticket.expires_at * 1000 - Date.now();
  const expired = left <= 0;
  if (counter) counter.textContent = expired ? 'Expired' : fmt(left);
  dlg.classList.toggle('expired', expired);
  const nb = dlg.querySelector('.newcode');
  if (nb) nb.hidden = !expired;
}

function checkPairedGrowth() {
  if (!dlg) return;
  const fresh = paired.filter((p) => !baseline.has(p.id));
  if (fresh.length) {
    closePairing(false);
    toast(`${fresh[0].name || 'Phone'} paired.`);
  }
}

async function loadTicket() {
  const qrBox = dlg.querySelector('.qr');
  try {
    ticket = await api.startPairing();
  } catch (e) {
    ticket = null;
    qrBox.replaceChildren(h('p', { class: 'muted' }, `Could not start pairing: ${e.message}`));
    return;
  }
  qrBox.innerHTML = ticket.qr_svg; // trusted: rendered by the agent from a fixed template
  qrBox.querySelector('svg')?.setAttribute('aria-label', 'QR code for pairing a phone');
  const addrs = (ticket.addrs ?? []).join(', ');
  dlg.querySelector('.addrs').textContent = addrs ? `${addrs} · port ${ticket.port}` : `port ${ticket.port}`;
  tick();
}

function closePairing(cancel) {
  clearInterval(tickTimer);
  const d = dlg;
  dlg = null;
  const had = ticket;
  ticket = null;
  if (cancel && had) api.cancelPairing().catch(() => {});
  if (d) { d.close(); d.remove(); }
}

export async function openPairing() {
  if (dlg) return;
  try { baseline = new Set((await api.paired()).map((p) => p.id)); } catch { baseline = new Set(paired.map((p) => p.id)); }
  counter = h('b', { class: 'count mono', role: 'timer' }, '--:--');
  dlg = h('dialog', { class: 'modal', 'aria-labelledby': 'pair-title' },
    h('header', null, h('h2', { id: 'pair-title' }, 'Pair a phone over Wi-Fi'),
      h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Close', onclick: () => closePairing(true) }, icon('close', 'ico-svg sm'))),
    h('div', { class: 'qr-card' }, h('div', { class: 'qr', 'aria-live': 'polite' })),
    h('p', { class: 'countline' }, 'Code expires in ', counter),
    h('button', { type: 'button', class: 'btn primary newcode', hidden: true, onclick: () => loadTicket() }, 'New code'),
    h('ol', { class: 'steps-list' },
      h('li', null, 'Open ', h('b', null, 'Notideck'), ' on the phone.'),
      h('li', null, 'Go to ', h('b', null, 'PCs'), ' and tap ', h('b', null, 'Scan QR'), '.'),
      h('li', null, 'Or point the phone’s camera app at this code.')),
    h('p', { class: 'muted small' }, 'Phone must be on the same Wi-Fi as this PC.'),
    h('p', { class: 'muted small mono addrs' }),
    h('footer', null, h('button', { type: 'button', class: 'btn', onclick: () => closePairing(true) }, 'Cancel')));
  dlg.addEventListener('cancel', (e) => { e.preventDefault(); closePairing(true); });
  document.body.append(dlg);
  dlg.showModal();
  tickTimer = setInterval(tick, 1000);
  loadTicket();
}

function pairCard() {
  const on = lanOn();
  return h('section', { class: 'card pair-cta' },
    icon('wifi'),
    h('div', null, h('h3', null, 'Pair a phone over Wi-Fi'),
      on ? h('p', { class: 'muted' }, 'No cable needed. Scan a code once; the phone reconnects by itself.')
        : h('p', { class: 'muted' }, 'Wi-Fi connections are off. Turn on ',
          h('a', { href: '#settings', class: 'link', onclick: (e) => { e.preventDefault(); go('settings'); } }, 'Allow Wi-Fi connections'), ' in Settings.')),
    h('button', { type: 'button', class: 'btn primary', disabled: !on, onclick: openPairing }, 'Pair a phone'));
}

const ANDROID = { 28: '9 (Pie)', 29: '10', 30: '11', 31: '12', 32: '12L', 33: '13', 34: '14', 35: '15', 36: '16' };
const androidName = (sdk) => (sdk ? `Android ${ANDROID[sdk] ?? '?'} · SDK ${sdk}` : 'Unknown');

function battery(d) {
  if (d.battery_percent == null) return h('span', { class: 'muted' }, 'Not reported');
  const pct = Math.max(0, Math.min(100, d.battery_percent));
  return h('span', { class: 'batt' },
    h('span', { class: 'cell-bar', role: 'img', 'aria-label': `Battery ${pct} percent` }, h('i', { style: `width:${pct}%` })),
    `${pct}%`,
    d.charging ? h('span', { class: 'charging', title: 'Charging' }, icon('bolt', 'ico-svg sm'), ' Charging') : null);
}

function card(d, pr) {
  const lan = d.transport === 'lan';
  const row = (k, v) => h('div', null, h('dt', null, k), h('dd', null, v));
  return h('article', { class: `dcard${d.connected ? '' : ' off'}` },
    h('header', null,
      h('span', { class: 'dico' }, icon('devices')),
      h('div', { class: 'dtitle' }, h('h3', null, d.name || d.id), h('small', { class: 'mono' }, d.id)),
      h('span', { class: 'chip' }, icon(lan ? 'wifi' : 'usb', 'ico-svg sm'), lan ? 'LAN' : 'USB'),
      h('span', { class: `status ${d.connected ? 'ok' : ''}` }, h('i', { class: 'dot' }), d.connected ? 'Connected' : 'Idle')),
    h('dl', { class: 'facts' },
      row('Battery', battery(d)),
      row('Temperature', d.battery_temp_c != null ? `${d.battery_temp_c.toFixed(1)} °C` : h('span', { class: 'muted' }, 'Not reported')),
      row('Android', androidName(d.android_sdk)),
      row('Notideck app', d.app_version ? h('span', { class: 'mono' }, d.app_version) : h('span', { class: 'muted' }, 'Unknown'))),
    pr ? h('footer', { class: 'dfoot' }, h('small', null, 'Paired', pr.model ? ` · ${pr.model}` : '', ' · fp ', h('span', { class: 'mono' }, pr.fp_short)), removeBtn(pr)) : null);
}

function adbLine() {
  const adb = S.host?.adb;
  if (!S.host) return h('p', { class: 'muted' }, 'Checking…');
  return adb?.found
    ? h('p', { class: 'adb ok' }, icon('check', 'ico-svg sm'), h('span', null, 'adb found', adb.version ? [' · ', h('span', { class: 'mono' }, adb.version)] : null))
    : h('p', { class: 'adb bad' }, icon('warn', 'ico-svg sm'), 'adb not found — USB is disabled. Install Android platform-tools or set ADB_PATH.');
}

let helpOpen = false;

function helpBody() {
  return [
    h('h3', null, 'Connect a phone over USB'),
    h('ol', { class: 'steps-list' },
      h('li', null, 'On the phone, open ', h('b', null, 'Settings › About phone'), ' and tap ', h('b', null, 'Build number'), ' seven times to enable Developer options.'),
      h('li', null, 'Open ', h('b', null, 'Developer options'), ' and turn on ', h('b', null, 'USB debugging'), '.'),
      h('li', null, 'Plug the phone into this PC with a data cable.'),
      h('li', null, 'Accept the ', h('b', null, 'Allow USB debugging?'), ' prompt on the phone. Tick “Always allow” to skip it next time.')),
    adbLine()];
}

export function renderDevices() {
  const root = $('#view-devices');
  const devs = S.status?.devices ?? [];
  const used = new Set();
  const cards = devs.map((d) => {
    const pr = matchPaired(d, paired);
    if (pr) used.add(pr.id);
    return card(d, pr);
  });
  const rest = paired.filter((p) => !used.has(p.id));
  root.replaceChildren(
    h('header', { class: 'page-head' }, h('h1', null, 'Devices'), h('p', { class: 'muted' }, 'Phones that can reach this PC right now.')),
    pending.length ? h('div', { class: 'cards' }, pending.map(pendingCard)) : null,
    h('div', { class: 'cards' },
      devs.length
        ? cards
        : h('section', { class: 'card empty' }, h('span', { class: 'dico big' }, icon('devices')), h('h3', null, S.status ? 'No phone connected' : 'Agent not answering'),
          h('p', { class: 'muted' }, S.status ? 'Follow the steps below to connect one.' : 'Check that Notideck is running.'))),
    pairCard(),
    rest.length ? h('section', { class: 'card sect-paired' }, h('h2', null, 'Paired phones'), h('ul', { class: 'plist' }, rest.map(pairedRow))) : null,
    devs.length ? h('details', { class: 'card help-fold', open: helpOpen, ontoggle: (e) => { helpOpen = e.target.open; } }, h('summary', null, 'Connection help'), helpBody()) : h('section', { class: 'card help' }, helpBody()));
}
