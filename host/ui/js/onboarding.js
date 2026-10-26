// SPDX-License-Identifier: GPL-3.0-or-later
// First-run setup wizard (round 23): a full-screen overlay with six short steps. It opens when
// settings.onboarding_done is false, and from Settings > Run setup again. Polling runs only while it is open.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S, refreshHost, refreshStatus, createProfileFromTemplate } from './store.js';
import { TEMPLATE_GROUPS } from './sidebar.js';
import { poll as pollPairing, openPairing, lanOn } from './devices.js';

const STEPS = ['Welcome', 'Phone app', 'Connect', 'First profile', 'Extras', 'Done'];
const POLL_MS = 2000;

let root = null;        // the overlay element, null while closed
let step = 0;
let timer = 0;
let usb = null;         // last GET /api/phone/usb
let pending = [];       // phones waiting for Allow
let cam = null;         // last GET /api/camera (Windows webcam driver / Linux v4l2loopback)
let installing = new Set();
let installErr = {};
let starter = 'blank';  // chosen template value
let created = null;     // { tpl, name } once the starter profile exists
let creating = false;
let busyFix = '';
let prevFocus = null;

const isWindows = () => S.host?.os === 'windows';
const isLinux = () => S.host?.os === 'linux';
const connectedPhone = () => (S.status?.devices ?? []).find((d) => d.connected) || null;

// ---- open / close ----

export function openOnboarding() {
  if (root) return;
  step = 0;
  created = null;
  starter = 'blank';
  installErr = {};
  installing = new Set();
  prevFocus = document.activeElement;
  root = h('div', { id: 'onboarding', class: 'ob', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'ob-title' });
  document.body.append(root);
  $('.app')?.setAttribute('inert', '');
  document.body.classList.add('ob-open');
  addEventListener('keydown', onKey, true);
  render(true);
  refreshHost().then(() => root && step === 4 && renderBody());
  timer = setInterval(tick, POLL_MS);
  tick();
}

function close() {
  clearInterval(timer);
  timer = 0;
  removeEventListener('keydown', onKey, true);
  root?.remove();
  root = null;
  $('.app')?.removeAttribute('inert');
  document.body.classList.remove('ob-open');
  try { prevFocus?.focus?.({ preventScroll: true }); } catch { /* element gone */ }
  prevFocus = null;
}

async function finish() {
  try {
    const saved = await api.saveSettings({ onboarding_done: true });
    S.settings = saved && typeof saved === 'object' && 'theme' in saved ? saved : { ...S.settings, onboarding_done: true };
  } catch (e) {
    toast(`Could not save: ${e.message}`, 'error');
    return;
  }
  close();
}

// Keep Tab inside the wizard (the app behind it is inert; toasts and the pairing dialog stay usable).
function onKey(e) {
  if (e.key !== 'Tab' || !root || document.querySelector('dialog[open]')) return;
  const items = [...root.querySelectorAll('button:not(:disabled), a[href], summary, input:not(:disabled), [tabindex="0"]')].filter((el) => el.offsetParent !== null);
  if (!items.length) return;
  const first = items[0];
  const last = items[items.length - 1];
  if (!root.contains(document.activeElement)) { e.preventDefault(); first.focus(); }
  else if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
}

// ---- polling (only while open, and only what the visible step needs) ----

let ticking = false;
async function tick() {
  if (!root || document.hidden || ticking) return;
  ticking = true;
  try {
    if (step === 1) {
      try { usb = await api.phoneUsb(); } catch { usb = { adb_ok: false, apk_available: false, phones: [] }; }
      if (root && step === 1) renderUsb();
    } else if (step === 2) {
      await Promise.all([refreshStatus(), pollPairing(), api.pending().then((p) => { pending = Array.isArray(p) ? p : []; }).catch(() => {})]);
      if (root && step === 2) renderConnect();
    } else if (step === 4) {
      await refreshHost();
      if (isWindows() || isLinux()) { try { cam = await api.camera(); } catch { /* old agent */ } }
      if (root && step === 4) renderExtras();
    }
  } finally {
    ticking = false;
  }
}

