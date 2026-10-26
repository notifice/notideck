// SPDX-License-Identifier: GPL-3.0-or-later
// Single mutable state object, pub/sub, autosave ("live mirror") and mutations.

import { api } from './api.js';
import { toast } from './dom.js';
import { uid, defaultControl, gridOf, planMove, canResize, firstFree, slug, clamp, mixerPageControls, obsPageControls, obsAudioPageControls, obsAutoRule, OBS_GLYPHS, mediaPageControls, mediaExtraPageControls, MEDIA_GLYPHS, APP_TEMPLATES, appTemplateGlyphs, appTemplatePages, appAutoRules, PROFILE_GLYPHS, profilesPageControls, profileBarControls } from './model.js';
import { uploadGlyphs } from './obsicons.js';

export const S = {
  profiles: [],
  profile: null,
  rail: [], // pinned side-rail keys (global, not per profile)
  pageIdx: 0,
  sel: null, // { id } | { cell: {col,row} } | { railKey: id } | null
  status: null,
  host: null,
  settings: null,
  save: 'idle', // idle | writing | synced | fault
  loaded: false,
  fault: null,
  enter: true, // play entry animation on next preview render
  settleIds: [],
  focusSel: false,
};

const subs = new Set();
export const subscribe = (fn) => subs.add(fn);
/** kind: 'all' | 'preview' | 'sel' | 'meta' */
export const emit = (kind = 'all') => subs.forEach((f) => f(kind));

export const page = () => S.profile?.pages[S.pageIdx] ?? null;
export const grid = () => (S.profile ? gridOf(S.profile, page()) : { cols: 5, rows: 3 });
export const controls = () => page()?.controls ?? [];
export const selectedControl = () => {
  if (S.sel?.railKey) return S.rail.find((c) => c.id === S.sel.railKey) ?? null;
  return S.sel?.id ? controls().find((c) => c.id === S.sel.id) ?? null : null;
};
export const MAX_RAIL_KEYS = 3;

// ---- persistence ----

let timer = 0;
let dirty = null; // the profile object with unsaved edits

async function write() {
  const p = dirty;
  dirty = null;
  if (!p) return;
  try {
    await api.save(p);
    if (!dirty) S.save = 'synced';
  } catch (e) {
    S.save = 'fault';
    toast(`Save failed: ${e.message}`, 'error');
  }
  emit('meta');
}

/** Write any pending edit now (before switching profiles or pushing). */
export async function flushNow() {
  clearTimeout(timer);
  timer = 0;
  await write();
  await flushRail();
}

/** Drop pending edits for a profile that is going away. */
function discard(p) {
  if (dirty === p) {
    dirty = null;
    clearTimeout(timer);
    timer = 0;
  }
}

/** Mark the profile changed. kind 'preview' skips re-rendering the inspector. */
export function touch(kind = 'preview') {
  if (S.sel?.railKey) return touchRail(kind); // the inspector is editing a rail key
  S.save = 'writing';
  dirty = S.profile;
  clearTimeout(timer);
  timer = setTimeout(write, 400);
  emit(kind);
}

// ---- side rail persistence ----

let railTimer = 0;
let railDirty = false;

async function writeRail() {
  if (!railDirty) return;
  railDirty = false;
  try {
    await api.saveRail(S.rail);
    if (!railDirty && !dirty) S.save = 'synced';
  } catch (e) {
    S.save = 'fault';
    toast(`Rail save failed: ${e.body?.errors?.join(' ') || e.message}`, 'error');
  }
  emit('meta');
}

/** Mark the rail changed (autosaved like profiles; the agent pushes it to the phone). */
export function touchRail(kind = 'preview') {
  S.save = 'writing';
  railDirty = true;
  clearTimeout(railTimer);
  railTimer = setTimeout(writeRail, 400);
  emit(kind);
}

export async function flushRail() {
  clearTimeout(railTimer);
  await writeRail();
}

export function addRailKey() {
  if (S.rail.length >= MAX_RAIL_KEYS) return toast(`The rail holds up to ${MAX_RAIL_KEYS} keys.`, 'error');
  const c = defaultControl('key', 0, 0, S.profile);
  c.id = uid('r');
  normalizeControl(c);
  S.rail.push(c);
  S.settleIds = [];
  S.sel = { railKey: c.id };
  touchRail('all');
}

export function removeRailKey(id) {
  const i = S.rail.findIndex((c) => c.id === id);
  if (i < 0) return;
  S.rail.splice(i, 1);
  S.sel = null;
  touchRail('all');
}

