// SPDX-License-Identifier: GPL-3.0-or-later
// Settings tab: general, appearance, keyboard, integrations, storage, about.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S } from './store.js';
import { openOnboarding } from './onboarding.js';
import { applyTheme, applyPhoneTheme, openExternal, fixButton, onRerender } from './shell.js';

const REPO = 'https://github.com/notideck/notideck';

let busy = false;
async function patch(p) {
  if (busy) return;
  busy = true;
  try {
    S.settings = { ...S.settings, ...p };
    if (p.theme) applyTheme(p.theme);
    if (p.phone_theme) applyPhoneTheme(p.phone_theme);
    const saved = await api.saveSettings(p);
    if (saved && typeof saved === 'object' && 'theme' in saved) S.settings = saved;
  } catch (e) {
    toast(`Could not save: ${e.message}`, 'error');
    try { S.settings = await api.settings(); } catch { /* ignore */ }
  }
  busy = false;
  renderSettings();
}

function toggle(id, label, hint, key, dflt = false) {
  const on = S.settings ? (S.settings[key] ?? dflt) : dflt;
  return h('div', { class: 'srow' },
    h('div', null, h('label', { for: id }, label), h('small', null, hint)),
    h('button', { id, type: 'button', class: 'switch', role: 'switch', 'aria-checked': on ? 'true' : 'false', disabled: !S.settings, onclick: () => patch({ [key]: !on }) }, h('i')));
}

function segmented(key, label, opts, dflt) {
  const cur = S.settings?.[key] ?? dflt;
  const els = opts.map(([v, l]) => h('button', {
    type: 'button', role: 'radio', 'aria-checked': v === cur ? 'true' : 'false', tabindex: v === cur ? 0 : -1, class: 'seg', id: `seg-${key}-${v}`,
    onclick: () => patch({ [key]: v }),
  }, l));
  const box = h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': label }, els);
  box.addEventListener('keydown', (e) => {
    const d = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!d) return;
    e.preventDefault();
    const i = (opts.findIndex(([v]) => v === cur) + d + opts.length) % opts.length;
    patch({ [key]: opts[i][0] });
  });
  return box;
}

const PHONE_THEMES = [
  ['Dark', [['black', 'Black (AMOLED)', '#000000', '#0E0F11', '#1B1D21', '#A8C478'], ['graphite', 'Graphite', '#121417', '#1F2328', '#2A2F36', '#A8C478'],
    ['dusk', 'Dusk', '#1E1B26', '#2C2738', '#383247', '#C7B3F0'], ['tide', 'Tide', '#172027', '#222F39', '#2E3E4A', '#9FD4E0']]],
  ['Light', [['cream', 'Cream', '#F6F1EA', '#FFFDFA', '#E3D9CC', '#E9A88B'], ['lavender', 'Lavender', '#EFEDF7', '#FCFBFF', '#D9D4EA', '#A99BE0'],
    ['mint', 'Mint', '#EDF5F1', '#FCFFFD', '#D0E2D8', '#86C9A8']]],
];

// Swatch radiogroup: same roving tabindex and arrow keys as segmented().
function themePicker(key, label, dflt) {
  const cur = S.settings?.[key] ?? dflt;
  const flat = PHONE_THEMES.flatMap(([, o]) => o);
  const groups = PHONE_THEMES.map(([g, opts]) => h('div', { class: 'tp-group', role: 'presentation' },
    h('span', { class: 'tp-h', 'aria-hidden': 'true' }, g),
    h('div', { class: 'tp-cards', role: 'presentation' }, opts.map(([v, l, base, face, edge, acc]) => h('button', {
      type: 'button', role: 'radio', class: 'tp-card', id: `seg-${key}-${v}`, 'aria-checked': v === cur ? 'true' : 'false', tabindex: v === cur ? 0 : -1,
      'aria-label': `${l}, ${g.toLowerCase()}`, onclick: () => patch({ [key]: v }),
    }, h('span', { class: 'tp-sw', style: `background:${base}`, 'aria-hidden': 'true' },
      h('i', { style: `background:${face};border-color:${edge}` }), h('i', { style: `background:${face};border-color:${edge}` }), h('b', { style: `background:${acc}` })),
    h('span', null, l))))));
  const box = h('div', { class: 'theme-picker', role: 'radiogroup', 'aria-label': label }, groups);
  box.addEventListener('keydown', (e) => {
    const d = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!d) return;
    e.preventDefault();
    const i = (Math.max(0, flat.findIndex(([v]) => v === cur)) + d + flat.length) % flat.length;
    patch({ [key]: flat[i][0] });
  });
  return box;
}

const section = (title, ...kids) => h('section', { class: 'card sect' }, h('h2', null, title), ...kids);
const kv = (k, v) => h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, k)), h('span', { class: 'v' }, v));
const mono = (t) => h('span', { class: 'mono' }, t);

function keyboardRow() {
  const acc = S.host?.keyboard_access;
  const prob = (S.status?.problems ?? []).find((p) => p.fix === 'keyboard_access');
  const label = acc === 'ok' ? 'Ready' : acc === 'missing' ? 'Needs permission' : acc === 'unsupported' ? 'Not supported on this system' : 'Checking…';
  return h('div', { class: 'srow' },
    h('div', null, h('span', { class: 'k' }, 'Keyboard access'), h('small', null, 'Lets keys on the phone press shortcuts on this PC.')),
    h('span', { class: 'v statusline' }, h('i', { class: `dot ${acc === 'ok' ? 'ok' : acc === 'missing' ? 'warn' : ''}` }), label,
      prob ? fixButton('keyboard_access', 'Fix…', 'btn sm primary') : null));
}