function go(n) {
  step = Math.max(0, Math.min(STEPS.length - 1, n));
  usb = step === 1 ? usb : null;
  render(true);
  tick();
}

// ---- frame ----

function render(focus) {
  if (!root) return;
  root.replaceChildren(
    h('div', { class: 'ob-panel' },
      h('header', { class: 'ob-head' },
        h('div', { class: 'ob-brand' }, icon('deck', 'ico-svg'), h('span', null, 'Set up Notideck')),
        step < STEPS.length - 1 ? h('button', { type: 'button', class: 'btn sm', id: 'ob-skip', onclick: finish }, 'Skip setup') : null),
      h('ol', { class: 'ob-steps', 'aria-label': 'Progress' }, STEPS.map((n, i) => h('li', {
        class: i === step ? 'cur' : i < step ? 'done' : '', 'aria-current': i === step ? 'step' : null,
      }, h('i', { 'aria-hidden': 'true' }, i < step ? icon('check', 'ico-svg sm') : String(i + 1)), h('span', null, n)))),
      h('div', { class: 'ob-body', id: 'ob-body' }),
      h('footer', { class: 'ob-foot', id: 'ob-foot' })));
  renderBody(focus);
}

function renderBody(focus) {
  const body = $('#ob-body', root);
  if (!body) return;
  const view = [stepWelcome, stepPhoneApp, stepConnect, stepProfile, stepExtras, stepDone][step]().filter(Boolean);
  body.replaceChildren(h('h1', { id: 'ob-title', tabindex: '-1' }, STEPS[step] === 'Welcome' ? 'Welcome to Notideck' : stepTitle()), ...view);
  renderFoot();
  if (step === 1) renderUsb();
  if (step === 2) renderConnect();
  if (step === 4) renderExtras();
  if (focus) $('#ob-title', root)?.focus({ preventScroll: true });
  body.scrollTop = 0;
}

const stepTitle = () => ['', 'Install the phone app', 'Connect your phone', 'Pick a first profile', 'A few extras', 'You are all set'][step];

function renderFoot() {
  const foot = $('#ob-foot', root);
  const back = step > 0 && step < STEPS.length - 1 ? h('button', { type: 'button', class: 'btn', id: 'ob-back', onclick: () => go(step - 1) }, 'Back') : h('span');
  let next;
  if (step === STEPS.length - 1) {
    next = h('button', { type: 'button', class: 'btn primary', id: 'ob-finish', onclick: finish }, 'Finish');
  } else if (step === 2 && !connectedPhone()) {
    next = h('button', { type: 'button', class: 'btn', id: 'ob-next', onclick: () => go(step + 1) }, 'Continue without a phone');
  } else if (step === 3) {
    next = h('button', { type: 'button', class: 'btn primary', id: 'ob-next', disabled: creating, 'aria-busy': creating ? 'true' : null, onclick: nextFromProfile },
      creating ? 'Creating…' : starter === 'blank' || created ? 'Next' : 'Create and continue');
  } else {
    next = h('button', { type: 'button', class: 'btn primary', id: 'ob-next', onclick: () => go(step + 1) }, step === 0 ? 'Get started' : 'Next');
  }
  foot.replaceChildren(back, next);
}

// ---- 1 Welcome ----

function stepWelcome() {
  const point = (glyph, title, text) => h('li', null, h('span', { class: 'ob-ico' }, icon(glyph)), h('div', null, h('b', null, title), h('p', { class: 'muted' }, text)));
  return [
    h('p', { class: 'ob-lead' }, 'Turn an old Android phone into a deck of keys and a webcam for this PC.'),
    h('ul', { class: 'ob-points' },
      point('deck', 'A deck of keys', 'Tap a key on the phone to press shortcuts, open apps, change volume or control OBS.'),
      point('camera', 'A webcam', 'Use the phone camera in Zoom, Discord, OBS and more.'),
      point('usb', 'Cable or Wi-Fi', 'Plug it in over USB, or pair once over Wi-Fi.')),
    h('p', { class: 'muted' }, 'Setup takes about two minutes. You can skip it any time and run it again from Settings.'),
  ];
}