export function moveRailKey(id, dir) {
  const i = S.rail.findIndex((c) => c.id === id);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= S.rail.length) return;
  [S.rail[i], S.rail[j]] = [S.rail[j], S.rail[i]];
  touchRail('all');
}

// ---- loading ----

export async function refreshList() {
  S.profiles = await api.profiles();
}

export async function refreshStatus() {
  try {
    S.status = await api.status();
  } catch {
    S.status = null;
  }
  emit('meta');
}

export async function refreshHost() {
  try { S.host = await api.host(); } catch { /* older agent */ }
}

export async function refreshSettings() {
  try { S.settings = await api.settings(); } catch { /* older agent */ }
}

export async function boot() {
  try {
    await refreshList();
    await refreshStatus();
    S.fault = null;
    try { S.rail = ((await api.rail()).keys || []).map(normalizeControl); } catch { /* older agent: no rail */ }
    const first = S.profiles.find((p) => p.active) || S.profiles[0];
    if (first) await openProfile(first.id, false);
  } catch (e) {
    S.fault = e.message;
  }
  S.loaded = true;
  emit('all');
}

function normalizeAction(a) {
  if (a.type === 'launch_app') {
    a.args = a.args || [];
    a.confirmed = !!a.confirmed;
  } else if (a.type === 'hotkey') {
    a.combo = a.combo || { modifiers: [], key: 'f13' };
    a.combo.modifiers = a.combo.modifiers || [];
  }
}

function normalizeControl(c) {
  c.span_w = c.span_w || 1;
  c.span_h = c.span_h || 1;
  c.actions = c.actions || [];
  for (const def of c.actions) {
    normalizeAction(def.action);
    if (def.action.type === 'multi') {
      def.action.steps = def.action.steps || [];
      for (const st of def.action.steps) {
        st.delay_ms = st.delay_ms || 0;
        normalizeAction(st.action);
      }
    }
  }
  return c;
}

/** Fill fields the backend omits when empty so the editor can rely on them. */
function normalize(p) {
  p.pages = p.pages || [];
  p.app_bindings = p.app_bindings || [];
  for (const pg of p.pages) {
    // A page grid equal to the profile grid is just the inherited size.
    if (pg.cols === p.cols && pg.rows === p.rows) {
      delete pg.cols;
      delete pg.rows;
    }
    pg.name = pg.name ?? '';
    pg.controls = pg.controls || [];
    pg.controls.forEach(normalizeControl);
  }
  return p;
}

export async function openProfile(id, notify = true) {
  await flushNow();
  const p = normalize(await api.profile(id));
  S.profile = p;
  S.pageIdx = 0;
  S.sel = null;
  S.enter = true;
  S.save = 'idle';
  if (notify) emit('all');
}

export async function createProfile(name) {
  const id = slug(name);
  const pg = { id: uid('p'), name: 'Main', controls: [] };
  const p = { schema_version: 1, id, name, cols: 5, rows: 3, pages: [pg], initial_page_id: pg.id };
  await flushNow();
  await api.save(p);
  await refreshList();
  await openProfile(id);
}

// ---- OBS templates (round 14, M8) ----

async function obsIcons() {
  const icons = await uploadGlyphs(OBS_GLYPHS);
  if (!Object.keys(icons).length) toast('Could not create the key icons, so the keys will show text only.', 'error');
  return icons;
}

/** Build the OBS template pages for a cols x rows grid. which: 'page' | 'audio' | 'both' (both are linked). */
function obsPages(which, cols, rows, icons) {
  const audio = which === 'page' ? null : { id: uid('p'), name: 'OBS audio', controls: obsAudioPageControls(cols, rows, icons) };
  const main = which === 'audio' ? null : { id: uid('p'), name: 'OBS', controls: obsPageControls(cols, rows, icons, which === 'both' ? audio.id : null) };
  return [main, audio].filter(Boolean);
}

/** Append OBS template pages to the open profile and select the first new one. */
export async function addObsPages(which) {
  if (!S.profile) throw new Error('Open a profile first.');
  const target = S.profile;
  const { cols, rows } = gridOf(target, null);
  const icons = await obsIcons();
  if (S.profile !== target) return; // the user switched profiles while icons uploaded
  const pages = obsPages(which, cols, rows, icons);
  const first = target.pages.length;
  target.pages.push(...pages);
  if (!target.initial_page_id) target.initial_page_id = pages[0].id;
  S.pageIdx = first;
  S.sel = null;
  S.enter = true;
  touch('all');
}

