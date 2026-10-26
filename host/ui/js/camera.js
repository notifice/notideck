// SPDX-License-Identifier: GPL-3.0-or-later
// Camera tab: the phone as a webcam. Status, install card, lens/mode/bitrate, live controls, test.

import { h, $, toast, debounce } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S, refreshStatus } from './store.js';
import { imageCard, imageSig, readoutText } from './camimage.js';

let cam = null;        // last GET /api/camera
let sig = '';
let timer = 0;
let previewTimer = 0;
let failed = false;

const root = () => $('#view-camera');
const sizeLabel = (w, h_) => `${w}×${h_}`;
const linkLabel = (l) => (l === 'lan' ? 'Wi-Fi' : 'USB');
const camSettings = () => ({
  lens: S.settings?.camera_lens ?? 'back',
  mode: S.settings?.camera_mode ?? 'auto',
  bitrate: S.settings?.camera_bitrate_kbps ?? 0,
  mirror: S.settings?.camera_mirror ?? 'auto',
  auto: S.settings?.camera_auto_start ?? true,
});

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
  try { cam = await api.cameraControl(c); } catch (e) { toast(e.message, 'error'); }
  render(true);
}

export function statusText(c) {
  if (!c) return 'Checking the camera…';
  if (c.state === 'unsupported') return c.message || 'The webcam is not available.';
  if (c.installed === 'module_missing') return 'The v4l2loopback kernel module is missing';
  if (c.installed === 'not_installed') return c.platform === 'linux' ? 'Notideck Camera is not set up yet' : 'Notideck Camera is not installed yet';
  if (c.installed === 'outdated' && c.state === 'idle') return 'Notideck Camera needs an update';
  const used = c.consumer?.source === 'test' ? 'test stream' : 'an app';
  switch (c.state) {
    case 'streaming': {
      const m = c.active_mode;
      const link = c.phone ? linkLabel(c.phone.link) : '';
      const lag = c.stats?.age_ms ? `, ${Math.round(c.stats.age_ms)} ms` : '';
      const base = m ? `Streaming ${sizeLabel(m.width, m.height)} @ ${Math.round(c.stats?.fps_out || m.fps)} fps over ${link}${lag}` : 'Streaming';
      return c.paused ? `Paused (${base.toLowerCase()})` : `${base}`;
    }
    case 'starting': return 'Starting the phone camera…';
    case 'waiting_phone': return `Waiting for a phone: ${used === 'an app' ? 'an app is using the camera' : 'test stream'}, but no phone with a camera is connected`;
    case 'error': return c.message || 'The phone camera reported an error';
    default:
      if (c.consumer) return 'Notideck Camera is in use';
      return c.phone ? 'Notideck Camera is idle' : 'Notideck Camera is idle. No phone connected';
  }
}

const dotClass = (c) => (!c ? '' : c.state === 'streaming' ? 'ok' : c.state === 'error' || c.installed === 'not_installed' || c.installed === 'module_missing' ? 'warn' : '');

function modeOptions(c) {
  const lens = camSettings().lens;
  const modes = (c?.phone?.supported_modes ?? []).filter((m) => m.lens === lens);
  const opts = [['auto', 'Automatic (fits the app)']];
  for (const m of modes) for (const f of m.fps) opts.push([`${m.width}x${m.height}@${f}`, `${sizeLabel(m.width, m.height)} @ ${f} fps`]);
  const cur = camSettings().mode;
  if (cur !== 'auto' && !opts.some(([v]) => v === cur)) opts.push([cur, `${cur.replace('x', '×').replace('@', ' @ ')} fps (not offered by this phone)`]);
  return opts;
}

function row(label, hint, control_) {
  return h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, label), hint ? h('small', null, hint) : null), control_);
}

function switchBtn(id, on, onclick, disabled = false) {
  return h('button', { id, type: 'button', class: 'switch', role: 'switch', 'aria-checked': on ? 'true' : 'false', disabled, onclick }, h('i'));
}

function segmented(label, opts, cur, onpick) {
  return h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': label }, opts.map(([v, l]) => h('button', {
    type: 'button', role: 'radio', class: 'seg', 'aria-checked': v === cur ? 'true' : 'false', tabindex: v === cur ? 0 : -1, onclick: () => onpick(v),
  }, l)));
}

