// SPDX-License-Identifier: GPL-3.0-or-later
// Auto-switch tab: pick the deck profile (and page) from the focused app.

import { h, $, toast } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';
import { S, refreshList } from './store.js';

let st = null;          // last GET /api/autoswitch
let draft = null;       // editable copy of st.config
let dirty = false;
let errors = {};        // rule_id (or '' for the fallback) -> [message]
let failed = false;
let timer = 0;
let busy = false;
const pageCache = new Map(); // profile id -> [{ id, name }]

const root = () => $('#view-autoswitch');
const KINDS = [['process', 'App'], ['class', 'Window class'], ['title', 'Title regex']];
const newId = () => (globalThis.crypto?.randomUUID?.() ?? `r${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`);
const profiles = () => S.profiles ?? [];
const appLabel = (a) => a.process || a.class || '(unknown)';

async function pagesOf(pid) {
  if (!pid) return [];
  if (pageCache.has(pid)) return pageCache.get(pid);
  try {
    const p = await api.profile(pid);
    const pages = (p.pages ?? []).map((pg) => ({ id: pg.id, name: pg.name || pg.id }));
    pageCache.set(pid, pages);
    return pages;
  } catch { return []; }
}

function profileSelect(value, onchange, { none, id } = {}) {
  const known = profiles().some((p) => p.id === value);
  return h('select', { id, onchange: (e) => onchange(e.target.value) },
    none ? h('option', { value: '', selected: !value }, none) : null,
    !known && value ? h('option', { value, selected: true }, `${value} (missing)`) : null,
    profiles().map((p) => h('option', { value: p.id, selected: p.id === value }, p.name || p.id)));
}

function markDirty() {
  dirty = true;
  const el = document.getElementById('as-dirty');
  if (el) el.hidden = false;
}

function ruleRow(rule, i) {
  const missing = !!rule.profile_id && !profiles().some((p) => p.id === rule.profile_id);
  const errs = errors[rule.id] ?? [];
  const move = (d) => { const j = i + d; if (j < 0 || j >= draft.rules.length) return; [draft.rules[i], draft.rules[j]] = [draft.rules[j], draft.rules[i]]; markDirty(); render(true); };
  const pageSel = h('select', { 'aria-label': 'Page', onchange: (e) => { rule.page_id = e.target.value || undefined; markDirty(); } },
    h('option', { value: '' }, 'Remember last page'));
  pagesOf(rule.profile_id).then((pages) => {
    for (const pg of pages) pageSel.append(h('option', { value: pg.id, selected: pg.id === rule.page_id }, pg.name));
    if (rule.page_id && !pages.some((pg) => pg.id === rule.page_id) && pages.length) pageSel.append(h('option', { value: rule.page_id, selected: true }, `${rule.page_id} (missing)`));
  });
  return h('li', { class: `as-rule${errs.length || missing ? ' bad' : ''}`, dataset: { rule: rule.id } },
    h('input', { type: 'checkbox', checked: rule.enabled !== false, 'aria-label': 'Rule enabled', onchange: (e) => { rule.enabled = e.target.checked; markDirty(); } }),
    h('select', { 'aria-label': 'Match', onchange: (e) => { rule.kind = e.target.value; markDirty(); } },
      KINDS.map(([v, l]) => h('option', { value: v, selected: v === rule.kind }, l))),
    h('input', { type: 'text', value: rule.pattern, placeholder: rule.kind === 'title' ? 'e.g. (?i)Figma' : rule.kind === 'class' ? 'e.g. Navigator' : 'e.g. obs64', 'aria-label': 'Pattern', spellcheck: 'false', oninput: (e) => { rule.pattern = e.target.value; markDirty(); } }),
    profileSelect(rule.profile_id, (v) => { rule.profile_id = v; rule.page_id = undefined; markDirty(); render(true); }),
    pageSel,
    h('div', { class: 'as-actions' },
      h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Move up', disabled: i === 0, onclick: () => move(-1) }, '↑'),
      h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Move down', disabled: i === draft.rules.length - 1, onclick: () => move(1) }, '↓'),
      h('button', { type: 'button', class: 'btn sm danger', 'aria-label': 'Delete rule', onclick: () => { draft.rules.splice(i, 1); markDirty(); render(true); } }, 'Delete')),
    missing ? h('div', { class: 'as-msg' }, 'This profile no longer exists. The rule is skipped until you pick another one.') : null,
    errs.length ? h('div', { class: 'as-msg', role: 'alert' }, errs.map((m) => h('span', null, m))) : null,
    h('div', { class: 'as-badge', hidden: true, dataset: { badge: rule.id } }, 'Active now'));
}