// ---- Media template (round 16) ----

async function mediaIcons() {
  const icons = await uploadGlyphs(MEDIA_GLYPHS);
  if (!Object.keys(icons).length) toast('Could not create the key icons, so the keys will show text only.', 'error');
  return icons;
}

/** Append a media control page to the open profile and select it. */
export async function addMediaPage() {
  if (!S.profile) throw new Error('Open a profile first.');
  const target = S.profile;
  const { cols, rows } = gridOf(target, null);
  const icons = await mediaIcons();
  if (S.profile !== target) return;
  const pg = { id: uid('p'), name: 'Media', controls: mediaPageControls(cols, rows, icons) };
  appendMediaPages(target, [pg]);
}

/** Append the second media page (shuffle, repeat, seek, player volume) to the open profile and select it. */
export async function addMediaPage2() {
  if (!S.profile) throw new Error('Open a profile first.');
  const target = S.profile;
  const { cols, rows } = gridOf(target, null);
  const icons = await mediaIcons();
  if (S.profile !== target) return;
  appendMediaPages(target, [{ id: uid('p'), name: 'Media 2', controls: mediaExtraPageControls(cols, rows, icons) }]);
}

function appendMediaPages(target, pages) {
  const pg = pages[0];
  const first = target.pages.length;
  target.pages.push(...pages);
  if (!target.initial_page_id) target.initial_page_id = pg.id;
  S.pageIdx = first;
  S.sel = null;
  S.enter = true;
  touch('all');
}

// ---- Profile switching templates (round 22) ----

async function profileIcons() {
  const icons = await uploadGlyphs(PROFILE_GLYPHS);
  if (!Object.keys(icons).length) toast('Could not create the key icons, so the keys will show text only.', 'error');
  return icons;
}

/** Append a "Profiles" page (one key per profile, then Back / Next) to the open profile and select it.
 *  Returns `{ placed, total }`. */
export async function addProfilesPage() {
  if (!S.profile) throw new Error('Open a profile first.');
  const target = S.profile;
  const { cols, rows } = gridOf(target, null);
  const icons = await profileIcons();
  if (S.profile !== target) return null;
  const list = S.profiles.map((p) => ({ id: p.id, name: p.name }));
  const r = profilesPageControls(cols, rows, list, icons);
  appendMediaPages(target, [{ id: uid('p'), name: 'Profiles', controls: r.controls }]);
  return { placed: r.placed, total: r.total };
}

/** Add Back / current profile / Next keys to the free cells of the current page. Returns false when fewer than
 *  three cells are free (nothing changed). */
export async function addProfileBar() {
  if (!S.profile) throw new Error('Open a profile first.');
  const target = S.profile;
  const pg = target.pages[S.pageIdx];
  if (!pg) throw new Error('Open a page first.');
  const { cols, rows } = gridOf(target, pg);
  if (!profileBarControls(pg.controls, cols, rows)) return false;
  const icons = await profileIcons();
  if (S.profile !== target || target.pages[S.pageIdx] !== pg) return null;
  const add = profileBarControls(pg.controls, cols, rows, icons);
  if (!add) return false;
  pg.controls.push(...add);
  S.sel = null;
  touch('all');
  return true;
}

/** Add auto-switch rules for a profile, skipping process patterns it already has. */
/** Add process rules (skipping ones already there) and turn auto-switch on: the user asked for switching.
 *  Returns true when auto-switch was off and is now on. */
async function addRules(profileId, rules) {
  const st = await api.autoswitch();
  const cfg = st.config;
  const norm = (x) => x.replace(/\.exe$/i, '').toLowerCase();
  const fresh = rules.filter((rule) => !(cfg.rules || []).some((r) => r.kind === 'process' && r.profile_id === profileId && norm(r.pattern) === norm(rule.pattern)));
  const turnOn = !cfg.enabled;
  if (!fresh.length && !turnOn) return false;
  cfg.rules = [...(cfg.rules || []), ...fresh];
  cfg.enabled = true;
  await api.saveAutoswitch(cfg);
  return turnOn;
}

const turnedOnNote = (on) => (on ? ' Auto-switch was off, so it is now on.' : '');

/** Add the "Switch to this profile when OBS is focused" auto-switch rule (skips if one exists). */
const addObsRule = (profileId) => addRules(profileId, [obsAutoRule(profileId, S.host?.os)]);

