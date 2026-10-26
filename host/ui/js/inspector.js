// SPDX-License-Identifier: GPL-3.0-or-later
// Right-hand inspector: control editor, empty-cell palette, page and profile settings.

import { h, $, toast } from './dom.js';
import { isLive } from './sidebar.js';
import { api } from './api.js';
import { iconView } from './iconview.js';
import { uploadIconFile, prepareIcon, dropTarget, pastedImage } from './imgload.js';
import { openAppPicker } from './apppicker.js';
import { obsInfo, refreshObs, watchObs } from './obs.js';
import { pluginsInfo, refreshPlugins, watchPlugins, findPlugin, pluginUsable, pluginIcon, statusText } from './plugins.js';
import {
  S, page, grid, deleteProfile, selectedControl, touch, addControl, addCameraPreview, removeControl, duplicateControl,
  resizeControl, changeKind, removePage, movePage, setGrid, addMixerPage, addObsPages, addMediaPage, addMediaPage2, addProfilesPage, addProfileBar,
  addRailKey, removeRailKey, moveRailKey, select, MAX_RAIL_KEYS,
} from './store.js';
import {
  KINDS, MODS, KEY_GROUPS, ACTION_TYPES, STEP_ACTION_TYPES, MAX_MULTI_DELAY_MS, multiTotal, TRIGGERS_BY_KIND, triggerLabel, triggerValid, AUDIO_TARGETS, APP_TARGET_KINDS, defaultAppTarget, triggerForAction, SOURCES, SOURCE_HINTS, FACES, defaultAction, defaultObsSource, defaultObsScene, defaultObsInput, PROFILE_SPECIAL_TARGETS, defaultProfileTarget, OBS_MODES, OBS_MODE_ACTIONS, MEDIA_COMMANDS, MEDIA_REPEAT_MODES, OBS_SPECIALS, argbToHex, hexToArgb, clamp, pluginDefaults,
} from './model.js';

const cap = (s) => s[0].toUpperCase() + s.slice(1);
const up = (s) => s.replace(/_/g, ' ').toUpperCase();

function panel(title, ...kids) {
  return h('section', { class: 'panel' }, h('h2', { class: 'ph' }, title), ...kids);
}

let fid = 0;
function field(label, input, hint) {
  const id = `f${++fid}`;
  input.id = id;
  return h('div', { class: 'field' }, h('label', { for: id }, label), input, hint ? h('small', null, hint) : null);
}

function text(obj, key, opts = {}) {
  return h('input', {
    type: 'text', value: obj[key] ?? '', maxlength: opts.max ?? 64, placeholder: opts.ph,
    oninput: (e) => { obj[key] = e.target.value; touch('preview'); },
  });
}

function select_(options, value, onchange) {
  const sel = h('select', { onchange: (e) => onchange(e.target.value) },
    options.map(([v, l]) => h('option', { value: v, selected: v === value }, l)));
  sel.value = value;
  return sel;
}

/** onchange(v) returns false to refuse; the field then snaps back without re-rendering. */
function stepper(label, value, min, max, onchange) {
  let cur = value;
  const input = h('input', {
    type: 'number', min, max, value, inputmode: 'numeric',
    onchange: (e) => {
      const v = clamp(parseInt(e.target.value, 10) || min, min, max);
      if (onchange(v) === false) {
        e.target.value = cur;
        return;
      }
      cur = v;
      e.target.value = v;
    },
  });
  return field(label, input);
}

export function render() {
  // Build off-DOM, then swap in one step: a blur-triggered change handler may re-enter render().
  const box = h('div');
  if (!S.profile) {
    box.append(panel('Inspector', h('p', { class: 'muted' }, 'No profile loaded.')));
  } else {
    const c = selectedControl();
    if (c) renderControl(box, c);
    else if (S.sel?.cell) renderPalette(box, S.sel.cell);
    else renderPageAndProfile(box);
  }
  const root = $('#inspector');
  const top = root.scrollTop;
  root.replaceChildren(...box.childNodes);
  root.scrollTop = top;
}

// ---- empty cell palette ----

const KIND_BLURB = {
  key: 'Button with icon, title and actions',
  dial: 'Rotary value with push',
  slider: 'Drag fader, horizontal or vertical',
  tile: 'Readout with gauge',
};

function renderPalette(root, cell) {
  root.append(
    panel(`Cell ${String.fromCharCode(65 + cell.row)}${cell.col + 1}`,
      h('p', { class: 'muted' }, 'Empty. Place a control here.'),
      h('ul', { class: 'palette' },
        ...KINDS.map((k) => h('li', null,
          h('button', { type: 'button', class: 'pal', onclick: () => addControl(k, cell.col, cell.row) },
            h('b', null, cap(k)), h('span', null, KIND_BLURB[k]), h('i', { 'aria-hidden': 'true' }, '\u203a')))),
        h('li', null,
          h('button', { type: 'button', class: 'pal', id: 'pal-camera', onclick: () => addCameraPreview(cell.col, cell.row) },
            h('b', null, 'Camera preview'), h('span', null, 'Live phone camera tile'), h('i', { 'aria-hidden': 'true' }, '\u203a')))),
    ),
  );
  renderPageAndProfile(root);
}

// ---- control editor ----

function renderControl(root, c) {
  if (S.sel?.railKey) return renderRailKey(root, c);
  const { cols, rows } = grid();
  root.append(
    panel(`${cap(c.kind)} \u00b7 ${c.id}`,
      h('div', { class: 'row2' },
        h('button', { type: 'button', class: 'btn', onclick: () => duplicateControl(c.id) }, 'Duplicate'),
        h('button', { type: 'button', class: 'btn danger', onclick: () => removeControl(c.id) }, 'Remove'),
      ),
      field('Kind', select_(KINDS.map((k) => [k, cap(k)]), c.kind, (k) => { if (k !== c.kind) changeKind(c, k); })),
    ),
    panel('Appearance',
      field('Title', text(c, 'title', { max: 24 })),
      c.kind === 'tile' && c.face !== 'camera' ? field('Subtitle', text(c, 'subtitle', { max: 32 })) : null,
      c.kind === 'tile' ? field('Face', select_(FACES, c.face || '', (v) => { if (v) c.face = v; else delete c.face; touch('all'); }), 'Media draws album art on the left, the track, artist and times, and a progress bar. Pair it with the Now playing source. Stream draws a live channel: a still, the channel, title and viewers (set by a plugin). Camera is the phone camera preview.') : null,
      h('div', { class: 'row2' }, colorField('Foreground', c, 'fg_argb'), colorField('Background', c, 'bg_argb')),
      c.kind !== 'slider' ? iconField(c) : null,
    ),
    panel('Geometry',
      h('div', { class: 'row2' },
        stepper('Span w', c.span_w, 1, cols, (v) => resizeOrWarn(c, v, c.span_h)),
        stepper('Span h', c.span_h, 1, rows, (v) => resizeOrWarn(c, c.span_w, v))),
      h('p', { class: 'readout' }, `Origin column ${c.col + 1}, row ${c.row + 1}`),
      c.kind === 'slider' ? h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: !!c.horizontal, onchange: (e) => { c.horizontal = e.target.checked; touch('preview'); } }), ' Horizontal') : null,
      c.kind !== 'key' ? valueField(c) : null,
    ),
    c.face === 'camera' ? cameraNote() : sourcePanel(c),
    c.face === 'camera' ? null : actionsPanel(c),
  );
}

/** A pinned side-rail key: same appearance, data source and action editors as a grid key. */
function renderRailKey(root, c) {
  const i = S.rail.findIndex((x) => x.id === c.id);
  root.append(
    panel(`Rail key ${i + 1} \u00b7 ${c.id}`,
      h('div', { class: 'row2' },
        h('button', { type: 'button', class: 'btn', disabled: i === 0, onclick: () => moveRailKey(c.id, -1) }, 'Move up'),
        h('button', { type: 'button', class: 'btn', disabled: i === S.rail.length - 1, onclick: () => moveRailKey(c.id, 1) }, 'Move down')),
      h('button', { type: 'button', class: 'btn danger wide', onclick: () => removeRailKey(c.id) }, 'Remove from rail'),
      h('small', { class: 'muted' }, 'Pinned keys appear on the phone\u2019s side rail on every profile and page.')),
    panel('Appearance',
      field('Title', text(c, 'title', { max: 24 })),
      h('div', { class: 'row2' }, colorField('Foreground', c, 'fg_argb'), colorField('Background', c, 'bg_argb')),
      iconField(c)),
    sourcePanel(c),
    actionsPanel(c),
  );
  root.append(railPanel());
}