function addRule(app) {
  const profile = profiles().find((p) => p.active) ?? profiles()[0];
  const pattern = app ? (app.process || app.class).replace(/\.exe$/i, '') : '';
  draft.rules.push({ id: newId(), enabled: true, kind: app && !app.process ? 'class' : 'process', pattern: app && !app.process ? app.class : pattern, profile_id: profile?.id ?? '' });
  markDirty();
  render(true);
}

function row(label, hint, control) {
  return h('div', { class: 'srow' }, h('div', null, h('span', { class: 'k' }, label), hint ? h('small', null, hint) : null), control);
}

async function setLocked(locked) {
  try { await api.autoswitchLock(locked); st = { ...st, locked }; } catch (e) { toast(`Could not change the lock: ${e.message}`, 'error'); }
  render(true);
  load();
}

async function save() {
  if (busy) return;
  busy = true;
  try {
    await api.saveAutoswitch(draft);
    errors = {};
    dirty = false;
    toast('Auto-switch saved');
  } catch (e) {
    errors = {};
    const list = e.body?.errors;
    if (Array.isArray(list)) {
      for (const er of list) (errors[er.rule_id ?? ''] ||= []).push(er.message);
      toast('Some rules need fixing', 'error');
    } else toast(`Could not save: ${e.message}`, 'error');
  }
  busy = false;
  await load();
  render(true);
}

function recentCard() {
  const apps = [];
  const seen = new Set();
  for (const a of [st.current, ...(st.recent ?? [])]) {
    if (!a) continue;
    const key = `${a.process}|${a.class}`;
    if (seen.has(key) || (!a.process && !a.class)) continue;
    seen.add(key);
    apps.push(a);
  }
  return h('section', { class: 'card sect' },
    h('h2', null, 'Pick an app'),
    h('p', { class: 'muted', style: 'padding: 6px 0 4px' }, apps.length ? 'Click an app you used recently to add a rule for it.' : 'Switch to another window and it will show up here.'),
    h('div', { class: 'as-recent', id: 'as-recent' }, apps.map((a) => h('button', { type: 'button', class: 'btn as-app', onclick: () => addRule(a) },
      h('b', null, appLabel(a)), h('small', null, [a.class && a.class !== a.process ? a.class : '', a.title].filter(Boolean).join(' · ') || ' ')))));
}