/** Create a profile from a template ('obs'). Returns once it is open. */
export async function createProfileFromTemplate(name, template, { autoswitch = true } = {}) {
  if (template === 'media') {
    const id = slug(name);
    const icons = await mediaIcons();
    const pg = { id: uid('p'), name: 'Media', controls: mediaPageControls(5, 3, icons) };
    const pg2 = { id: uid('p'), name: 'Media 2', controls: mediaExtraPageControls(5, 3, icons) };
    await flushNow();
    await api.save({ schema_version: 1, id, name, cols: 5, rows: 3, pages: [pg, pg2], initial_page_id: pg.id });
    await refreshList();
    await openProfile(id);
    return;
  }
  if (APP_TEMPLATES[template]) {
    const id = slug(name);
    const icons = await uploadGlyphs(appTemplateGlyphs(template));
    if (!Object.keys(icons).length) toast('Could not create the key icons, so the keys will show text only.', 'error');
    const pages = appTemplatePages(template, 5, 3, icons);
    await flushNow();
    await api.save({ schema_version: 1, id, name, cols: 5, rows: 3, pages, initial_page_id: pages[0].id });
    await refreshList();
    await openProfile(id);
    if (autoswitch) {
      try {
        const on = await addRules(id, appAutoRules(template, id));
        toast(`Added auto-switch rules: this profile shows when ${APP_TEMPLATES[template].app} is focused.${turnedOnNote(on)}`);
      } catch (e) {
        toast(`The profile was created, but the auto-switch rules were not: ${e.body?.errors?.[0]?.message || e.message}`, 'error');
      }
    }
    return;
  }
  if (template !== 'obs') return createProfile(name);
  const id = slug(name);
  const icons = await obsIcons();
  const pages = obsPages('both', 5, 3, icons);
  const p = { schema_version: 1, id, name, cols: 5, rows: 3, pages, initial_page_id: pages[0].id };
  await flushNow();
  await api.save(p);
  await refreshList();
  await openProfile(id);
  if (autoswitch) {
    if (!S.host) await refreshHost();
    try {
      const on = await addObsRule(id);
      toast(`Added an auto-switch rule: this profile shows when OBS is focused.${turnedOnNote(on)}`);
    } catch (e) {
      toast(`The profile was created, but the auto-switch rule was not: ${e.body?.errors?.[0]?.message || e.message}`, 'error');
    }
  }
}

export async function deleteProfile(id) {
  const gone = S.profile?.id === id ? S.profile : null;
  await api.remove(id);
  if (gone) discard(gone);
  await refreshList();
  if (S.profile?.id === id) {
    S.profile = null;
    if (S.profiles[0]) await openProfile(S.profiles[0].id, false);
  }
  emit('all');
}

export async function activateProfile(id) {
  await flushNow();
  await api.activate(id);
  await refreshList();
  await refreshStatus();
  emit('all');
}

// ---- selection ----

export function select(sel, focus = false) {
  S.sel = sel;
  S.focusSel = focus;
  emit('sel');
}

// ---- pages ----

export function selectPage(i) {
  S.pageIdx = i;
  S.sel = null;
  S.enter = true;
  emit('all');
}

export function addPage() {
  const n = S.profile.pages.length + 1;
  const pg = { id: uid('p'), name: `Page ${n}`, controls: [] };
  S.profile.pages.push(pg);
  if (!S.profile.initial_page_id) S.profile.initial_page_id = pg.id;
  S.pageIdx = S.profile.pages.length - 1;
  S.sel = null;
  S.enter = true;
  touch('all');
}

/** Append a generated mixer page (sliders or dials, one column per slot) and select it. */
export function addMixerPage(mode, slots) {
  const { cols, rows } = gridOf(S.profile, null);
  const n = clamp(slots, 1, cols);
  const pg = { id: uid('p'), name: 'Mixer', controls: mixerPageControls(mode, n, rows) };
  S.profile.pages.push(pg);
  if (!S.profile.initial_page_id) S.profile.initial_page_id = pg.id;
  S.pageIdx = S.profile.pages.length - 1;
  S.sel = null;
  S.enter = true;
  touch('all');
}

export function removePage(i) {
  const pages = S.profile.pages;
  if (pages.length <= 1) return toast('A profile needs at least one page.', 'error');
  const [gone] = pages.splice(i, 1);
  if (S.profile.initial_page_id === gone.id) S.profile.initial_page_id = pages[0].id;
  S.pageIdx = Math.min(S.pageIdx, pages.length - 1);
  S.sel = null;
  S.enter = true;
  touch('all');
}