/** Slots of the side rail, shown beside the page and profile settings. */
function railPanel() {
  return panel('Side rail',
    h('p', { class: 'muted' }, `Up to ${MAX_RAIL_KEYS} keys pinned under Home and Back on the phone. Click a slot to edit it.`),
    h('ol', { class: 'rail-slots' }, S.rail.map((c, i) => h('li', null,
      h('button', {
        type: 'button', class: `btn wide${S.sel?.railKey === c.id ? ' primary' : ''}`, onclick: () => select({ railKey: c.id }),
      }, `${i + 1}. ${c.title || 'Untitled key'}`)))),
    S.rail.length < MAX_RAIL_KEYS ? h('button', { type: 'button', class: 'btn wide', onclick: addRailKey }, 'Add pinned key') : null);
}

function resizeOrWarn(c, w, h_) {
  if (resizeControl(c, w, h_)) return true;
  toast('No room to resize.', 'error');
  return false;
}

function gridOrRefuse(cols, rows) {
  if (!setGrid(cols, rows)) return false;
  readout();
  return true;
}

function valueField(c) {
  const out = h('output', null, `${Math.round((c.value || 0) * 100)}`);
  const input = h('input', {
    type: 'range', min: 0, max: 100, value: Math.round((c.value || 0) * 100),
    oninput: (e) => { c.value = e.target.value / 100; out.textContent = e.target.value; touch('preview'); },
  });
  const f = field(c.kind === 'tile' ? 'Gauge preview' : 'Value preview', input);
  f.append(out);
  f.classList.add('with-out');
  return f;
}

function colorField(label, c, key) {
  const auto = !c[key];
  const input = h('input', {
    type: 'color', value: auto ? '#161616' : argbToHex(c[key]),
    oninput: (e) => { c[key] = hexToArgb(e.target.value); touch('preview'); },
    onchange: () => touch('all'),
  });
  const f = field(label, input);
  f.append(h('button', { type: 'button', class: 'btn sm', disabled: auto, onclick: () => { c[key] = 0; touch('all'); } }, auto ? 'Auto' : 'Reset'));
  return f;
}

function iconField(c) {
  const lib = [...new Set([...S.profile.pages.flatMap((p) => p.controls), ...S.rail].flatMap((x) => [x.icon_hash, ...(x.states || []).map((s) => s.icon_hash)]).filter(Boolean))];
  const apply = async (f) => {
    if (!f) return;
    const hash = await uploadIconFile(f);
    if (!hash) return;
    c.icon_hash = hash;
    touch('all');
  };
  const file = h('input', { type: 'file', accept: 'image/*,.svg', hidden: true, onchange: (e) => {
    const f = e.target.files[0];
    e.target.value = '';
    apply(f);
  } });
  const zone = h('div', {
    class: 'dropzone', tabindex: 0, role: 'group', 'aria-label': 'Icon drop zone. Drop an image, paste one, or choose a file.',
    onpaste: (e) => { const f = pastedImage(e); if (f) { e.preventDefault(); apply(f); } },
  },
  h('span', { class: 'dz-text' }, 'Drop an image here'),
  h('button', { type: 'button', class: 'btn sm', onclick: () => file.click() }, 'Choose file…'),
  file);
  dropTarget(zone, apply);
  return h('div', { class: 'field' },
    h('span', { class: 'lab' }, 'Icon'),
    h('div', { class: 'icons' },
      c.icon_hash ? h('span', { class: 'cur' }, iconView(api.iconUrl(c.icon_hash), 'cur-i', 'Current icon')) : h('span', { class: 'cur none' }, 'None'),
      c.icon_hash ? h('button', { type: 'button', class: 'btn sm', title: 'Shrink the image with a transparent margin so it is drawn above the title instead of under it', onclick: () => fitIcon(c) }, 'Fit above title') : null,
      c.icon_hash ? h('button', { type: 'button', class: 'btn sm', onclick: () => { c.icon_hash = ''; touch('all'); } }, 'Clear') : null),
    zone,
    lib.length ? h('div', { class: 'lib', role: 'group', 'aria-label': 'Icon library' }, lib.map((hash) =>
      h('button', { type: 'button', class: `lib-i${hash === c.icon_hash ? ' on' : ''}`, 'aria-label': 'Use this icon', onclick: () => { c.icon_hash = hash; touch('all'); } },
        iconView(api.iconUrl(hash), 'lib-img')))) : null,
    h('small', null, 'PNG, WebP, JPEG or SVG. Drop a file here or choose one.'),
  );
}

/** Re-draw the current icon as a fitted logo (transparent margin), so it is a CUTOUT, not full-bleed ARTWORK.
 *  The original stays in the icon library. */
async function fitIcon(c) {
  try {
    const res = await fetch(api.iconUrl(c.icon_hash));
    if (!res.ok) throw new Error(String(res.status));
    const blob = await res.blob();
    const file = new File([blob], 'icon', { type: blob.type || 'image/png' });
    const { hash } = await api.uploadIcon(await prepareIcon(file, { fit: true }));
    c.icon_hash = hash;
    touch('all');
  } catch (err) {
    toast(`Could not fit this icon: ${err.message}`, 'error');
  }
}

// ---- actions ----

function cameraNote() {
  return panel('Camera preview',
    h('p', { class: 'muted', id: 'camera-face-note' }, 'Works on the phone: tap to preview, long press for full screen'));
}

function actionsPanel(c) {
  const triggers = TRIGGERS_BY_KIND[c.kind];
  const list = c.actions.length
    ? h('ol', { class: 'actions' }, c.actions.map((def, i) => actionRow(c, def, i, triggers)))
    : h('p', { class: 'muted' }, 'No actions bound. This control only displays.');
  return panel('Actions', list,
    h('button', { type: 'button', class: 'btn wide', onclick: () => {
      c.actions.push({ action: defaultAction('hotkey', S.profile), trigger: triggers[0] });
      touch('all');
    } }, 'Bind action'));
}

function actionRow(c, def, i, triggers) {
  const a = def.action;
  const bad = def.trigger && !triggerValid(c.kind, def.trigger);
  const opts = triggers.map((t) => [t, triggerLabel(t)]);
  if (bad) opts.unshift([def.trigger, `${triggerLabel(def.trigger)} (invalid)`]);
  return h('li', { class: 'act' },
    h('div', { class: 'act-head' },
      h('span', { class: 'idx' }, String(i + 1)),
      select_(opts, def.trigger, (v) => { def.trigger = v; touch('all'); }),
      h('button', { type: 'button', class: 'btn sm danger', 'aria-label': 'Remove action', onclick: () => { c.actions.splice(i, 1); touch('all'); } }, 'Delete')),
    bad ? h('p', { class: 'warn' }, `Trigger "${triggerLabel(def.trigger)}" does not exist on a ${c.kind}; this action will never fire. Pick another trigger or delete it.`) : null,
    typeSelect(c, def, (v) => {
      if (v.startsWith('plugin:')) { pickPlugin(c, def, v); return; }
      def.action = defaultAction(v, S.profile, S.profiles);
      setFaceFor(c, null);
      const trig = triggerForAction(c.kind, v);
      if (trig && !c.actions.some((o) => o !== def && o.trigger === trig && o.action.type === v)) def.trigger = trig;
      if (v === 'switch_profile') applyProfileTarget(c, def.action.to);
      if (v === 'go_home' && c.kind === 'key' && (!c.title || c.title === 'Key')) c.title = 'Main';
      touch('all');
    }),
    a.type === 'plugin' ? pluginEditor(c, def) : actionParams(a, c),
  );
}