function installCard(c) {
  const job = c.install || {};
  const linux = c.platform === 'linux';
  if (c.installed === 'unsupported') {
    return h('section', { class: 'card cam-note' }, icon('camera'), h('p', null, 'The Notideck webcam driver is available on Windows 11 and Linux. You can still try the stream with the test camera below.'));
  }
  if (c.installed === 'installed') return null;
  if (c.installed === 'module_missing') {
    return h('section', { class: 'card cam-note' }, icon('camera'), h('div', null,
      h('p', null, 'The webcam needs the v4l2loopback kernel module, which is not installed for the running kernel.'),
      h('p', { class: 'muted' }, 'Arch / CachyOS: install v4l2loopback-dkms (or your kernel’s own v4l2loopback package). Debian / Ubuntu: v4l2loopback-dkms. Fedora: v4l2loopback from RPM Fusion. Then reopen this page. You can still try the stream with the test camera below.')));
  }
  const outdated = c.installed === 'outdated';
  return h('section', { class: 'card cam-install' },
    h('div', null,
      h('h2', null, outdated ? 'Update the webcam' : 'Install the webcam'),
      h('p', { class: 'muted' }, linux
        ? 'This asks for your administrator password once to set up a virtual camera called “Notideck Camera” (the v4l2loopback module, loaded at boot). Afterwards Zoom, Discord, OBS and browsers can pick it.'
        : 'Windows needs a one-time administrator approval to register “Notideck Camera”. Afterwards Zoom, Teams, Discord, OBS and the Camera app can pick it. Nothing else changes on your PC.'),
      !c.dll_present && !linux ? h('p', { class: 'warn' }, 'notideck_vcam.dll is missing next to Notideck.exe, so the install cannot run. Re-extract the Notideck zip.') : null,
      job.error ? h('p', { class: 'warn', role: 'alert' }, job.error) : null),
    h('button', {
      id: 'cam-install', type: 'button', class: 'btn primary', disabled: job.running || (!c.dll_present && !linux), 'aria-busy': job.running ? 'true' : null,
      onclick: async () => {
        try { await api.fix('install_webcam'); toast(linux ? 'Enter your password in the prompt…' : 'Approve the Windows permission prompt…'); } catch (e) { toast(`Install failed: ${e.message}`, 'error'); }
        load();
      },
    }, job.running ? (linux ? 'Waiting for password…' : 'Waiting for permission…') : outdated ? 'Update webcam (needs admin once)' : linux ? 'Set up webcam (asks for password)' : 'Install webcam (needs admin once)'));
}

function bitrateRow(set) {
  const auto = set.bitrate === 0;
  const out = h('output', { id: 'cam-bitrate-out' }, auto ? 'Auto' : `${(set.bitrate / 1000).toFixed(1)} Mbit/s`);
  const range = h('input', {
    id: 'cam-bitrate', type: 'range', min: 500, max: 12000, step: 250, value: auto ? 4000 : set.bitrate, disabled: auto, 'aria-label': 'Bitrate in kbit/s',
    oninput: (e) => { out.textContent = `${(e.target.value / 1000).toFixed(1)} Mbit/s`; },
    onchange: (e) => patch({ camera_bitrate_kbps: parseInt(e.target.value, 10) }),
  });
  return row('Quality (bitrate)', 'Higher is sharper but needs more bandwidth. Automatic picks 4 Mbit/s for 720p over USB and 2.5 Mbit/s over Wi-Fi.',
    h('div', { class: 'cam-rate' }, range, out, switchBtn('cam-bitrate-auto', auto, () => patch({ camera_bitrate_kbps: auto ? 4000 : 0 }))));
}

function liveCard(c) {
  if (c.state !== 'streaming') return null;
  const zoomOut = h('output', { id: 'cam-zoom-out' }, `${Number(c.zoom || 1).toFixed(1)}x`);
  const sendZoom = debounce((v) => control({ zoom: v }), 120);
  return h('section', { class: 'card sect' },
    h('h2', null, 'Live controls'),
    row('Torch', 'Phone flashlight.', switchBtn('cam-torch', c.torch, () => control({ torch: !c.torch }))),
    row('Zoom', null, h('div', { class: 'cam-rate' },
      h('input', { id: 'cam-zoom', type: 'range', min: 1, max: 8, step: 0.1, value: c.zoom || 1, 'aria-label': 'Zoom',
        oninput: (e) => { zoomOut.textContent = `${Number(e.target.value).toFixed(1)}x`; sendZoom(parseFloat(e.target.value)); } }), zoomOut)),
    row('Picture', 'Ask the phone for a fresh key frame if the picture looks glitchy.',
      h('button', { type: 'button', class: 'btn sm', onclick: () => control({ keyframe: true }) }, 'Refresh picture')));
}