function openBtn(which, label) {
  return h('button', { type: 'button', class: 'btn', onclick: async () => {
    try { await api.open(which); } catch (e) { toast(`Could not open: ${e.message}`, 'error'); }
  } }, icon('folder', 'ico-svg sm'), label);
}

let webcamState = null;
function webcamRow() {
  const inst = webcamState?.installed;
  const can = inst === 'installed' || inst === 'outdated';
  const linux = webcamState?.platform === 'linux';
  const note = inst === 'unsupported' ? 'Only available on Windows and Linux.'
    : inst === 'module_missing' ? 'The v4l2loopback kernel module is not installed for the running kernel. Install v4l2loopback-dkms (Fedora: v4l2loopback from RPM Fusion).'
    : can ? (linux ? 'Notideck Camera is set up. Removing it asks for your administrator password.' : 'Notideck Camera is installed. Removing it needs administrator permission.')
    : linux ? 'Notideck Camera is not set up. Use the Camera tab to set it up.' : 'Notideck Camera is not installed.';
  return h('div', { class: 'srow' },
    h('div', null, h('span', { class: 'k' }, linux ? 'Webcam device' : 'Webcam driver'), h('small', null, note)),
    can ? fixButton('uninstall_webcam', 'Uninstall…', 'btn sm danger') : h('span', { class: 'v muted' }, inst === 'unsupported' ? '—' : inst === 'module_missing' ? 'Module missing' : 'Not installed'));
}

const SLEEP_AFTER = [[30, '30 seconds'], [60, '1 minute'], [120, '2 minutes'], [300, '5 minutes'], [600, '10 minutes'], [1800, '30 minutes']];

function sleepAfterRow() {
  const cur = S.settings?.deck_sleep_idle_s ?? 120;
  const opts = SLEEP_AFTER.some(([v]) => v === cur) ? SLEEP_AFTER : [...SLEEP_AFTER, [cur, `${cur} seconds`]].sort((a, b) => a[0] - b[0]);
  return h('div', { class: 'srow' },
    h('div', null, h('label', { for: 'set-sleep-after' }, 'After'), h('small', null, 'How long without a touch before the deck sleeps.')),
    h('select', { id: 'set-sleep-after', 'aria-label': 'Sleep after', disabled: !S.settings || S.settings.deck_sleep_enabled === false,
      onchange: (e) => patch({ deck_sleep_idle_s: parseInt(e.target.value, 10) }) },
    opts.map(([v, l]) => h('option', { value: String(v), selected: v === cur }, l))));
}

export function renderSettings() {
  if (!webcamState) api.camera().then((c) => { webcamState = c; if (!$('#view-settings').hidden) renderSettings(); }).catch(() => {});
  const root = $('#view-settings');
  const hst = S.host;
  const focusId = document.activeElement?.id;
  root.replaceChildren(
    h('header', { class: 'page-head' }, h('h1', null, 'Settings')),
    section('General',
      toggle('set-autostart', 'Start at login', 'Open Notideck when you sign in to this PC.', 'autostart'),
      toggle('set-minimized', 'Start minimized to tray', 'Skip the window on start; the tray icon stays available.', 'start_minimized')),
    section('Network',
      toggle('set-lan', 'Allow Wi-Fi connections', 'Phones on your network can connect after pairing. Windows may ask to allow Notideck through the firewall — choose Private networks. Turning this on takes effect after Notideck restarts.', 'lan_enabled', true)),
    section('Appearance',
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Theme'), h('small', null, 'System follows your operating system.')),
        segmented('theme', 'Theme', [['system', 'System'], ['dark', 'Dark'], ['light', 'Light']], 'system')),
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Phone theme'), h('small', null, 'Black turns pixels off on AMOLED screens and saves battery; the pastel themes are softer. Applies to the phone instantly.')),
        themePicker('phone_theme', 'Phone theme', 'black'))),
    section('Phone screen',
      toggle('set-sleep', 'Sleep when idle', 'The deck goes black and dims the backlight; the first touch wakes it.', 'deck_sleep_enabled', true),
      sleepAfterRow(),
      toggle('set-sleep-wake', 'Wake when the PC uses the camera or mic', 'The deck wakes when an app starts the phone camera or microphone.', 'deck_sleep_wake_on_camera_mic', true),
      toggle('set-sleep-clock', 'Show a clock while asleep', 'A faint clock drifts around the black screen.', 'deck_sleep_clock', true)),
    section('Keyboard and input', keyboardRow()),
    section('Integrations',
      kv('Audio backend', mono(hst?.audio_backend ?? '—')),
      kv('Media backend', mono(hst?.media_backend ?? '—'))),
    section('Setup',
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Setup guide'), h('small', null, 'Walk through installing the phone app, connecting and picking a first profile again.')),
        h('button', { id: 'set-run-setup', type: 'button', class: 'btn', onclick: () => openOnboarding() }, 'Run setup again'))),
    section('Advanced', webcamRow()),
    section('Storage',
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Logs'), h('small', { class: 'mono path' }, hst?.log_dir ?? '')), openBtn('logs', 'Open logs folder')),
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Configuration'), h('small', { class: 'mono path' }, hst?.config_dir ?? '')), openBtn('config', 'Open config folder'))),
    section('About',
      kv('Version', mono(hst?.version ?? S.status?.version ?? '—')),
      kv('System', hst ? mono(hst.os) : '—'),
      kv('Ports', hst ? mono(`UI ${hst.ui_port} · control ${hst.control_port}`) : '—'),
      kv('Licence', 'GPL-3.0-or-later'),
      h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, 'Project')),
        h('a', { href: REPO, class: 'link', onclick: (e) => { e.preventDefault(); openExternal(REPO); } }, 'github.com/notideck/notideck', icon('external', 'ico-svg sm')))),
  );
  if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
}

onRerender(() => { if (!$('#view-settings').hidden) renderSettings(); });