function actionParams(a, c) {
  const pages = S.profile.pages.map((p) => [p.id, p.name || p.id]);
  switch (a.type) {
    case 'hotkey': return hotkeyEditor(a.combo);
    case 'type_text': return h('textarea', { rows: 2, maxlength: 512, 'aria-label': 'Text to type', oninput: (e) => { a.text = e.target.value; touch('preview'); } }, a.text);
    case 'open_url': return urlEditor(a, c);
    case 'launch_app': return launchEditor(a);
    case 'go_to_page': return select_(pages, a.page_id, (v) => { a.page_id = v; touch('preview'); });
    case 'open_folder': return select_(pages, a.folder_page_id, (v) => { a.folder_page_id = v; touch('preview'); });
    case 'multi': return multiEditor(a);
    case 'adjust_volume': return h('div', { class: 'stack' }, targetSelect(a),
      field('Step (%)', h('input', {
        type: 'number', step: 1, min: -100, max: 100, value: Math.round((a.step || 0) * 1000) / 10, inputmode: 'decimal',
        onchange: (e) => { const v = parseFloat(e.target.value); a.step = clamp(Number.isFinite(v) ? v : 0, -100, 100) / 100; e.target.value = Math.round(a.step * 1000) / 10; touch('preview'); },
      }), 'Per dial detent / per press. Negative lowers the volume.'));
    case 'set_volume': return h('div', { class: 'stack' }, targetSelect(a),
      field('Level (%)', h('input', {
        type: 'number', step: 1, min: 0, max: 100, value: a.level == null ? '' : Math.round(a.level * 100), placeholder: 'slider / dial value', inputmode: 'numeric',
        onchange: (e) => {
          const v = parseFloat(e.target.value);
          if (e.target.value.trim() === '' || !Number.isFinite(v)) { delete a.level; e.target.value = ''; } else { a.level = clamp(v, 0, 100) / 100; e.target.value = Math.round(a.level * 100); }
          touch('preview');
        },
      }), 'Empty = use the slider or dial value.'));
    case 'toggle_mute': return h('div', { class: 'stack' }, targetSelect(a),
      h('small', { class: 'muted' }, 'State 2 of the key is shown when muted.'));
    case 'adjust_app_volume': return h('div', { class: 'stack' }, appTargetEditor(a),
      field('Step (%)', h('input', {
        type: 'number', step: 1, min: -100, max: 100, value: Math.round((a.step ?? 0.05) * 1000) / 10, inputmode: 'decimal',
        onchange: (e) => { const v = parseFloat(e.target.value); a.step = clamp(Number.isFinite(v) ? v : 5, -100, 100) / 100; e.target.value = Math.round(a.step * 1000) / 10; touch('preview'); },
      }), 'Per dial detent / per press. Negative lowers the volume.'));
    case 'set_app_volume': return h('div', { class: 'stack' }, appTargetEditor(a),
      field('Level (%)', h('input', {
        type: 'number', step: 1, min: 0, max: 100, value: a.level == null ? '' : Math.round(a.level * 100), placeholder: 'slider / dial value', inputmode: 'numeric',
        onchange: (e) => {
          const v = parseFloat(e.target.value);
          if (e.target.value.trim() === '' || !Number.isFinite(v)) { delete a.level; e.target.value = ''; } else { a.level = clamp(v, 0, 100) / 100; e.target.value = Math.round(a.level * 100); }
          touch('preview');
        },
      }), 'Empty = use the slider or dial value.'));
    case 'toggle_app_mute': return h('div', { class: 'stack' }, appTargetEditor(a),
      h('small', { class: 'muted' }, 'State 2 of the key is shown while the app is muted (set the live source to "App audio" for the same app).'));
    case 'media_transport': return h('div', { class: 'stack' }, field('Command', select_(MEDIA_COMMANDS, a.command || 'play_pause', (v) => { a.command = v; touch('preview'); })),
      h('small', { class: 'muted' }, 'Controls the active player directly; falls back to the media key if the player cannot be controlled.'));
    case 'media_seek': return h('div', { class: 'stack' }, field('Position (%)', h('input', {
      type: 'number', step: 1, min: 0, max: 100, value: a.position == null ? '' : Math.round(a.position * 100), placeholder: 'slider value', inputmode: 'numeric',
      onchange: (e) => {
        const v = parseFloat(e.target.value);
        if (e.target.value.trim() === '' || !Number.isFinite(v)) { delete a.position; e.target.value = ''; } else { a.position = clamp(v, 0, 100) / 100; e.target.value = Math.round(a.position * 100); }
        touch('preview');
      },
    }), 'Empty = use the slider value (on a slider use the Touch end trigger so it seeks once, where you let go). Needs a track with a known length.'));
    case 'media_seek_by': return field('Seconds', h('input', {
      type: 'number', step: 1, min: -3600, max: 3600, value: a.seconds ?? 10, inputmode: 'numeric',
      onchange: (e) => { const v = parseFloat(e.target.value); a.seconds = clamp(Number.isFinite(v) ? v : 10, -3600, 3600); e.target.value = a.seconds; touch('preview'); },
    }), 'Negative skips back. On a dial, multiplied by the detents turned.');
    case 'media_shuffle': return h('div', { class: 'stack' }, obsModeSelect(a),
      h('small', { class: 'muted' }, 'Set the live source to "Media: shuffle" so the key lights up while shuffle is on.'));
    case 'media_repeat': return h('div', { class: 'stack' }, field('Mode', select_(MEDIA_REPEAT_MODES, a.mode || 'cycle', (v) => { a.mode = v; touch('preview'); })),
      h('small', { class: 'muted' }, 'Set the live source to "Media: repeat" and give the key three states (off, all, one).'));
    case 'obs_scene': return h('div', { class: 'stack' }, obsSceneEditor(a, 'scene', 'Scene', false),
      h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: !!a.preview, onchange: (e) => { if (e.target.checked) a.preview = true; else delete a.preview; touch('preview'); } }), ' Set the preview scene (studio mode)'),
      h('small', { class: 'muted' }, 'Scene slots follow OBS\'s scene list top to bottom, so the key works whatever your scenes are called. Set the live source to "OBS: scene" for the same scene to light the key up.'));
    case 'obs_source_visibility': return h('div', { class: 'stack' }, obsSceneEditor(a, 'scene', 'Scene', true), obsItemEditor(a), obsModeSelect(a),
      h('small', { class: 'muted' }, 'Set the live source to "OBS: source shown" with the same scene and source to show its state.'));
    case 'obs_stream': return h('div', { class: 'stack' }, obsModeSelect(a), h('small', { class: 'muted' }, 'Tip: use the Long press trigger so a stray tap cannot go live.'));
    case 'obs_record': case 'obs_record_pause': case 'obs_replay_buffer': case 'obs_virtual_cam': case 'obs_studio_mode':
      return h('div', { class: 'stack' }, obsModeSelect(a));
    case 'obs_save_replay': return h('small', { class: 'muted' }, 'Saves the last seconds of the replay buffer. The replay buffer must be running.');
    case 'obs_studio_transition': return h('small', { class: 'muted' }, 'Sends the preview scene live. Studio mode must be on.');
    case 'obs_input_mute': return h('div', { class: 'stack' }, obsInputEditor(a, 'input', 'Input'), obsModeSelect(a),
      h('small', { class: 'muted' }, 'State 2 of the key is shown while muted (set the live source to "OBS: audio input" for the same input).'));
    case 'obs_set_input_volume': return h('div', { class: 'stack' }, obsInputEditor(a, 'input', 'Input'),
      field('Level (%)', h('input', {
        type: 'number', step: 1, min: 0, max: 100, value: a.level == null ? '' : Math.round(a.level * 100), placeholder: 'slider / dial value', inputmode: 'numeric',
        onchange: (e) => {
          const v = parseFloat(e.target.value);
          if (e.target.value.trim() === '' || !Number.isFinite(v)) { delete a.level; e.target.value = ''; } else { a.level = clamp(v, 0, 100) / 100; e.target.value = Math.round(a.level * 100); }
          touch('preview');
        },
      }), 'Empty = use the slider or dial value (0-100% of the OBS fader).'));
    case 'obs_adjust_input_volume': return h('div', { class: 'stack' }, obsInputEditor(a, 'input', 'Input'),
      field('Step (%)', h('input', {
        type: 'number', step: 1, min: -100, max: 100, value: Math.round((a.step ?? 0.05) * 1000) / 10, inputmode: 'decimal',
        onchange: (e) => { const v = parseFloat(e.target.value); a.step = clamp(Number.isFinite(v) ? v : 5, -100, 100) / 100; e.target.value = Math.round(a.step * 1000) / 10; touch('preview'); },
      }), 'Per dial detent / per press. Negative lowers the volume.'));
    case 'camera_toggle': return h('small', { class: 'muted' }, 'Starts or stops the test stream. While an app uses the camera it pauses (the app shows the "phone not connected" picture) or resumes. State 2 is shown while on; set the live source to "Webcam on" to keep it in sync.');
    case 'mic_toggle': return h('small', { class: 'muted' }, 'Turns the phone microphone on or off (the Microphone tab has the setup). State 2 is shown while on; set the live source to "Phone mic on" to keep it in sync.');
    case 'mic_mute': return h('small', { class: 'muted' }, 'Mutes or unmutes the phone microphone without stopping it, so unmuting is instant. State 2 is shown while muted; set the live source to "Phone mic muted".');
    case 'camera_switch_lens': return h('small', { class: 'muted' }, 'Flips between the back and front camera of the phone.');
    case 'camera_torch': return h('small', { class: 'muted' }, 'Toggles the phone torch while the camera streams. Set the live source to "Webcam torch" so the key shows its state.');
    case 'go_home': return h('div', { class: 'stack' },
      h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: !!a.pause_auto_switch, onchange: (e) => { a.pause_auto_switch = e.target.checked; touch('preview'); } }), ' Pause auto-switch'),
      h('small', { class: 'muted' }, 'Shows the main profile on its start page (set on the Auto-switch tab). Without the pause, auto-switch stays on and the main profile stays until you focus an app with a different rule.'));
    case 'switch_profile': return profileTargetEditor(c, a);
    case 'auto_switch_pause': return h('div', { class: 'stack' },
      field('Mode', select_([['toggle', 'Toggle'], ['pause', 'Pause'], ['resume', 'Resume']], a.mode || 'toggle', (v) => { a.mode = v; touch('preview'); })),
      h('small', { class: 'muted' }, 'State 2 of the key is shown while paused; set the live source to "Auto-switch paused" to keep it in sync. Resuming switches to the focused app\'s profile.'));
    case 'camera_zoom': return h('div', { class: 'stack' },
      field('Zoom step (x)', h('input', {
        type: 'number', step: 0.05, min: -4, max: 4, value: a.step ?? 0.25, inputmode: 'decimal',
        onchange: (e) => { const v = parseFloat(e.target.value); a.step = clamp(Number.isFinite(v) ? v : 0.25, -4, 4); e.target.value = a.step; touch('preview'); },
      }), 'Added to the zoom ratio per dial detent or press (1x to 8x). Use the "Turn" trigger on a dial.'));
    default: return null;
  }
}