// ---- 2 Phone app ----

function stepPhoneApp() {
  return [
    h('p', { class: 'ob-lead' }, 'Notideck needs its app on the phone. Plug the phone into this PC with a data cable and we will install it for you.'),
    h('details', { class: 'ob-fold' },
      h('summary', null, 'How to turn on USB debugging'),
      h('ol', { class: 'steps-list' },
        h('li', null, 'On the phone, open ', h('b', null, 'Settings › About phone'), ' and tap ', h('b', null, 'Build number'), ' seven times.'),
        h('li', null, 'Go back, open ', h('b', null, 'Developer options'), ' and turn on ', h('b', null, 'USB debugging'), '.'),
        h('li', null, 'Plug the phone in. When it asks ', h('b', null, 'Allow USB debugging?'), ', tap Allow. Tick “Always allow” to skip this next time.'))),
    h('div', { id: 'ob-usb', class: 'ob-usb', 'aria-live': 'polite' }),
    h('p', { class: 'muted small' }, 'No cable? You can install the app by hand and pair over Wi-Fi in the next step.'),
  ];
}

function renderUsb() {
  const box = $('#ob-usb', root);
  if (!box) return;
  const kids = [];
  if (!usb) {
    kids.push(h('p', { class: 'muted' }, 'Looking for phones…'));
  } else if (!usb.adb_ok) {
    kids.push(h('p', { class: 'ob-note warn' }, icon('warn', 'ico-svg sm'), h('span', null, 'USB is not available because Notideck cannot run adb. You can still pair over Wi-Fi in the next step.')));
  } else if (!usb.phones.length) {
    kids.push(h('p', { class: 'ob-note' }, h('span', { class: 'ob-spin', 'aria-hidden': 'true' }), h('span', null, 'No phone found yet. Plug it in and turn on USB debugging.')));
  } else {
    kids.push(h('ul', { class: 'ob-phones' }, usb.phones.map(phoneRow)));
  }
  if (usb && usb.adb_ok && !usb.apk_available) {
    kids.push(h('p', { class: 'ob-note' }, icon('warn', 'ico-svg sm'),
      h('span', null, 'This copy of Notideck does not include the phone app. Download ', h('b', null, 'notideck-phone.apk'), ' from the Notideck releases page, put it next to Notideck, then come back. Or copy the file to the phone and open it there.')));
  }
  const sig = JSON.stringify([usb, [...installing], installErr]);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  box.replaceChildren(...kids);
}

function phoneRow(p) {
  const name = p.model || p.serial;
  let right;
  let sub = p.serial;
  if (p.state === 'unauthorized') {
    right = h('span', { class: 'chip off' }, 'Waiting for you');
    sub = 'Look at the phone and tap Allow on the “Allow USB debugging?” prompt.';
  } else if (p.state === 'offline' || p.state === 'authorizing') {
    right = h('span', { class: 'chip off' }, 'Not ready');
    sub = 'Unplug the cable, plug it in again and wait a moment.';
  } else if (p.state !== 'device') {
    right = h('span', { class: 'chip off' }, p.state);
  } else if (installing.has(p.serial)) {
    right = h('button', { type: 'button', class: 'btn sm', disabled: true, 'aria-busy': 'true' }, h('span', { class: 'ob-spin', 'aria-hidden': 'true' }), 'Installing…');
  } else if (p.app_installed) {
    right = h('span', { class: 'ob-ok' }, icon('check', 'ico-svg sm'), p.app_version ? `Installed v${p.app_version}` : 'Installed');
  } else if (p.app_installed === false) {
    right = h('button', { type: 'button', class: 'btn sm primary', disabled: !usb.apk_available, onclick: () => install(p.serial) }, 'Install app');
  } else {
    right = h('span', { class: 'muted' }, 'Checking…');
  }
  return h('li', { class: 'ob-phone' },
    h('span', { class: 'ob-ico' }, icon('devices')),
    h('div', { class: 'ob-phone-t' }, h('b', null, name), h('small', { class: installErr[p.serial] ? 'warn' : 'muted' }, installErr[p.serial] || sub)),
    right);
}

