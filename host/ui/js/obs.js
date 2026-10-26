// SPDX-License-Identifier: GPL-3.0-or-later
// OBS tab: connection status with plain-language fixes, connection settings, templates.
// Also the shared GET /api/obs cache that the inspector pickers read.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { S, addObsPages, subscribe } from './store.js';
import { openNewProfile } from './sidebar.js';

let st = null;        // last GET /api/obs
let missing = false;  // the agent has no OBS routes (404) or reports no OBS integration
let failed = false;   // the agent is not answering
let timer = 0;
let fetchedAt = 0;
const watchers = new Set();

const root = () => $('#view-obs');

// ---- shared info for the inspector ----

/** Latest OBS info, or null when unknown / unavailable. */
export const obsInfo = () => (missing ? null : st);
export const obsAvailable = () => !missing;

/** Fetch (at most every 4 s unless forced) and tell the watchers. Resolves with the info or null. */
export async function refreshObs(force = false) {
  if (!force && Date.now() - fetchedAt < 4000 && (st || missing)) return obsInfo();
  fetchedAt = Date.now();
  try {
    const v = await api.obs();
    st = v;
    missing = !v || v.settings == null;
    failed = false;
  } catch (e) {
    if (e.status === 404) { missing = true; failed = false; } else failed = true;
  }
  watchers.forEach((f) => f());
  return obsInfo();
}

/** Subscribe to info refreshes; returns an unsubscribe. */
export function watchObs(fn) {
  watchers.add(fn);
  return () => watchers.delete(fn);
}

// ---- view ----

let draft = null;        // editable settings
let pwMode = 'keep';     // keep | clear | new
let draftDirty = false;
let busy = false;
let enabledNote = '';    // shown after "Enable it for me"

const STATE_TEXT = {
  disabled: ['OBS integration is off', 'Turn it on below and Notideck will connect to OBS Studio.'],
  connecting: ['Connecting to OBS…', 'This usually takes a second.'],
  offline: ["OBS isn't running", 'Start OBS Studio and Notideck connects by itself.'],
  server_disabled: ["OBS's WebSocket server is off", 'Notideck needs it to talk to OBS. It is a single setting in OBS.'],
  auth_failed: ['Wrong password', "OBS rejected the password. Use OBS's own settings (below), or type the password you set in OBS under Tools > WebSocket Server Settings."],
  connected: ['Connected to OBS', ''],
};

function row(label, hint, control) {
  return h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, label), hint ? h('small', null, hint) : null), control);
}

function switchBtn(id, on, onclick, disabled = false) {
  return h('button', { id, type: 'button', class: 'switch', role: 'switch', 'aria-checked': on ? 'true' : 'false', disabled, onclick }, h('i'));
}

async function act(fn, okMsg) {
  if (busy) return;
  busy = true;
  try {
    const r = await fn();
    if (okMsg) toast(okMsg);
    return r ?? true;
  } catch (e) {
    toast(`OBS: ${e.message}`, 'error');
  } finally {
    busy = false;
  }
}

async function enableServer() {
  const ok = await act(() => api.obsEnableServer());
  if (ok) {
    enabledNote = 'Done. Start OBS, or restart it if it is already open, and Notideck will connect.';
    toast('WebSocket server enabled. Start or restart OBS.');
  }
  await refreshObs(true);
  renderStatus(true);
}

const detectedHint = (d) => d && d.exists
  ? `OBS's settings were found${d.server_enabled ? ` (WebSocket on, port ${d.server_port})` : ' (WebSocket off)'}.`
  : '';