export function movePage(i, dir) {
  const pages = S.profile.pages;
  const j = i + dir;
  if (j < 0 || j >= pages.length) return;
  [pages[i], pages[j]] = [pages[j], pages[i]];
  S.pageIdx = j;
  touch('all');
}

/** Change grid size for the profile, refusing if controls would fall outside. */
export function setGrid(cols, rows, pg = null) {
  const targets = pg ? [pg] : S.profile.pages.filter((p) => p.cols == null && p.rows == null);
  for (const p of targets) {
    const g = gridOf({ cols, rows }, pg ? { cols, rows } : p);
    if (p.controls.some((c) => c.col + c.span_w > g.cols || c.row + c.span_h > g.rows)) {
      toast('Controls would fall outside the smaller grid. Move them first.', 'error');
      return false;
    }
  }
  if (pg) {
    pg.cols = cols;
    pg.rows = rows;
  } else {
    S.profile.cols = cols;
    S.profile.rows = rows;
  }
  touch('preview');
  return true;
}

// ---- controls ----

export function addControl(kind, col, row) {
  const c = defaultControl(kind, col, row, S.profile);
  const { cols, rows } = grid();
  if (!canResize(c, c.span_w, c.span_h, controls(), cols, rows)) {
    c.span_w = 1;
    if (!canResize(c, 1, 1, controls(), cols, rows)) return toast('That cell is occupied.', 'error');
  }
  page().controls.push(c);
  S.settleIds = [c.id];
  S.sel = { id: c.id };
  touch('all');
}

/** Camera preview tile (round 28): 2x2 when it fits, else 1x1, at the given cell or the first free spot. */
export function addCameraPreview(col, row) {
  const { cols, rows } = grid();
  const c = defaultControl('tile', 0, 0, S.profile);
  Object.assign(c, { title: 'Camera', face: 'camera', span_w: 2, span_h: 2 });
  delete c.subtitle;
  delete c.value;
  const spots = [];
  if (col != null) spots.push({ col, row });
  for (let r = 0; r < rows; r++) for (let q = 0; q < cols; q++) spots.push({ col: q, row: r });
  for (const [w, hh] of [[2, 2], [1, 1]]) {
    for (const p of spots) {
      c.col = p.col;
      c.row = p.row;
      if (canResize(c, w, hh, controls(), cols, rows)) {
        c.span_w = w;
        c.span_h = hh;
        page().controls.push(c);
        S.settleIds = [c.id];
        S.sel = { id: c.id };
        touch('all');
        return;
      }
    }
  }
  toast('No free cell for the camera preview.', 'error');
}

export function removeControl(id) {
  const pg = page();
  const i = pg.controls.findIndex((c) => c.id === id);
  if (i < 0) return;
  const [c] = pg.controls.splice(i, 1);
  S.sel = { cell: { col: c.col, row: c.row } };
  touch('all');
}

export function duplicateControl(id) {
  const src = controls().find((c) => c.id === id);
  const spot = firstFree(controls(), grid().cols, grid().rows);
  if (!src || !spot) return toast('No free cell for a copy.', 'error');
  const copy = JSON.parse(JSON.stringify(src));
  copy.id = uid('c');
  copy.col = spot.col;
  copy.row = spot.row;
  if (!canResize(copy, copy.span_w, copy.span_h, controls(), grid().cols, grid().rows)) {
    copy.span_w = 1;
    copy.span_h = 1;
  }
  page().controls.push(copy);
  S.settleIds = [copy.id];
  S.sel = { id: copy.id };
  touch('all');
}

/** Try to move control to target cell. Returns true if applied. */
export function moveControl(id, target) {
  const c = controls().find((x) => x.id === id);
  if (!c) return false;
  const { cols, rows } = grid();
  const plan = planMove(c, target, controls(), cols, rows);
  if (!plan.ok) return false;
  for (const m of plan.moves) {
    const t = controls().find((x) => x.id === m.id);
    t.col = m.col;
    t.row = m.row;
  }
  S.settleIds = plan.moves.map((m) => m.id);
  S.sel = { id };
  touch('all');
  return true;
}

export function resizeControl(c, w, h) {
  const { cols, rows } = grid();
  if (!canResize(c, w, h, controls(), cols, rows)) return false;
  c.span_w = w;
  c.span_h = h;
  touch('preview');
  return true;
}

export function changeKind(c, kind) {
  c.kind = kind;
  if (kind === 'slider' && c.horizontal === undefined) c.horizontal = true;
  touch('all'); // actions with triggers invalid for the new kind are kept and flagged in the inspector
}