async function install(serial) {
  installing.add(serial);
  delete installErr[serial];
  renderUsb();
  try {
    await api.phoneInstall(serial);
    toast('Phone app installed. Open it on the phone.');
  } catch (e) {
    installErr[serial] = e.status === 404 ? 'This copy of Notideck has no phone app file, or the phone went away.' : `Install failed: ${e.message}`;
  }
  installing.delete(serial);
  try { usb = await api.phoneUsb(); } catch { /* next tick */ }
  if (root && step === 1) renderUsb();
}

// ---- 3 Connect ----

function stepConnect() {
  return [
    h('p', { class: 'ob-lead' }, 'Open the Notideck app on the phone. Over USB it connects on its own.'),
    h('div', { id: 'ob-conn', class: 'ob-conn', 'aria-live': 'polite' }),
    h('section', { class: 'ob-wifi' },
      h('span', { class: 'ob-ico' }, icon('wifi')),
      h('div', null, h('b', null, 'Prefer Wi-Fi?'), h('p', { class: 'muted' }, lanOn() ? 'Scan a code with the phone once. After that it reconnects by itself.' : 'Wi-Fi connections are off. Turn them on in Settings to pair this way.')),
      h('button', { type: 'button', class: 'btn', id: 'ob-pair', disabled: !lanOn(), onclick: () => openPairing() }, 'Pair over Wi-Fi')),
  ];
}

function renderConnect() {
  const box = $('#ob-conn', root);
  if (!box) return;
  const kids = pending.map((p) => h('div', { class: 'ob-pend', role: 'group', 'aria-label': 'New phone waiting for approval' },
    h('span', { class: 'ob-ico' }, icon('usb')),
    h('div', { class: 'ob-phone-t' }, h('b', null, `${p.name || 'A phone'} wants to connect`), h('small', { class: 'muted' }, 'Allow it only if this is your phone.')),
    h('button', { type: 'button', class: 'btn sm primary', onclick: () => resolve(p, true) }, 'Allow'),
    h('button', { type: 'button', class: 'btn sm', onclick: () => resolve(p, false) }, 'Deny')));
  const dev = connectedPhone();
  if (dev) {
    kids.push(h('p', { class: 'ob-note ok' }, icon('check', 'ico-svg sm'),
      h('span', null, h('b', null, dev.name || dev.id), dev.transport === 'lan' ? ' is connected over Wi-Fi.' : ' is connected over USB.')));
  } else if (!pending.length) {
    kids.push(h('p', { class: 'ob-note' }, h('span', { class: 'ob-spin', 'aria-hidden': 'true' }), h('span', null, 'Waiting for the phone. Open the Notideck app on it.')));
  }
  const sig = JSON.stringify([pending, dev && [dev.id, dev.name, dev.transport]]);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  box.replaceChildren(...kids);
  renderFoot();
}

async function resolve(p, allow) {
  try {
    await api.resolvePending(p.session_id, allow);
    toast(allow ? `${p.name || 'Phone'} allowed.` : `${p.name || 'Phone'} denied.`);
  } catch (e) { toast(`Could not ${allow ? 'allow' : 'deny'}: ${e.message}`, 'error'); }
  tick();
}

// ---- 4 First profile ----