function statusCard() {
  const s = st.status || 'disabled';
  const [title, hint] = STATE_TEXT[s] || [s, ''];
  const det = st.detected;
  const body = [];
  const dot = s === 'connected' ? 'ok' : s === 'connecting' || s === 'disabled' ? '' : 'warn';
  body.push(h('div', { class: 'obs-head' },
    h('span', { class: `dot ${dot}`, 'aria-hidden': 'true' }),
    h('div', null, h('b', { id: 'obs-state' }, title), hint ? h('p', { class: 'muted' }, hint) : null)));

  if (s === 'connected') {
    body.push(h('p', { class: 'obs-facts' },
      [st.obs_version ? `OBS ${st.obs_version}` : null, `${(st.scenes || []).length} scenes`, `${(st.inputs || []).length} inputs`,
        st.studio_mode ? 'studio mode' : null, st.program_scene ? `live: ${st.program_scene}` : null].filter(Boolean).join(' · ')));
  }
  const actions = [];
  if (s === 'server_disabled' || (s === 'offline' && det?.exists && det.server_enabled === false)) {
    actions.push(h('button', { id: 'obs-enable', type: 'button', class: 'btn primary', disabled: busy, onclick: enableServer }, 'Enable it for me'));
    body.push(h('p', { class: 'muted' }, 'This turns on the WebSocket server in OBS\'s settings file. Then start OBS, or restart it if it is open.'));
  }
  if (s === 'auth_failed') {
    actions.push(h('a', { class: 'btn', href: '#obs-manual', onclick: (e) => { e.preventDefault(); $('#obs-settings')?.scrollIntoView({ block: 'start' }); } }, 'Manual settings'));
  }
  if (s !== 'disabled' && s !== 'connected') {
    actions.push(h('button', { id: 'obs-reconnect', type: 'button', class: 'btn', disabled: busy, onclick: async () => { await act(() => api.obsReconnect(), 'Reconnecting…'); refreshObs(true); } }, 'Reconnect'));
  }
  if (s === 'disabled') {
    actions.push(h('button', { id: 'obs-turn-on', type: 'button', class: 'btn primary', disabled: busy, onclick: () => saveSettings({ enabled: true }) }, 'Turn on'));
  }
  if (actions.length) body.push(h('div', { class: 'obs-actions' }, actions));
  if (enabledNote && s !== 'connected') body.push(h('p', { class: 'obs-note', role: 'status' }, enabledNote));

  if (det === null && s !== 'connected' && s !== 'disabled') {
    body.push(h('div', { class: 'obs-howto', id: 'obs-howto' },
      h('b', null, "OBS doesn't seem to be installed, or has never been opened"),
      h('ol', null,
        h('li', null, 'Install OBS Studio (free, from obsproject.com).'),
        h('li', null, 'Open OBS once, then close it.'),
        h('li', null, 'Come back here and press Enable it for me, or just start OBS.'))));
  } else if (det && s !== 'connected') {
    body.push(h('p', { class: 'muted obs-small' }, detectedHint(det)));
  }
  if (st.last_error && s !== 'connected') body.push(h('p', { class: 'muted obs-small' }, `Last error: ${st.last_error}`));
  return h('section', { class: `card obs-status s-${s}`, id: 'obs-status', 'aria-live': 'polite' }, ...body);
}

let statusSig = '';
function renderStatus(force = false) {
  const box = $('#obs-status-slot');
  if (!box || !st) return;
  const sig = JSON.stringify([st.status, st.obs_version, (st.scenes || []).length, (st.inputs || []).length, st.studio_mode, st.program_scene, st.detected, st.last_error, enabledNote, busy]);
  if (!force && sig === statusSig) return;
  statusSig = sig;
  const focusId = box.contains(document.activeElement) ? document.activeElement.id : '';
  box.replaceChildren(statusCard());
  if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
}

async function saveSettings(patch = {}) {
  const d = { ...draft, ...patch };
  const body = { enabled: !!d.enabled, auto_detect: !!d.auto_detect, host: (d.host || '127.0.0.1').trim(), port: parseInt(d.port, 10) || 4455 };
  if (pwMode === 'clear') body.password = '';
  else if (pwMode === 'new' && d.password) body.password = d.password;
  const ok = await act(() => api.obsConfig(body), 'OBS settings saved');
  if (!ok) return;
  draftDirty = false;
  draft = null;
  pwMode = 'keep';
  await refreshObs(true);
  buildSettings();
  renderStatus(true);
}