function build(c) {
  const set = camSettings();
  const lensOpts = [['back', 'Back'], ['front', 'Front']];
  return [
    h('header', { class: 'page-head' }, h('h1', null, 'Camera'), h('small', { class: 'muted' }, 'Use the phone as a webcam: it starts by itself when an app opens “Notideck Camera”.')),
    h('section', { class: 'card cam-status' },
      h('div', { class: 'cam-status-text' },
        h('span', { class: 'statusline' }, h('i', { id: 'cam-dot', class: `dot ${dotClass(c)}` }), h('b', { id: 'cam-status' }, statusText(c))),
        h('small', { id: 'cam-stats', class: 'muted' }, statsText(c)),
        c.phone ? h('small', { class: 'muted' }, `${c.phone.name || 'Phone'} · ${linkLabel(c.phone.link)}`) : null),
      h('div', { class: 'cam-preview-box' }, h('img', { id: 'cam-preview', alt: 'Live preview', hidden: true }), h('span', { id: 'cam-noprev', class: 'muted' }, 'No picture'))),
    installCard(c),
    h('section', { class: 'card sect' },
      h('h2', null, 'Camera'),
      row('Lens', 'Applies instantly while streaming.', segmented('Lens', lensOpts, set.lens, (v) => { patch({ camera_lens: v }); })),
      row('Resolution', 'Automatic uses the smallest phone mode that covers what the app asks for.',
        h('select', { id: 'cam-mode', 'aria-label': 'Resolution', onchange: (e) => patch({ camera_mode: e.target.value }) },
          modeOptions(c).map(([v, l]) => h('option', { value: v, selected: v === set.mode }, l)))),
      bitrateRow(set),
      row('Mirror', 'Automatic mirrors the front lens like a selfie.', segmented('Mirror', [['auto', 'Auto'], ['on', 'On'], ['off', 'Off']], set.mirror, (v) => patch({ camera_mirror: v }))),
      row('Start with the app', 'Turn the phone camera on when an app opens Notideck Camera, off 5 seconds after it closes.', switchBtn('cam-auto', set.auto, () => patch({ camera_auto_start: !set.auto })))),
    liveCard(c),
    imageCard(c, { lens: set.lens, patch, rerender: () => render(true), row, switchBtn }),
    h('section', { class: 'card sect' },
      h('h2', null, 'Test'),
      row('Test camera', 'Streams 1280×720 without any app, so you can check the picture here.',
        switchBtn('cam-test', !!c.test, async () => {
          try { cam = await api.cameraTest(!c.test); } catch (e) { toast(e.message, 'error'); }
          render(true);
        }))),
  ].filter(Boolean);
}

function statsText(c) {
  if (!c || c.state !== 'streaming' || !c.stats) return c?.consumer ? `Consumer ${sizeLabel(c.consumer.width, c.consumer.height)}` : '';
  const s = c.stats;
  return `in ${s.fps_in} fps · out ${s.fps_out} fps · ${s.kbps} kbit/s · decode ${s.decode_ms} ms · dropped ${s.dropped}`;
}

/** Update only the live bits, without rebuilding (keeps sliders and focus). */
function updateLive() {
  if (!cam) return;
  const set = (id, fn) => { const el = document.getElementById(id); if (el) fn(el); };
  set('cam-status', (el) => { el.textContent = statusText(cam); });
  set('cam-stats', (el) => { el.textContent = statsText(cam); });
  set('cam-dot', (el) => { el.className = `dot ${dotClass(cam)}`; });
  set('cam-img-readout', (el) => { el.textContent = readoutText(cam.readout); el.hidden = !cam.readout; });
}

export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  const s = camSettings();
  const next = JSON.stringify([
    cam?.state, cam?.message, cam?.installed, cam?.install, cam?.dll_present, cam?.phone?.session_id, cam?.phone?.supported_modes?.length,
    cam?.test, cam?.paused, cam?.torch, cam?.active_mode, cam?.consumer?.source, !!cam?.phone, s, imageSig(cam, s.lens),
  ]);
  const busy = el.contains(document.activeElement) && ['INPUT', 'SELECT'].includes(document.activeElement.tagName);
  if (force || (next !== sig && !busy)) {
    sig = next;
    const focusId = el.contains(document.activeElement) ? document.activeElement.id : '';
    el.replaceChildren(...(cam ? build(cam) : [h('header', { class: 'page-head' }, h('h1', null, 'Camera')), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…')]));
    if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  }
  updateLive();
}

async function load() {
  try {
    const [c] = await Promise.all([api.camera(), S.settings ? null : api.settings().then((v) => { S.settings = v; })]);
    cam = c;
    failed = false;
  } catch {
    failed = true;
  }
  render();
  refreshStatus();
}

function tickPreview() {
  const img = document.getElementById('cam-preview');
  if (!img || document.hidden) return;
  const on = cam && cam.state === 'streaming' && !cam.paused;
  if (!on) { img.hidden = true; document.getElementById('cam-noprev')?.removeAttribute('hidden'); return; }
  const probe = new Image();
  probe.onload = () => { img.src = probe.src; img.hidden = false; document.getElementById('cam-noprev')?.setAttribute('hidden', ''); };
  probe.src = api.cameraPreviewUrl();
}

export function startCameraPolling() {
  stopCameraPolling();
  load();
  timer = setInterval(() => { if (!document.hidden) load(); }, 1500);
  previewTimer = setInterval(tickPreview, 500);
}

export function stopCameraPolling() {
  clearInterval(timer);
  clearInterval(previewTimer);
  timer = 0;
  previewTimer = 0;
}

export const renderCamera = render;