const STARTERS = TEMPLATE_GROUPS.flatMap(([, items]) => items).filter(([v]) => v !== 'blank');
const STARTER_BLURB = { obs: 'Scenes, record, stream and mic keys', media: 'Play, pause, skip and volume' };

function stepProfile() {
  const cards = [['blank', 'Start empty', 'Build your own deck from scratch']].concat(STARTERS.map(([v, l]) => [v, l, STARTER_BLURB[v] || 'Ready-made shortcuts']));
  const group = h('div', { class: 'ob-tpls', role: 'radiogroup', 'aria-label': 'Starter profile' }, cards.map(([v, l, d]) => h('button', {
    type: 'button', role: 'radio', class: 'ob-tpl', id: `ob-tpl-${v}`, 'aria-checked': v === starter ? 'true' : 'false', tabindex: v === starter ? 0 : -1,
    disabled: !!created, onclick: () => pick(v),
  }, h('b', null, l), h('small', null, d))));
  group.addEventListener('keydown', (e) => {
    const dl = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!dl || created) return;
    e.preventDefault();
    const i = cards.findIndex(([v]) => v === starter);
    pick(cards[(i + dl + cards.length) % cards.length][0], true);
  });
  return [
    h('p', { class: 'ob-lead' }, 'A profile is a set of pages with keys. Pick a ready-made one to start from, or start empty. You can change everything later.'),
    created ? h('p', { class: 'ob-note ok' }, icon('check', 'ico-svg sm'), h('span', null, h('b', null, created.name), ' was created.')) : null,
    group,
  ];
}

function pick(v, focus) {
  starter = v;
  renderBody();
  if (focus) $(`#ob-tpl-${v}`, root)?.focus({ preventScroll: true });
}

async function nextFromProfile() {
  if (starter === 'blank' || created) { go(4); return; }
  const label = (STARTERS.find(([v]) => v === starter) || [])[1] || 'Profile';
  const name = starter === 'media' ? 'Media' : label;
  creating = true;
  renderFoot();
  try {
    await createProfileFromTemplate(name, starter, { autoswitch: false });
    created = { tpl: starter, name };
    toast(`${name} profile created.`);
    creating = false;
    go(4);
  } catch (e) {
    creating = false;
    toast(`Could not create the profile: ${e.message}`, 'error');
    renderFoot();
  }
}

// ---- 5 Extras ----

function stepExtras() {
  return [
    h('p', { class: 'ob-lead' }, 'Optional. You can change these later in Settings.'),
    h('div', { id: 'ob-extras', class: 'ob-extras' }),
  ];
}

function extraRow(title, text, control) {
  return h('div', { class: 'ob-xrow' }, h('div', null, h('b', null, title), h('p', { class: 'muted' }, text)), control);
}