function initDraft() {
  const s = st.settings || {};
  draft = { enabled: !!s.enabled, auto_detect: s.auto_detect !== false, host: s.host || '127.0.0.1', port: s.port || 4455, has_password: !!s.has_password, password: '' };
}

function buildSettings() {
  const box = $('#obs-settings');
  if (!box || !st) return;
  if (!draft) initDraft();
  const manual = !draft.auto_detect;
  const dirty = () => { draftDirty = true; $('#obs-dirty')?.removeAttribute('hidden'); };
  const pwInput = h('input', {
    id: 'obs-password', type: 'password', autocomplete: 'new-password', spellcheck: 'false', disabled: !manual,
    placeholder: pwMode === 'clear' ? 'Will be cleared' : draft.has_password ? 'set (leave empty to keep)' : 'no password',
    value: pwMode === 'new' ? draft.password : '',
    oninput: (e) => { draft.password = e.target.value; pwMode = e.target.value ? 'new' : (pwMode === 'clear' ? 'clear' : 'keep'); dirty(); },
  });
  box.replaceChildren(
    h('h2', null, 'Connection'),
    row('Connect to OBS', 'Switch this off to stop talking to OBS.',
      switchBtn('obs-enabled', draft.enabled, (e) => { draft.enabled = !draft.enabled; e.currentTarget.setAttribute('aria-checked', String(draft.enabled)); dirty(); })),
    row("Use OBS's own settings (recommended)", 'Reads the port and password from OBS, so there is nothing to type.',
      switchBtn('obs-auto', draft.auto_detect, () => { draft.auto_detect = !draft.auto_detect; dirty(); buildSettings(); })),
    h('div', { class: `obs-manual${manual ? '' : ' off'}`, id: 'obs-manual' },
      h('div', { class: 'row2' },
        h('div', { class: 'field' }, h('label', { for: 'obs-host' }, 'Host'),
          h('input', { id: 'obs-host', type: 'text', value: draft.host, disabled: !manual, spellcheck: 'false', oninput: (e) => { draft.host = e.target.value; dirty(); } })),
        h('div', { class: 'field' }, h('label', { for: 'obs-port' }, 'Port'),
          h('input', { id: 'obs-port', type: 'number', min: 1, max: 65535, value: draft.port, disabled: !manual, inputmode: 'numeric', oninput: (e) => { draft.port = e.target.value; dirty(); } }))),
      h('div', { class: 'field' }, h('label', { for: 'obs-password' }, `Password${draft.has_password && pwMode === 'keep' ? ' (set)' : ''}`),
        h('div', { class: 'obs-pw' }, pwInput,
          draft.has_password || pwMode === 'new' ? h('button', { id: 'obs-pw-clear', type: 'button', class: 'btn sm', disabled: !manual || pwMode === 'clear',
            onclick: () => { pwMode = 'clear'; draft.password = ''; dirty(); buildSettings(); } }, 'Clear') : null,
          pwMode === 'clear' ? h('button', { type: 'button', class: 'btn sm', onclick: () => { pwMode = 'keep'; buildSettings(); } }, 'Undo') : null),
        h('small', null, 'Stored on this PC only. It is never shown again.')),
      st.endpoint ? h('p', { class: 'muted obs-small' }, `Currently using ${st.endpoint}${st.from_detected ? " (from OBS's settings)" : ''}.`) : null),
    h('div', { class: 'as-bar' },
      h('button', { id: 'obs-save', type: 'button', class: 'btn primary', disabled: busy, onclick: () => saveSettings() }, 'Save'),
      h('span', { id: 'obs-dirty', class: 'as-dirty', hidden: !draftDirty }, 'Unsaved changes')));
}