function build() {
  const fb = draft.fallback_profile_id ?? '';
  const unavailable = !st.available
    ? h('div', { class: 'as-banner', role: 'status' }, icon('warn'),
      h('p', null, h('b', null, "Auto-switch isn't available on this desktop"),
        h('small', null, `Backend: ${st.backend || 'none'}${st.error ? ` · ${st.error}` : ''}`),
        h('small', null, 'You can still edit rules here; they apply once a focus backend works.')))
    : null;
  return [
    h('header', { class: 'page-head' }, h('h1', null, 'Auto-switch'), h('small', { class: 'muted' }, 'Change the deck profile by the app you are using. The first matching rule wins.')),
    unavailable,
    h('section', { class: 'card sect' },
      h('h2', null, 'General'),
      row('Auto-switch', 'Switch profiles when the focused app changes.',
        h('button', { id: 'as-enabled', type: 'button', class: 'switch', role: 'switch', 'aria-checked': draft.enabled ? 'true' : 'false', onclick: () => { draft.enabled = !draft.enabled; markDirty(); render(true); } }, h('i'))),
      row('Lock the current profile', 'Pause switching until you unlock. Applies immediately.',
        h('button', { id: 'as-lock', type: 'button', class: 'switch', role: 'switch', 'aria-checked': st.locked ? 'true' : 'false', onclick: () => setLocked(!st.locked) }, h('i'))),
      row('Main profile', 'Shown by the "Main menu" deck key.',
        h('div', null, profileSelect(draft.home_profile_id ?? '', (v) => { draft.home_profile_id = v || undefined; markDirty(); render(true); }, { none: 'Fallback, else first profile', id: 'as-home' }),
          errors[''] ? h('small', { class: 'warn', role: 'alert' }, errors[''].filter((m) => /main profile/.test(m)).join(' ')) : null)),
      row('When no rule matches', 'Profile to show for other apps.',
        h('div', null, profileSelect(fb, (v) => { draft.fallback_profile_id = v || undefined; markDirty(); render(true); }, { none: 'Stay on current profile', id: 'as-fallback' }),
          errors[''] && errors[''].some((m) => !/main profile/.test(m)) ? h('small', { class: 'warn', role: 'alert' }, errors[''].filter((m) => !/main profile/.test(m)).join(' ')) : null)),
      h('div', { class: 'as-now' }, h('span', { class: 'k' }, 'Focused now'), h('small', { id: 'as-current', class: 'muted' }, currentText()))),
    h('section', { class: 'card sect' },
      h('h2', null, 'Rules'),
      draft.rules.length ? h('ol', { class: 'as-rules', id: 'as-rules', style: 'list-style: none; margin: 0; padding: 6px 0 12px' }, draft.rules.map(ruleRow))
        : h('p', { class: 'muted', style: 'padding: 10px 0 14px' }, 'No rules yet. Add one, or pick an app below.'),
      h('div', { class: 'as-bar' },
        h('button', { id: 'as-add', type: 'button', class: 'btn', onclick: () => addRule(null) }, 'Add rule'),
        h('button', { id: 'as-save', type: 'button', class: 'btn primary', onclick: save }, 'Save'),
        h('span', { id: 'as-dirty', class: 'as-dirty', hidden: !dirty }, 'Unsaved changes'))),
    recentCard(),
  ].filter(Boolean);
}

function currentText() {
  const c = st?.current;
  if (!c) return 'Nothing yet';
  return [c.process, c.class && c.class !== c.process ? c.class : '', c.title].filter(Boolean).join(' · ');
}

function updateLive() {
  if (!st) return;
  const cur = document.getElementById('as-current');
  if (cur) cur.textContent = currentText();
  const lock = document.getElementById('as-lock');
  if (lock) lock.setAttribute('aria-checked', st.locked ? 'true' : 'false');
  const active = st.active_rule_id && st.available ? st.active_rule_id : null;
  for (const li of root().querySelectorAll('.as-rule')) {
    const on = li.dataset.rule === active;
    li.classList.toggle('active', on);
    li.querySelector('.as-badge')?.toggleAttribute('hidden', !on);
  }
}

let sig = '';
export function render(force = false) {
  const el = root();
  if (!el || el.hidden) return;
  if (!st) {
    el.replaceChildren(h('header', { class: 'page-head' }, h('h1', null, 'Auto-switch')), h('p', { class: 'muted' }, failed ? 'The agent is not answering.' : 'Loading…'));
    return;
  }
  const next = JSON.stringify([st.available, st.backend, st.error, profiles().map((p) => p.id + p.name), (st.recent ?? []).map((a) => a.process + a.class), st.current?.process, st.current?.class]);
  const typing = el.contains(document.activeElement) && ['INPUT', 'SELECT'].includes(document.activeElement.tagName);
  if (force || (next !== sig && !typing)) {
    sig = next;
    const focusId = el.contains(document.activeElement) ? document.activeElement.id : '';
    el.replaceChildren(...build());
    if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  }
  updateLive();
}

async function load() {
  try {
    const [v] = await Promise.all([api.autoswitch(), profiles().length ? null : refreshList()]);
    st = v;
    if (!dirty) draft = structuredClone(v.config);
    else if (!draft) draft = structuredClone(v.config);
    failed = false;
  } catch {
    failed = true;
  }
  render(!dirty && draft && sigConfig !== JSON.stringify(draft));
  sigConfig = draft ? JSON.stringify(draft) : '';
}
let sigConfig = '';

export function startAutoswitch() {
  stopAutoswitch();
  pageCache.clear();
  load();
  timer = setInterval(() => { if (!document.hidden) load(); }, 2000);
}

export function stopAutoswitch() {
  clearInterval(timer);
  timer = 0;
}

export const renderAutoswitch = render;