function renderExtras() {
  const box = $('#ob-extras', root);
  if (!box) return;
  const rows = [];
  if (isWindows() || isLinux()) {
    const inst = cam?.installed;
    const lx = isLinux();
    if (inst === 'installed') rows.push(extraRow('Webcam', lx ? 'The Notideck Camera is set up.' : 'The Notideck Camera driver is installed.', h('span', { class: 'ob-ok' }, icon('check', 'ico-svg sm'), lx ? 'Ready' : 'Installed')));
    else if (inst === 'module_missing') rows.push(extraRow('Webcam', 'Needs the v4l2loopback kernel module: install v4l2loopback-dkms (Fedora: v4l2loopback from RPM Fusion), then reopen this page.', h('span', { class: 'muted' }, 'Module missing')));
    else if (inst === 'unsupported') rows.push(extraRow('Webcam', 'The webcam is not available on this system.', h('span', { class: 'muted' }, '—')));
    else if (inst === 'not_installed' || inst === 'outdated') {
      const running = cam.install?.running || busyFix === 'install_webcam';
      rows.push(extraRow('Webcam', lx ? 'Lets Zoom, Discord, OBS and browsers use the phone camera. This asks for your password once.' : 'Lets Zoom, Discord and OBS use the phone camera. Windows asks for administrator permission once.',
        h('button', { type: 'button', class: 'btn sm', id: 'ob-webcam', disabled: running || (!cam.dll_present && !lx), 'aria-busy': running ? 'true' : null, onclick: () => runFix('install_webcam') },
          running ? (lx ? 'Waiting for password…' : 'Waiting for permission…') : inst === 'outdated' ? 'Update webcam' : lx ? 'Set up webcam' : 'Install webcam')));
    } else rows.push(extraRow('Webcam', 'Checking…', h('span')));
  }
  if (isLinux()) {
    const acc = S.host?.keyboard_access;
    if (acc === 'ok') rows.push(extraRow('Keyboard access', 'Keys on the phone can press shortcuts on this PC.', h('span', { class: 'ob-ok' }, icon('check', 'ico-svg sm'), 'Ready')));
    else if (acc === 'missing') {
      rows.push(extraRow('Keyboard access', 'Notideck needs permission to press keys on this PC. This asks for your password once.',
        h('button', { type: 'button', class: 'btn sm', id: 'ob-kbd', disabled: busyFix === 'keyboard_access', 'aria-busy': busyFix === 'keyboard_access' ? 'true' : null, onclick: () => runFix('keyboard_access') },
          busyFix === 'keyboard_access' ? 'Working…' : 'Fix…')));
    } else if (acc === 'unsupported') rows.push(extraRow('Keyboard access', 'Pressing keys is not supported on this system.', h('span', { class: 'muted' }, '—')));
    else rows.push(extraRow('Keyboard access', 'Checking…', h('span')));
  }
  const on = !!S.settings?.autostart;
  rows.push(extraRow('Start at login', 'Open Notideck in the tray when you sign in, so the phone is ready.',
    h('button', { id: 'ob-autostart', type: 'button', class: 'switch', role: 'switch', 'aria-label': 'Start at login', 'aria-checked': on ? 'true' : 'false', onclick: toggleAutostart }, h('i'))));
  const sig = JSON.stringify([rows.length, cam?.installed, cam?.install?.running, cam?.dll_present, S.host?.keyboard_access, busyFix, on]);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  box.replaceChildren(...rows);
}

async function runFix(id) {
  busyFix = id;
  renderExtras();
  try {
    await api.fix(id);
    if (id === 'install_webcam') toast(isLinux() ? 'Enter your password in the prompt…' : 'Approve the Windows permission prompt…');
  } catch (e) { toast(`Failed: ${e.message}`, 'error'); }
  busyFix = '';
  await tick();
  if (root && step === 4) renderExtras();
}

async function toggleAutostart() {
  const want = !S.settings?.autostart;
  try {
    const saved = await api.saveSettings({ autostart: want });
    if (saved && typeof saved === 'object' && 'theme' in saved) S.settings = saved;
  } catch (e) { toast(`Could not change it: ${e.message}`, 'error'); }
  if (root && step === 4) { renderExtras(); $('#ob-autostart', root)?.focus({ preventScroll: true }); }
}

// ---- 6 Done ----

function stepDone() {
  const dev = connectedPhone();
  return [
    h('p', { class: 'ob-lead' }, dev ? `${dev.name || 'Your phone'} is connected. Your keys are ready.` : 'Notideck is ready. Connect a phone any time from the Devices tab.'),
    h('ul', { class: 'ob-points' },
      h('li', null, h('span', { class: 'ob-ico' }, icon('deck')), h('div', null, h('b', null, 'Edit your deck'), h('p', { class: 'muted' }, 'Use the Deck tab to add keys and pages, then push them to the phone.'))),
      h('li', null, h('span', { class: 'ob-ico' }, icon('settings')), h('div', null, h('b', null, 'Run this again'), h('p', { class: 'muted' }, 'Settings › Setup guide opens this guide any time.')))),
  ];
}