function templatesCard() {
  const has = !!S.profile;
  return h('section', { class: 'card sect', id: 'obs-templates' },
    h('h2', null, 'Templates'),
    h('p', { class: 'muted', style: 'padding: 6px 0 10px' }, 'Ready-made keys for OBS. They use scene slots (the first scene in OBS is slot 1) and OBS\'s default mic and desktop audio, so they work with any OBS setup. No editing needed.'),
    h('div', { class: 'obs-tpl' },
      h('button', { id: 'obs-new-profile', type: 'button', class: 'btn primary', onclick: () => openNewProfile('obs') }, 'New profile from OBS template'),
      h('button', { id: 'obs-add-page', type: 'button', class: 'btn', disabled: !has, title: has ? null : 'Open a profile on the Deck tab first.', onclick: () => addTo('page') }, 'Add OBS page'),
      h('button', { id: 'obs-add-audio', type: 'button', class: 'btn', disabled: !has, title: has ? null : 'Open a profile on the Deck tab first.', onclick: () => addTo('audio') }, 'Add OBS audio page')),
    has ? h('small', { class: 'muted', style: 'padding: 8px 0 12px' }, `Pages are added to the profile "${S.profile.name}".`) : null);
}

async function addTo(which) {
  if (busy) return;
  busy = true;
  try {
    await addObsPages(which);
    toast(which === 'audio' ? 'Added the OBS audio page.' : 'Added the OBS page. Find it on the Deck tab.');
  } catch (e) {
    toast(`Could not add the page: ${e.message}`, 'error');
  }
  busy = false;
}

function shell() {
  const el = root();
  el.replaceChildren(
    h('header', { class: 'page-head' }, h('h1', null, 'OBS'),
      h('small', { class: 'muted' }, 'Control OBS Studio from the deck: scenes, recording, streaming, mic and desktop audio.')),
    h('div', { id: 'obs-status-slot' }),
    h('section', { class: 'card sect', id: 'obs-settings' }),
    h('div', { id: 'obs-tpl-slot' }, templatesCard()));
}

export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  if (!st && !missing) {
    el.replaceChildren(h('header', { class: 'page-head' }, h('h1', null, 'OBS')), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…'));
    return;
  }
  if (missing) {
    el.replaceChildren(h('header', { class: 'page-head' }, h('h1', null, 'OBS')),
      h('section', { class: 'card obs-unavailable', id: 'obs-unavailable' }, h('b', null, 'OBS integration unavailable'),
        h('p', { class: 'muted' }, 'This version of the Notideck agent has no OBS support, or it is switched off. You can still add the OBS templates; their keys appear dimmed until the agent can reach OBS.')),
      h('div', { id: 'obs-tpl-slot' }, templatesCard()));
    return;
  }
  if (!$('#obs-status-slot')) { shell(); statusSig = ''; buildSettings(); }
  renderStatus(force);
  const typing = el.contains(document.activeElement) && ['INPUT', 'SELECT'].includes(document.activeElement.tagName);
  if (!typing) {
    const slot = $('#obs-tpl-slot');
    const sig = S.profile ? S.profile.id + S.profile.name : '';
    if (slot && slot.dataset.sig !== sig) { slot.dataset.sig = sig; slot.replaceChildren(templatesCard()); }
  }
}

async function load() {
  await refreshObs(true);
  render();
}

export function startObs() {
  stopObs();
  statusSig = '';
  draft = null;
  draftDirty = false;
  pwMode = 'keep';
  root()?.replaceChildren();
  render();
  load().then(() => { if (st && !missing) { if (!draftDirty) buildSettings(); } });
  timer = setInterval(() => { if (!document.hidden) load(); }, 2000);
}

export function stopObs() {
  clearInterval(timer);
  timer = 0;
}

subscribe(() => { if (timer) render(); });

export const renderObs = render;