// ---- profile switching (round 22) ----

const profileTargetValue = (t) => (t?.by === 'profile' ? `p:${t.id}` : t?.by || 'next');

/** Mirror a switch_profile target onto the control: live source + (fixed profile, empty title) the profile's name. */
function applyProfileTarget(c, to) {
  c.source = { type: 'profile_destination', to: { ...to } };
  const blank = !c.title || c.title === 'Key';
  if (to.by === 'profile' && blank && c.kind !== 'slider') {
    const p = S.profiles.find((x) => x.id === to.id);
    if (p) c.title = p.name;
  }
}

function profileOptions(to) {
  const opts = S.profiles.map((p) => [`p:${p.id}`, p.name]);
  if (to?.by === 'profile' && !S.profiles.some((p) => p.id === to.id)) opts.unshift([`p:${to.id}`, `${to.id} (missing)`]);
  return [...opts, ...PROFILE_SPECIAL_TARGETS];
}

function profileTargetEditor(c, a) {
  const opts = profileOptions(a.to);
  return h('div', { class: 'stack' },
    field('Go to', select_(opts, profileTargetValue(a.to), (v) => {
      a.to = v.startsWith('p:') ? { by: 'profile', id: v.slice(2) } : { by: v };
      if (c) applyProfileTarget(c, a.to);
      touch('all');
    })),
    h('small', { class: 'muted' }, 'Switches the phone to that profile\'s start page. Next and Previous follow the profile list order and wrap around. Back returns to the profile shown before, and pressing it again goes further back. The key lights up while that profile is on screen and shows its name.'));
}

// ---- plugin actions (docs/PLUGINS.md) ----

const plugValue = (p, a) => `plugin:${p}/${a}`;

/** Actions of running/starting plugins that fit this control kind: [{ p, a, value }]. */
function pluginChoices(kind) {
  const out = [];
  for (const p of pluginsInfo() || []) {
    if (!pluginUsable(p)) continue;
    for (const a of p.actions) if ((a.controls || ['key']).includes(kind)) out.push({ p, a, value: plugValue(p.id, a.id) });
  }
  return out;
}

/** The action-type select with a Plugins group. Refreshes the plugin cache when it is opened. */
function typeSelect(c, def, onchange) {
  const a = def.action;
  const cur = a.type === 'plugin' ? plugValue(a.plugin, a.action) : a.type;
  const sel = h('select', { 'aria-label': 'Action type', onchange: (e) => onchange(e.target.value) });
  let osig = '';
  const fill = () => {
    const choices = pluginChoices(c.kind);
    const known = choices.some((x) => x.value === cur);
    const next = JSON.stringify([choices.map((x) => [x.value, x.p.name, x.a.name]), known]);
    if (next === osig) return;
    osig = next;
    const opts = ACTION_TYPES.map(([v, l]) => h('option', { value: v }, l));
    if (choices.length) {
      const names = [...new Set(choices.map((x) => x.p.id))];
      for (const id of names) {
        const rows = choices.filter((x) => x.p.id === id);
        opts.push(h('optgroup', { label: `Plugin: ${rows[0].p.name}` }, rows.map((x) => h('option', { value: x.value }, x.a.name))));
      }
    }
    if (a.type === 'plugin' && !known) opts.push(h('option', { value: cur }, `Plugin not available: ${a.plugin}`));
    sel.replaceChildren(...opts);
    sel.value = cur;
  };
  fill();
  const un = watchPlugins(() => { if (!sel.isConnected) un(); else fill(); });
  const refresh = () => { refreshPlugins(); };
  sel.addEventListener('focus', refresh);
  sel.addEventListener('mousedown', refresh);
  refreshPlugins();
  return sel;
}

const isPlug = (d, plugin, action) => d.action.type === 'plugin' && d.action.plugin === plugin && d.action.action === action;

/** Replace `def` by the first trigger of the plugin action and add one binding per other trigger. */
function pickPlugin(c, def, value) {
  const [pid, aid] = value.slice(7).split('/');
  const p = findPlugin(pid);
  const act = p?.actions.find((x) => x.id === aid);
  if (!p || !act) return;
  const valid = (act.triggers || ['key_down']).filter((t) => triggerValid(c.kind, t));
  const triggers = valid.length ? valid : [TRIGGERS_BY_KIND[c.kind][0]];
  const defaults = () => pluginDefaults(act);
  const mk = (trigger) => ({ trigger, action: { type: 'plugin', plugin: pid, action: aid, settings: defaults() } });
  c.actions[c.actions.indexOf(def)] = mk(triggers[0]);
  for (const t of triggers.slice(1)) {
    if (!c.actions.some((d) => isPlug(d, pid, aid) && d.trigger === t)) c.actions.push(mk(t));
  }
  setFaceFor(c, act.face);
  if (c.kind !== 'slider' && !c.icon_hash && (act.icon || p.icon)) importPluginIcon(c, p, act);
  touch('all');
}

/** A tile placed on a plugin action takes the action's face; leaving a stream action resets a stream face. */
function setFaceFor(c, face) {
  if (c.kind !== 'tile') return;
  if (face) c.face = face;
  else if (c.face === 'stream') delete c.face;
}

