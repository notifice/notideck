// SPDX-License-Identifier: GPL-3.0-or-later
// Deck toolbar: profile switcher, new profile, push to phone, save state.

import { h, $, toast } from './dom.js';
import { APP_TEMPLATES } from './model.js';
import { S, openProfile, createProfileFromTemplate, activateProfile } from './store.js';
import { go, tab } from './shell.js';

async function guard(fn) {
  try { await fn(); } catch (e) { toast(e.message, 'error'); }
}

export const isLive = () => !!S.profile && S.profiles.some((p) => p.id === S.profile.id && p.active);

export function renderToolbar() {
  const bar = $('#toolbar');
  const focusId = bar.contains(document.activeElement) ? document.activeElement.id : null;
  bar.replaceChildren();

  if (!S.loaded) {
    bar.append(h('span', { class: 'muted' }, 'Loading…'));
    return;
  }
  if (S.fault) {
    bar.append(h('span', { class: 'warn' }, `Agent unreachable: ${S.fault}`), h('button', { class: 'btn', type: 'button', onclick: () => location.reload() }, 'Retry'));
    return;
  }

  const sel = h('select', {
    id: 'profile-select', 'aria-label': 'Profile', disabled: !S.profiles.length,
    onchange: (e) => guard(() => openProfile(e.target.value)),
  }, S.profiles.map((p) => h('option', { value: p.id, selected: S.profile?.id === p.id }, `${p.name}${p.active ? '  • live' : ''}`)));
  if (S.profile) sel.value = S.profile.id;

  const save = { idle: '', writing: 'Saving…', synced: 'Saved', fault: 'Save failed' }[S.save];
  bar.append(
    h('label', { class: 'tb-lab', for: 'profile-select' }, 'Profile'),
    sel,
    h('button', { id: 'new-profile', type: 'button', class: 'btn', onclick: () => openNewProfile() }, 'New profile'),
    h('span', { class: `savestate ${S.save}`, role: 'status' }, save),
    S.profile ? h('button', { id: 'push-btn', type: 'button', class: 'btn primary', onclick: () => guard(async () => {
      await activateProfile(S.profile.id);
      toast(`${S.profile.name} is live on the phone.`);
    }) }, isLive() ? 'Re-push to phone' : 'Push to phone') : null,
  );
  if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
}

const APP_ORDER = ['vscode', 'discord', 'teamspeak', 'photoshop', 'gimp', 'premiere', 'resolve', 'spotify'];
/** [group, [[value, label]...]] for the template select; the first group has no label. */
export const TEMPLATE_GROUPS = [
  [null, [['blank', 'Blank']]],
  ['Streaming', [['obs', 'OBS Studio']]],
  ['Media', [['media', 'Media controls'], ['spotify', 'Spotify']]],
  ['Apps', APP_ORDER.filter((k) => k !== 'spotify').map((k) => [k, APP_TEMPLATES[k].label])],
];
export const PROFILE_TEMPLATES = TEMPLATE_GROUPS.flatMap(([, items]) => items);
const templateName = (v) => (v === 'obs' ? 'OBS Studio' : v === 'media' ? 'Media' : APP_TEMPLATES[v]?.name || '');
let dlg = null;

/** "New profile" dialog: name, template (blank by default) and, for OBS, the auto-switch rule. */
export function openNewProfile(template = 'blank') {
  if (dlg) return;
  let busy = false;
  const fromObs = tab === 'obs';
  const name = h('input', { id: 'np-name', type: 'text', maxlength: 32, autocomplete: 'off', value: templateName(template), placeholder: 'e.g. Streaming', 'aria-label': 'Profile name' });
  const names = new Set(PROFILE_TEMPLATES.map(([v]) => templateName(v)));
  const tpl = h('select', { id: 'np-template', 'aria-label': 'Template', onchange: () => {
    sync();
    if (!name.value.trim() || names.has(name.value)) name.value = templateName(tpl.value);
  } });
  tpl.replaceChildren(...TEMPLATE_GROUPS.map(([g, items]) => {
    const opts = items.map(([v, l]) => h('option', { value: v }, l));
    return g ? h('optgroup', { label: g }, opts) : opts;
  }).flat());
  tpl.value = [...tpl.options].some((o) => o.value === template) ? template : 'blank';
  const rule = h('input', { id: 'np-autoswitch', type: 'checkbox', checked: true });
  const ruleText = document.createTextNode('');
  const ruleRow = h('label', { class: 'check' }, rule, ruleText);
  const info = h('small', { class: 'muted', id: 'np-info' });
  const create = h('button', { id: 'np-create', type: 'button', class: 'btn primary', onclick: submit }, 'Create');
  const close = () => { dlg?.close(); dlg?.remove(); dlg = null; };
  function sync() {
    const obs = tpl.value === 'obs';
    const app = APP_TEMPLATES[tpl.value];
    ruleRow.hidden = !obs && !app;
    ruleText.nodeValue = ` Switch to this profile when ${obs ? 'OBS' : app?.app || 'the app'} is focused`;
    info.textContent = obs ? 'An OBS page (scenes, record, stream, mic, desktop audio, replay) and an audio page with two faders.'
      : app ? app.info : tpl.value === 'media' ? 'Two pages: a now-playing tile with transport, mute keys and a volume slider; then shuffle, repeat, a seek bar, skip keys and a player volume slider.' : 'An empty profile with one page.';
  }
  async function submit() {
    const n = name.value.trim().slice(0, 32);
    if (!n) { name.focus(); return; }
    if (busy) return;
    busy = true;
    create.disabled = true;
    create.textContent = 'Creating…';
    try {
      await createProfileFromTemplate(n, tpl.value, { autoswitch: rule.checked }); // also opens the new profile in the editor
      close();
      if (fromObs) go('deck');
    } catch (e) {
      toast(e.message, 'error');
      busy = false;
      create.disabled = false;
      create.textContent = 'Create';
    }
  }
  sync();
  dlg = h('dialog', { class: 'modal', 'aria-labelledby': 'np-title' },
    h('header', null, h('h2', { id: 'np-title' }, 'New profile')),
    h('form', { class: 'stack', onsubmit: (e) => { e.preventDefault(); submit(); } },
      h('div', { class: 'field' }, h('label', { for: 'np-name' }, 'Name'), name),
      h('div', { class: 'field' }, h('label', { for: 'np-template' }, 'Template'), tpl),
      info, ruleRow),
    h('footer', { class: 'np-foot' }, h('button', { type: 'button', class: 'btn', onclick: close }, 'Cancel'), create));
  dlg.addEventListener('cancel', (e) => { e.preventDefault(); if (!busy) close(); });
  document.body.append(dlg);
  dlg.showModal();
  name.focus();
  name.select();
}