/** Rasterise an SVG blob to a 144x144 PNG, the way obsicons.js does for glyphs. */
async function svgToPng(blob) {
  const markup = await blob.text();
  const img = await new Promise((resolve, reject) => {
    const im = new Image();
    im.onload = () => resolve(im);
    im.onerror = () => reject(new Error('could not draw the icon'));
    im.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(markup)}`;
  });
  const canvas = document.createElement('canvas');
  canvas.width = 144;
  canvas.height = 144;
  canvas.getContext('2d').drawImage(img, 24, 24, 96, 96);
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('PNG export failed'))), 'image/png'));
}

/** Use the action's icon (else the plugin's) as the key icon. Best effort, like importAppIcon. */
async function importPluginIcon(c, p, act) {
  try {
    const path = act.icon || p.icon;
    const res = await fetch(api.pluginFileUrl(p.id, path));
    if (!res.ok) return;
    let blob = await res.blob();
    if (/\.svg$/i.test(path) || blob.type.includes('svg')) blob = await svgToPng(blob);
    const { hash } = await api.uploadIcon(blob);
    if (!c.icon_hash) { c.icon_hash = hash; touch('all'); }
  } catch { /* no usable icon: leave the key without one */ }
}

/** Set one setting on every binding of that plugin action on this control. */
function setPluginSetting(c, plugin, action, key, value) {
  for (const d of c.actions) {
    if (!isPlug(d, plugin, action)) continue;
    const next = { ...(d.action.settings || {}) };
    if (value === undefined) delete next[key]; else next[key] = value;
    d.action.settings = next;
  }
  touch('preview');
}

let pwShown = new Set();

function settingControl(c, a, f) {
  const val = a.settings?.[f.key] ?? undefined;
  const set = (v) => setPluginSetting(c, a.plugin, a.action, f.key, v);
  const ph = f.placeholder || undefined;
  switch (f.type) {
    case 'textarea':
      return field(f.label, h('textarea', { rows: 3, placeholder: ph, oninput: (e) => set(e.target.value) }, val ?? ''), f.help);
    case 'password': {
      const id = `f${++fid}`;
      const input = h('input', { id, type: pwShown.has(f.key) ? 'text' : 'password', value: val ?? '', placeholder: ph, autocomplete: 'off', spellcheck: 'false', oninput: (e) => set(e.target.value) });
      const shown = () => input.type === 'text';
      return h('div', { class: 'field' }, h('label', { for: id }, f.label),
        h('div', { class: 'plg-pw' }, input,
          h('button', { type: 'button', class: 'btn sm', 'aria-label': `Show or hide ${f.label}`, onclick: (e) => {
            if (shown()) pwShown.delete(f.key); else pwShown.add(f.key);
            input.type = shown() ? 'password' : 'text';
            e.currentTarget.textContent = shown() ? 'Hide' : 'Show';
          } }, pwShown.has(f.key) ? 'Hide' : 'Show')),
        f.help ? h('small', null, f.help) : null);
    }
    case 'number': {
      const input = h('input', {
        type: 'number', value: val ?? '', min: f.min, max: f.max, step: f.step ?? 'any', placeholder: ph, inputmode: 'decimal',
        oninput: (e) => { const v = parseFloat(e.target.value); set(Number.isFinite(v) ? v : undefined); },
        onchange: (e) => {
          const v = parseFloat(e.target.value);
          if (!Number.isFinite(v)) return;
          const lo = f.min ?? -Infinity;
          const hi = f.max ?? Infinity;
          const k = clamp(v, lo, hi);
          if (k !== v) { e.target.value = k; set(k); }
        },
      });
      return field(f.label, input, f.help);
    }
    case 'toggle': {
      const on = val === undefined ? !!f.default : !!val;
      const sw = h('button', { type: 'button', class: 'switch', role: 'switch', 'aria-checked': String(on), 'aria-label': f.label,
        onclick: (e) => { const n = e.currentTarget.getAttribute('aria-checked') !== 'true'; e.currentTarget.setAttribute('aria-checked', String(n)); set(n); } }, h('i'));
      return h('div', { class: 'field plg-toggle' }, h('div', { class: 'plg-trow' }, h('span', { class: 'lab' }, f.label), sw), f.help ? h('small', null, f.help) : null);
    }
    case 'select': {
      const sel = h('select', { onchange: (e) => set(e.target.value) },
        (f.options || []).map((o) => h('option', { value: o.value }, o.label)));
      sel.value = val ?? f.default ?? f.options?.[0]?.value ?? '';
      return field(f.label, sel, f.help);
    }
    default:
      return field(f.label, h('input', { type: 'text', value: val ?? '', placeholder: ph, oninput: (e) => set(e.target.value) }), f.help);
  }
}

/** Container that redraws when the plugin list changes (and the user is not typing in it). */
function pluginLive(a, draw) {
  const box = h('div', { class: 'stack plg-edit' });
  let sig = '';
  const paint = () => {
    const p = findPlugin(a.plugin);
    const next = JSON.stringify([pluginsInfo() == null, p?.status, p?.name, p?.actions]);
    if (next === sig) return;
    if (sig && box.contains(document.activeElement) && ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName)) return;
    sig = next;
    box.replaceChildren(...draw(p).flat(Infinity).filter(Boolean));
  };
  paint();
  const un = watchPlugins(() => { if (!box.isConnected) un(); else paint(); });
  refreshPlugins();
  return box;
}

function pluginEditor(c, def) {
  const a = def.action;
  return pluginLive(a, (p) => {
    const act = p?.actions.find((x) => x.id === a.action);
    if (pluginsInfo() == null && !p) return [h('p', { class: 'muted' }, 'Loading plugins…')];
    if (!p || !act || !pluginUsable(p)) return unavailablePlugin(a, p, act);
    const siblings = c.actions.filter((d) => isPlug(d, a.plugin, a.action));
    const first = siblings[0] === def;
    const fields = act.settings || [];
    return [
      h('div', { class: 'plg-who' }, pluginIcon(p, act.icon, 'plg-ico'),
        h('div', null, h('b', null, `${p.name}: ${act.name}`), act.description ? h('small', { class: 'muted' }, act.description) : null)),
      !first ? h('small', { class: 'muted' }, `Settings are shared with action ${c.actions.indexOf(siblings[0]) + 1} above.`)
        : fields.length ? fields.map((f) => settingControl(c, a, f)) : h('small', { class: 'muted' }, 'This action has no settings.'),
      first && siblings.length > 1 ? h('small', { class: 'muted' }, `Applies to all ${siblings.length} triggers of this action.`) : null,
    ];
  });
}

function unavailablePlugin(a, p, act) {
  const why = !p ? 'It is not installed.' : !act ? `${p.name} no longer has this action.` : `${p.name} is not running (${statusText(p.status).toLowerCase()}). Turn it on in the Plugins tab.`;
  const entries = Object.entries(a.settings || {});
  return [
    h('p', { class: 'warn plg-missing' }, `Plugin not available: ${a.plugin}`),
    h('small', { class: 'muted' }, `${why} Its settings are kept as they are. Use Delete above to remove this action.`),
    entries.length ? h('dl', { class: 'plg-ro' }, entries.map(([k, v]) => [h('dt', null, k), h('dd', null, typeof v === 'string' ? v : JSON.stringify(v))])) : null,
  ];
}

// ---- OBS pickers (fed by GET /api/obs; free text when OBS is offline) ----

/** A container that redraws `draw(info)` whenever the OBS lists change. */
function obsLive(draw) {
  const box = h('div', { class: 'obs-pick' });
  let sig = '';
  const paint = () => {
    const i = obsInfo();
    const next = JSON.stringify([i?.status, i?.scenes, i?.inputs, i?.special, i?.program_scene, i ? Object.keys(i.scene_items || {}) : 0]);
    if (next === sig) return;
    sig = next;
    box.replaceChildren(...draw(i));
  };
  paint();
  const un = watchObs(() => { if (!box.isConnected) un(); else paint(); });
  refreshObs();
  return box;
}

const sceneKey = (s) => (!s ? '' : s.by === 'slot' ? `slot:${s.index}` : `name:${s.name}`);
const parseScene = (v) => (!v ? undefined : v.startsWith('slot:') ? { by: 'slot', index: parseInt(v.slice(5), 10) || 0 } : { by: 'name', name: v.slice(5) });

function obsSceneEditor(obj, key, label, optional) {
  const set = (v) => { if (v) obj[key] = v; else delete obj[key]; touch('all'); };
  return obsLive((info) => {
    const cur = obj[key];
    const scenes = info?.scenes || [];
    if (scenes.length) {
      const slots = Math.max(scenes.length, cur?.by === 'slot' ? cur.index + 1 : 0);
      const sel = h('select', { onchange: (e) => set(parseScene(e.target.value)) },
        optional ? h('option', { value: '' }, 'Current scene') : null,
        h('optgroup', { label: 'Scene slots' }, Array.from({ length: slots }, (_, i) =>
          h('option', { value: `slot:${i}` }, `Scene slot ${i + 1} (${scenes[i] ? `currently: ${scenes[i]}` : 'empty in OBS'})`))),
        h('optgroup', { label: 'Named scenes' }, [
          ...scenes.map((n) => h('option', { value: `name:${n}` }, n)),
          cur?.by === 'name' && !scenes.includes(cur.name) ? h('option', { value: `name:${cur.name}` }, `${cur.name} (not in OBS)`) : null,
        ]));
      sel.value = sceneKey(cur);
      return [field(label, sel, 'A slot follows OBS\'s scene list, so the key works whatever the scenes are called.')];
    }
    const by = cur ? cur.by : '';
    const kind = select_([...(optional ? [['', 'Current scene']] : []), ['slot', 'Scene slot'], ['name', 'Named scene']], by,
      (v) => set(v === '' ? undefined : v === 'slot' ? defaultObsScene() : { by: 'name', name: '' }));
    return [
      field(label, kind, 'OBS is not connected, so the scene list is unknown. Type the target.'),
      cur?.by === 'slot' ? field('Slot (1-64)', h('input', {
        type: 'number', min: 1, max: 64, step: 1, value: (cur.index || 0) + 1, inputmode: 'numeric',
        onchange: (e) => { const v = clamp(parseInt(e.target.value, 10) || 1, 1, 64); cur.index = v - 1; e.target.value = v; touch('preview'); },
      })) : null,
      cur?.by === 'name' ? field('Scene name', h('input', { type: 'text', value: cur.name || '', maxlength: 128, spellcheck: 'false', oninput: (e) => { cur.name = e.target.value; touch('preview'); } })) : null,
    ];
  });
}

const inputKey = (i) => (!i ? '' : i.by === 'special' ? `special:${i.which}` : 'other');

function obsInputEditor(obj, key, label) {
  if (!obj[key]) obj[key] = defaultObsInput();
  return obsLive((info) => {
    const cur = obj[key];
    const audio = (info?.inputs || []).filter((x) => x.audio !== false);
    const named = audio.map((x) => x.name);
    const isKnownName = cur.by === 'name' && named.includes(cur.name);
    const sel = h('select', { onchange: (e) => {
      const v = e.target.value;
      obj[key] = v.startsWith('special:') ? { by: 'special', which: v.slice(8) } : v.startsWith('name:') ? { by: 'name', name: v.slice(5) } : { by: 'name', name: cur.by === 'name' ? cur.name : '' };
      touch('all');
    } },
    h('optgroup', { label: 'OBS defaults' }, OBS_SPECIALS.map(([w, l]) => h('option', { value: `special:${w}` }, info?.special?.[w] ? `${l}: ${info.special[w]}` : l))),
    named.length ? h('optgroup', { label: 'Audio inputs' }, named.map((n) => h('option', { value: `name:${n}` }, n))) : null,
    h('option', { value: 'other' }, 'Other name…'));
    sel.value = cur.by === 'special' ? `special:${cur.which}` : isKnownName ? `name:${cur.name}` : 'other';
    return [
      field(label, sel, 'The OBS defaults work on any setup, whatever the inputs are called.'),
      sel.value === 'other' ? field('Input name', h('input', { type: 'text', value: cur.name || '', maxlength: 128, spellcheck: 'false', placeholder: 'e.g. Mic/Aux', oninput: (e) => { cur.name = e.target.value; touch('preview'); } })) : null,
    ];
  });
}

/** The source (scene item) name for a visibility action or source; suggestions come from the chosen scene. */
function obsItemEditor(obj) {
  return obsLive((info) => {
    const sc = obj.scene;
    const scenes = info?.scenes || [];
    const sceneName = !sc ? info?.program_scene : sc.by === 'slot' ? scenes[sc.index] : sc.name;
    const items = (info?.scene_items?.[sceneName] || []).map((x) => (typeof x === 'string' ? x : x.source));
    const listId = `obsitems${++appListId}`;
    return [
      field('Source', h('input', { type: 'text', value: obj.source || '', list: listId, maxlength: 128, spellcheck: 'false', placeholder: 'Name of the source in the scene', oninput: (e) => { obj.source = e.target.value; touch('preview'); } }),
        items.length ? `Sources in ${sceneName}: pick one or type a name.` : 'Type the source name exactly as OBS shows it.'),
      h('datalist', { id: listId }, items.map((n) => h('option', { value: n }))),
    ];
  });
}

function obsModeSelect(a) {
  return field('Mode', select_(OBS_MODES, a.mode || 'toggle', (v) => { a.mode = v; touch('preview'); }));
}

function targetSelect(a) {
  return field('Target', select_(AUDIO_TARGETS, a.target || 'output', (v) => { a.target = v; touch('preview'); }));
}

function sourcePanel(c) {
  const src = c.source;
  const hint = h('small', null, src ? SOURCE_HINTS[src.type] || '' : 'Static control. Pick a source to keep it updated live.');
  return panel('Live data source',
    field('Source', select_(SOURCES, src?.type || '', (v) => {
      if (!v) delete c.source;
      else if (v === 'profile_destination') c.source = { type: v, to: { ...(c.actions.find((d) => d.action.type === 'switch_profile')?.action.to ?? defaultProfileTarget(S.profiles)) } };
      else if (v === 'app_audio') c.source = { type: v, app: src?.app ?? defaultAppTarget() };
      else if (v.startsWith('obs_')) c.source = defaultObsSource(v);
      else if (v === 'now_playing') c.source = { type: v, art: !!c.face || src?.art || false };
      else c.source = (v === 'volume' || v === 'mute') ? { ...(src && src.target ? { target: src.target } : { target: 'output' }), type: v } : { type: v };
      touch('all');
    })),
    src && (src.type === 'volume' || src.type === 'mute') ? targetField(src) : null,
    src && src.type === 'now_playing' ? h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: !!src.art, onchange: (e) => { if (e.target.checked) src.art = true; else delete src.art; touch('preview'); } }), ' Album art') : null,
    src && src.type === 'profile_destination' ? field('Profile', select_(profileOptions(src.to), profileTargetValue(src.to), (v) => { src.to = v.startsWith('p:') ? { by: 'profile', id: v.slice(2) } : { by: v }; touch('all'); })) : null,
    src && src.type === 'app_audio' ? appTargetEditor(src) : null,
    src && src.type === 'obs_scene' ? obsSceneEditor(src, 'scene', 'Scene', false) : null,
    src && src.type === 'obs_source_visibility' ? h('div', { class: 'stack' }, obsSceneEditor(src, 'scene', 'Scene', true), obsItemEditor(src)) : null,
    src && src.type === 'obs_input' ? obsInputEditor(src, 'input', 'Input') : null,
    hint);
}

// Apps the mixer currently knows, for the "Specific app" datalist. Fetched once per page load, refreshed on demand.
let mixerApps = [];
let mixerFetched = 0;
function loadMixerApps(list) {
  if (Date.now() - mixerFetched < 5000) return;
  mixerFetched = Date.now();
  api.mixer().then((m) => {
    mixerApps = m?.apps ?? [];
    list.replaceChildren(...mixerApps.flatMap((x) => [h('option', { value: x.key }, x.display_name || x.key), h('option', { value: x.display_name || x.key })]));
  }).catch(() => { mixerFetched = 0; });
}

let appListId = 0;
/** Target picker for an `app` field: mixer slot / focused app / specific app. */
function appTargetEditor(obj) {
  if (!obj.app) obj.app = defaultAppTarget();
  const t = obj.app;
  const listId = `apps${++appListId}`;
  const list = h('datalist', { id: listId });
  list.append(...mixerApps.flatMap((x) => [h('option', { value: x.key }, x.display_name || x.key)]));
  const kind = field('App', select_(APP_TARGET_KINDS, t.by, (v) => {
    obj.app = v === 'slot' ? { by: 'slot', index: 0 } : v === 'focused' ? { by: 'focused' } : v === 'media' ? { by: 'media' } : { by: 'name', name: '' };
    touch('all');
  }));
  const extra = t.by === 'slot'
    ? field('Slot (1-16)', h('input', {
      type: 'number', min: 1, max: 16, step: 1, value: (t.index || 0) + 1, inputmode: 'numeric',
      onchange: (e) => { const v = clamp(parseInt(e.target.value, 10) || 1, 1, 16); t.index = v - 1; e.target.value = v; touch('preview'); },
    }), 'Slot 1 is the first app on the Mixer tab.')
    : t.by === 'name'
      ? field('App name', h('input', {
        type: 'text', value: t.name || '', list: listId, maxlength: 64, placeholder: 'e.g. spotify', spellcheck: 'false',
        onfocus: () => loadMixerApps(list),
        oninput: (e) => { t.name = e.target.value; touch('preview'); },
      }), 'Matches the process name or the display name.')
      : t.by === 'media'
        ? h('small', { class: 'muted' }, 'Follows whichever app is the current media player (the one shown in Now playing).')
        : h('small', { class: 'muted' }, 'Follows the app you are using right now.');
  return h('div', { class: 'stack' }, kind, extra, list);
}

function targetField(src) {
  return field('Target', select_(AUDIO_TARGETS, src.target || 'output', (v) => { src.target = v; touch('preview'); }));
}

function multiEditor(a) {
  a.steps = a.steps || [];
  const total = h('output', { class: 'multi-total' });
  const refresh = () => {
    const t = multiTotal(a);
    total.textContent = `Total delay ${t} / ${MAX_MULTI_DELAY_MS} ms`;
    total.classList.toggle('over', t > MAX_MULTI_DELAY_MS);
  };
  refresh();
  const move = (i, d) => {
    const j = i + d;
    if (j < 0 || j >= a.steps.length) return;
    [a.steps[i], a.steps[j]] = [a.steps[j], a.steps[i]];
    touch('all');
  };
  const rows = a.steps.map((st, i) => {
    const delay = h('input', {
      type: 'number', min: 0, max: MAX_MULTI_DELAY_MS, step: 10, value: st.delay_ms || 0, inputmode: 'numeric', 'aria-label': `Step ${i + 1} delay in ms`,
      onchange: (e) => {
        const v = Math.max(0, parseInt(e.target.value, 10) || 0);
        const others = multiTotal(a) - (st.delay_ms || 0);
        if (others + v > MAX_MULTI_DELAY_MS) {
          toast(`Total delay is capped at ${MAX_MULTI_DELAY_MS} ms.`, 'error');
          e.target.value = st.delay_ms || 0;
          return;
        }
        st.delay_ms = v;
        e.target.value = v;
        refresh();
        touch('preview');
      },
    });
    return h('li', { class: 'step' },
      h('div', { class: 'step-head' },
        h('span', { class: 'idx' }, `S${i + 1}`),
        h('label', { class: 'delay' }, 'Delay (ms)', delay),
        h('button', { type: 'button', class: 'btn sm', disabled: i === 0, 'aria-label': 'Move step up', onclick: () => move(i, -1) }, 'Up'),
        h('button', { type: 'button', class: 'btn sm', disabled: i === a.steps.length - 1, 'aria-label': 'Move step down', onclick: () => move(i, 1) }, 'Down'),
        h('button', { type: 'button', class: 'btn sm danger', 'aria-label': 'Remove step', onclick: () => { a.steps.splice(i, 1); touch('all'); } }, 'Delete')),
      select_(STEP_ACTION_TYPES, st.action.type, (v) => { st.action = defaultAction(v, S.profile, S.profiles); touch('all'); }),
      actionParams(st.action));
  });
  return h('div', { class: 'multi' },
    a.steps.length ? h('ol', { class: 'steps' }, rows) : h('p', { class: 'muted' }, 'No steps. Add at least one.'),
    h('button', { type: 'button', class: 'btn wide', onclick: () => { a.steps.push({ action: defaultAction('hotkey', S.profile), delay_ms: 0 }); touch('all'); } }, 'Add step'),
    total);
}

function launchEditor(a) {
  // Editing the command revokes the review (ADR 0002), and the checkbox must show that.
  const box = h('input', { type: 'checkbox', checked: !!a.confirmed, onchange: (e) => {
    if (e.target.checked && !confirm(`Allow this control to run on your PC?\n\n${[a.path, ...a.args].join(' ')}`)) { e.target.checked = false; return; }
    a.confirmed = e.target.checked;
    touch('preview');
  } });
  const revoke = () => { a.confirmed = false; box.checked = false; };
  const pathInput = h('input', { type: 'text', value: a.path, placeholder: 'Executable or file path', 'aria-label': 'Path', oninput: (e) => { a.path = e.target.value; revoke(); touch('preview'); } });
  const argsInput = h('textarea', { rows: 2, placeholder: 'Arguments, one per line', 'aria-label': 'Arguments', oninput: (e) => { a.args = e.target.value.split('\n').filter(Boolean); revoke(); touch('preview'); } }, a.args.join('\n'));
  const chosen = h('p', { class: 'muted small' }, a.path ? `Current: ${a.path}` : 'No app chosen yet.');
  const choose = () => openAppPicker((app) => {
    a.path = app.target;
    a.args = [...(app.args || [])];
    a.confirmed = true; // picking from the system's app list is the review (ADR 0002)
    pathInput.value = a.path;
    argsInput.value = a.args.join('\n');
    box.checked = true;
    chosen.textContent = `Chosen: ${app.name}`;
    const c = selectedControl();
    if (c) {
      if (!c.title || c.title === 'Key') c.title = app.name.slice(0, 64);
      importAppIcon(c, app); // the app's icon replaces any icon already on the key
    }
    touch('all');
  });
  return h('div', { class: 'stack' },
    h('button', { type: 'button', class: 'btn primary wide', onclick: choose }, 'Choose app…'),
    chosen,
    h('details', { class: 'adv' },
      h('summary', null, 'Advanced: path'),
      h('div', { class: 'stack' }, pathInput, argsInput)),
    h('label', { class: 'check' }, box, ' I reviewed this command and allow it to run'),
    h('p', { class: 'warn' }, 'Runs on your PC when the control fires. Unconfirmed commands are blocked.'));
}

/** Fetch the app's icon from the agent and import it through the normal icon upload. Best effort. */
async function importAppIcon(c, app) {
  try {
    const res = await fetch(api.appIconUrl(app.id));
    if (!res.ok) return;
    const blob = await res.blob();
    const ext = /svg/.test(res.headers.get('content-type') || '') ? 'svg' : 'png';
    const file = new File([blob], `${app.id}.${ext}`, { type: ext === 'svg' ? 'image/svg+xml' : 'image/png' });
    const { hash } = await api.uploadIcon(await prepareIcon(file, { fit: true })); // logo: margin so it sits above the title
    c.icon_hash = hash;
    touch('all');
  } catch { /* the app has no usable icon: leave it empty */ }
}

/** "Open URL" editor: fetches the site's own icon (via the agent, no third-party service) into a key without one. */
function urlEditor(a, c) {
  const validUrl = (v) => { try { return /^https?:$/.test(new URL(v.trim()).protocol); } catch { return false; } };
  let timer = 0;
  const input = h('input', {
    type: 'url', value: a.url, 'aria-label': 'URL',
    oninput: (e) => {
      a.url = e.target.value;
      touch('preview');
      clearTimeout(timer);
      if (!c.icon_hash && validUrl(a.url)) timer = setTimeout(() => importSiteIcon(c, a.url, false), 800);
    },
    onchange: (e) => {
      clearTimeout(timer);
      if (!c.icon_hash && validUrl(e.target.value)) importSiteIcon(c, e.target.value, false);
    },
  });
  const btn = h('button', {
    type: 'button', class: 'btn', onclick: () => {
      clearTimeout(timer);
      if (!validUrl(a.url)) { toast('Enter a full http(s) URL first', 'error'); return; }
      importSiteIcon(c, a.url, true);
    },
  }, 'Use site icon');
  return h('div', { class: 'stack' }, input, btn);
}

/** Fetch the site's icon from the agent and import it through the normal icon upload. `loud` = report failures. */
async function importSiteIcon(c, url, loud) {
  const host = (() => { try { return new URL(url.trim()).hostname; } catch { return url; } })();
  try {
    const res = await fetch(api.siteIconUrl(url.trim()));
    if (!res.ok) throw new Error(String(res.status));
    const blob = await res.blob();
    const type = (res.headers.get('content-type') || 'image/png').split(';')[0];
    const ext = type.includes('svg') ? 'svg' : type.includes('jpeg') ? 'jpg' : type.includes('webp') ? 'webp' : 'png';
    const file = new File([blob], `${host}.${ext}`, { type });
    const { hash } = await api.uploadIcon(await prepareIcon(file, { fit: true })); // logo: margin so it sits above the title
    if (!loud && c.icon_hash) return; // the user set an icon while we were fetching
    c.icon_hash = hash;
    touch('all');
  } catch {
    if (loud) toast(`No icon found for ${host}`, 'error');
  }
}

function hotkeyEditor(combo) {
  const key = h('select', { 'aria-label': 'Key', onchange: (e) => { combo.key = e.target.value; touch('preview'); } },
    KEY_GROUPS.map(([g, keys]) => h('optgroup', { label: g }, keys.map((k) => h('option', { value: k }, up(k))))));
  key.value = combo.key;
  return h('div', { class: 'hotkey' },
    h('div', { class: 'mods', role: 'group', 'aria-label': 'Modifiers' }, MODS.map((m) => {
      const on = combo.modifiers.includes(m);
      return h('button', {
        type: 'button', class: `mod${on ? ' on' : ''}`, 'aria-pressed': on ? 'true' : 'false',
        onclick: (e) => {
          const now = !combo.modifiers.includes(m);
          combo.modifiers = now ? [...combo.modifiers, m] : combo.modifiers.filter((x) => x !== m);
          e.currentTarget.classList.toggle('on', now);
          e.currentTarget.setAttribute('aria-pressed', String(now));
          touch('preview');
        },
      }, m.toUpperCase());
    })),
    key);
}

// ---- page and profile ----

function readout() {
  const { cols, rows } = grid();
  const el = $('#grid-readout');
  if (el) el.textContent = `This page ${cols} \u00d7 ${rows}`;
}

function renderPageAndProfile(root) {
  const pg = page();
  const i = S.pageIdx;
  const { cols, rows } = grid();
  if (pg) {
    root.append(panel(`Page ${i + 1}`,
      field('Name', text(pg, 'name', { max: 24 })),
      h('div', { class: 'row2' },
        h('button', { type: 'button', class: 'btn', disabled: i === 0, onclick: () => movePage(i, -1) }, 'Move left'),
        h('button', { type: 'button', class: 'btn', disabled: i === S.profile.pages.length - 1, onclick: () => movePage(i, 1) }, 'Move right')),
      h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: S.profile.initial_page_id === pg.id, onchange: (e) => { if (e.target.checked) { S.profile.initial_page_id = pg.id; touch('all'); } else render(); } }), ' Start page'),
      h('button', { type: 'button', class: 'btn danger wide', onclick: () => { if (confirm(`Remove page "${pg.name}" and its controls?`)) removePage(i); } }, 'Remove page'),
    ));
  }
  root.append(pageTemplatesPanel(cols));
  root.append(panel('Profile',
    field('Name', h('input', { type: 'text', maxlength: 32, value: S.profile.name, oninput: (e) => {
      S.profile.name = e.target.value;
      const row = S.profiles.find((x) => x.id === S.profile.id);
      if (row) row.name = e.target.value;
      touch('meta');
    } })),
    h('button', { type: 'button', class: 'btn danger wide', disabled: isLive(), title: isLive() ? 'Push another profile to the phone first.' : null,
      onclick: () => { if (confirm(`Delete profile "${S.profile.name}"? This cannot be undone.`)) deleteProfile(S.profile.id).catch((e) => toast(e.message, 'error')); } }, 'Delete profile')));
  root.append(railPanel());
  root.append(panel('Profile grid',
    h('div', { class: 'row2' },
      stepper('Columns', S.profile.cols, 1, 8, (v) => gridOrRefuse(v, S.profile.rows)),
      stepper('Rows', S.profile.rows, 1, 6, (v) => gridOrRefuse(S.profile.cols, v))),
    h('p', { class: 'readout', id: 'grid-readout' }, `This page ${cols} \u00d7 ${rows}`),
    field('App bindings', h('input', {
      type: 'text', value: (S.profile.app_bindings || []).join(', '), placeholder: 'obs64.exe, firefox',
      oninput: (e) => { S.profile.app_bindings = e.target.value.split(',').map((s) => s.trim()).filter(Boolean); touch('preview'); },
    }), 'Process names that switch to this profile.'),
  ));
}

let mixGen = { mode: 'sliders', slots: 0 };
function mixerSub(cols) {
  const slots = clamp(mixGen.slots || cols, 1, cols);
  return h('div', { class: 'sub', id: 'tpl-mixer' },
    h('h3', null, 'Mixer page'),
    h('p', { class: 'muted' }, 'Adds a page with one column per app slot: a full-height volume slider. Tap an app’s icon on the phone to mute it. Slots fill automatically with the apps that play sound.'),
    h('div', { class: 'row2' },
      field('Controls', select_([['sliders', 'Sliders'], ['dials', 'Dials']], mixGen.mode, (v) => { mixGen.mode = v; })),
      stepper('Slots', slots, 1, cols, (v) => { mixGen.slots = v; })),
    h('button', { type: 'button', class: 'btn wide', onclick: () => addMixerPage(mixGen.mode, clamp(mixGen.slots || cols, 1, cols)) }, 'Add mixer page'));
}

function obsTemplateButton(label, which, id) {
  const b = h('button', { id, type: 'button', class: 'btn wide', onclick: async () => {
    b.disabled = true;
    try { await addObsPages(which); toast('Added. Edit any key to adjust it.'); } catch (e) { toast(`Could not add the page: ${e.message}`, 'error'); }
    b.disabled = false;
  } }, label);
  return b;
}

function obsSub() {
  return h('div', { class: 'sub', id: 'tpl-obs' },
    h('h3', null, 'OBS Studio'),
    h('p', { class: 'muted' }, 'Ready-made keys: scene slots, record, stream (hold to go live), mic, desktop audio, replay and more. They use OBS\'s default mic and desktop audio, so nothing needs setting up.'),
    obsTemplateButton('Add OBS page', 'page', 'tpl-obs-page'),
    obsTemplateButton('Add OBS audio page', 'audio', 'tpl-obs-audio'),
    obsTemplateButton('Add both (linked)', 'both', 'tpl-obs-both'));
}

function profilesSub() {
  const b = h('button', { id: 'tpl-profiles-page', type: 'button', class: 'btn wide', onclick: async () => {
    b.disabled = true;
    try {
      const r = await addProfilesPage();
      if (r) toast(r.placed < r.total ? `Added. Only ${r.placed} of ${r.total} profiles fit on this grid. Make the grid bigger, or add a second page by hand, for the rest.` : 'Added. Edit any key to adjust it.', r.placed < r.total ? 'error' : undefined);
    } catch (e) { toast(`Could not add the page: ${e.message}`, 'error'); }
    b.disabled = false;
  } }, 'Add profiles page');
  const b2 = h('button', { id: 'tpl-profile-bar', type: 'button', class: 'btn wide', onclick: async () => {
    b2.disabled = true;
    try {
      const r = await addProfileBar();
      if (r === false) toast('This page needs 3 free cells for the profile bar (Back, current profile, Next). Free some cells or use another page.', 'error');
      else if (r) toast('Added to the free cells. Edit any key to adjust it.');
    } catch (e) { toast(`Could not add the keys: ${e.message}`, 'error'); }
    b2.disabled = false;
  } }, 'Add profile bar to this page');
  return h('div', { class: 'sub', id: 'tpl-profiles' },
    h('h3', null, 'Profile switching'),
    h('p', { class: 'muted' }, 'Profiles page: one key per profile (it lights up while that profile is on screen), then Back and Next. Profile bar: Back, the current profile\'s name (tap for the next profile) and Next, placed into three free cells of the page you are on.'),
    b, b2);
}

function mediaSub() {
  const b = h('button', { id: 'tpl-media-page', type: 'button', class: 'btn wide', onclick: async () => {
    b.disabled = true;
    try { await addMediaPage(); toast('Added. Edit any key to adjust it.'); } catch (e) { toast(`Could not add the page: ${e.message}`, 'error'); }
    b.disabled = false;
  } }, 'Add media page');
  const b2 = h('button', { id: 'tpl-media-page2', type: 'button', class: 'btn wide', onclick: async () => {
    b2.disabled = true;
    try { await addMediaPage2(); toast('Added. Edit any key to adjust it.'); } catch (e) { toast(`Could not add the page: ${e.message}`, 'error'); }
    b2.disabled = false;
  } }, 'Add media page 2');
  return h('div', { class: 'sub', id: 'tpl-media' },
    h('h3', null, 'Media controls'),
    h('p', { class: 'muted' }, 'A now-playing tile with album art, previous / play / next / stop keys, mute and mic mute, and a volume slider. Works with whatever player is active. Page 2 adds shuffle, repeat, a seek bar, skip 10 s keys and a player volume slider.'),
    b, b2);
}

function cameraSub() {
  return h('div', { class: 'sub', id: 'tpl-camera' },
    h('h3', null, 'Camera preview'),
    h('p', { class: 'muted' }, 'A tile that shows the phone camera: tap to preview, long press for full screen. Placed 2x2 in the first free spot (1x1 if there is no room).'),
    h('button', { id: 'tpl-camera-tile', type: 'button', class: 'btn wide', onclick: () => addCameraPreview() }, 'Add camera preview'));
}

function pageTemplatesPanel(cols) {
  return panel('Page templates', mixerSub(cols), mediaSub(), obsSub(), profilesSub(), cameraSub());
}
