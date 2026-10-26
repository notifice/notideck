// SPDX-License-Identifier: GPL-3.0-or-later
// Camera tab "Image" card: focus, exposure, white balance, anti-flicker, stabilization.
// Settings live in S.settings.camera_image (whole object, PATCHed as one).

import { h, debounce } from './dom.js';
import { S } from './store.js';

const DEFAULTS = {
  focus: 'continuous', focus_permille: 0, exposure: 'auto', exposure_comp: 0, ae_lock: false, iso: 0, exposure_us: 0,
  white_balance: 'auto', awb_lock: false, antibanding: 'auto', stabilization: false,
};
const WB_NAMES = {
  auto: 'Auto', incandescent: 'Incandescent', fluorescent: 'Fluorescent', warm_fluorescent: 'Warm fluorescent',
  daylight: 'Daylight', cloudy_daylight: 'Cloudy', twilight: 'Twilight', shade: 'Shade',
};
const AB_NAMES = [['auto', 'Auto'], ['off', 'Off'], ['50hz', '50 Hz'], ['60hz', '60 Hz']];
// Used when the phone sends no capabilities (older phone): the phone clamps whatever we send.
const NO_CAPS = {
  focus_manual: true, focus_lock: true, exposure_manual: true, iso_min: 50, iso_max: 3200, exposure_us_min: 100, exposure_us_max: 1000000,
  ev_min: -12, ev_max: 12, ev_step: 1 / 6, ae_lock: true, awb_lock: true, white_balances: Object.keys(WB_NAMES),
  antibandings: AB_NAMES.map(([v]) => v), stabilization: true,
};

export const imageSettings = () => ({ ...DEFAULTS, ...(S.settings?.camera_image ?? {}) });

/** Capabilities of the lens in use, or null when the phone sent none. */
export function activeCaps(c, settingsLens) {
  const list = c?.phone?.lens_caps ?? [];
  if (!list.length) return null;
  const lens = c.active_mode?.lens ?? settingsLens;
  return list.find((x) => x.lens === lens) ?? null;
}

export const imageSig = (c, lens) => JSON.stringify([imageSettings(), activeCaps(c, lens), c?.active_mode?.fps, !!c?.readout]);

const fmtShutter = (us) => (us >= 500000 ? `${(us / 1e6).toFixed(us >= 1e7 ? 0 : 1)} s` : `1/${Math.max(1, Math.round(1e6 / Math.max(1, us)))} s`);
const fmtEv = (idx, step) => {
  const v = idx * step;
  return `${v > 0.005 ? '+' : ''}${v.toFixed(1)} EV`;
};
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

export function readoutText(r) {
  if (!r) return '';
  const parts = [];
  if (r.iso) parts.push(`ISO ${r.iso}`);
  if (r.exposure_us) parts.push(fmtShutter(r.exposure_us));
  parts.push(`focus ${Math.round((r.focus_permille ?? 0) / 10)}%`);
  parts.push(r.focused ? 'focused' : 'focusing');
  return parts.join(' · ');
}

const save = debounce(async (patch) => { await patch({ camera_image: imageSettings() }); }, 150);

/** @param {object} c GET /api/camera  @param {{lens:string, patch:Function, rerender:Function, row:Function, switchBtn:Function}} k */
export function imageCard(c, k) {
  const caps = activeCaps(c, k.lens) ?? NO_CAPS;
  const cur = imageSettings();
  const fps = c.active_mode?.fps || 30;

  // Change settings: update local state at once, save (debounced) as the whole object.
  const set = (delta, rebuild = true) => {
    S.settings = { ...S.settings, camera_image: { ...imageSettings(), ...delta } };
    if (rebuild) k.rerender();
    save(k.patch);
  };

  const seg = (label, opts, value, onpick) => h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': label },
    opts.map(([v, l, off]) => h('button', {
      type: 'button', role: 'radio', class: 'seg', disabled: !!off, 'aria-checked': v === value ? 'true' : 'false', tabindex: v === value ? 0 : -1,
      onclick: () => { if (v !== value) onpick(v); },
    }, l)));

  /** Slider whose label follows the thumb; `to(v)`/`from(pos)` map between setting value and slider position. */
  const slider = (id, label, min, max, step, pos, fmt, onvalue, disabled = false) => {
    const out = h('output', { id: `${id}-out` }, fmt(pos));
    const input = h('input', {
      id, type: 'range', min, max, step, value: pos, disabled, 'aria-label': label, 'aria-valuetext': fmt(pos),
      oninput: (e) => {
        const p = parseFloat(e.target.value);
        out.textContent = fmt(p);
        e.target.setAttribute('aria-valuetext', fmt(p));
        onvalue(p);
      },
    });
    return h('div', { class: 'cam-rate cam-img-rate' }, input, out);
  };

  // log mapping for ISO / shutter sliders: position 0..1000
  const logPos = (v, lo, hi) => Math.round(1000 * Math.log(clamp(v, lo, hi) / lo) / Math.log(hi / lo || 1));
  const logVal = (p, lo, hi) => Math.round(lo * Math.exp((p / 1000) * Math.log(hi / lo || 1)));

  // ---- focus
  const focusOpts = [['continuous', 'Auto'], ['locked', 'Lock', !caps.focus_lock], ['manual', 'Manual', !caps.focus_manual]];
  const focusRows = [
    k.row('Focus', !caps.focus_lock || !caps.focus_manual ? 'This lens has no ' + [!caps.focus_lock && 'lock', !caps.focus_manual && 'manual focus'].filter(Boolean).join(' or ') + '.' : 'Auto follows the subject. Lock keeps the current distance.',
      seg('Focus mode', focusOpts, cur.focus, (v) => {
        // Manual starts where the lens is now, so the picture doesn't jump.
        const d = { focus: v };
        if (v === 'manual' && c.readout?.focus_permille) d.focus_permille = c.readout.focus_permille;
        set(d);
      })),
  ];
  if (cur.focus === 'manual') {
    focusRows.push(k.row('Focus distance', 'Far ↔ Near', slider('cam-img-focus', 'Focus distance, far to near', 0, 1000, 10, cur.focus_permille,
      (p) => `${Math.round(p / 10)}%`, (p) => set({ focus_permille: Math.round(p) }, false))));
  }

  // ---- exposure
  const manualExp = cur.exposure === 'manual' && caps.exposure_manual;
  const expRows = [
    k.row('Exposure', caps.exposure_manual ? 'Auto picks brightness itself. Manual fixes ISO and shutter.' : 'This lens has no manual exposure.',
      seg('Exposure mode', [['auto', 'Auto'], ['manual', 'Manual', !caps.exposure_manual]], cur.exposure, (v) => {
        const d = { exposure: v };
        if (v === 'manual') {
          const r = c.readout;
          if (!cur.iso) d.iso = clamp(r?.iso || 100, caps.iso_min || 1, caps.iso_max || 409600);
          if (!cur.exposure_us) d.exposure_us = clamp(r?.exposure_us || Math.round(1e6 / 60), caps.exposure_us_min || 1, caps.exposure_us_max || 1e7);
        }
        set(d);
      })),
  ];
  if (manualExp) {
    const isoLo = Math.max(1, caps.iso_min || 50);
    const isoHi = Math.max(isoLo + 1, caps.iso_max || 3200);
    const shLo = Math.max(1, caps.exposure_us_min || 100);
    const shHi = Math.max(shLo + 1, Math.min(caps.exposure_us_max || 1e6, Math.round(1e6 / fps)));
    expRows.push(
      k.row('ISO', 'Higher is brighter and noisier.', slider('cam-img-iso', 'ISO', 0, 1000, 5, logPos(cur.iso || isoLo, isoLo, isoHi),
        (p) => `ISO ${logVal(p, isoLo, isoHi)}`, (p) => set({ iso: logVal(p, isoLo, isoHi) }, false))),
      k.row('Shutter', `Longest is 1/${fps} s at the current frame rate.`, slider('cam-img-shutter', 'Shutter time', 0, 1000, 5, logPos(cur.exposure_us || shHi, shLo, shHi),
        (p) => fmtShutter(logVal(p, shLo, shHi)), (p) => set({ exposure_us: logVal(p, shLo, shHi) }, false))));
  } else {
    const evLo = caps.ev_min ?? 0;
    const evHi = caps.ev_max ?? 0;
    const step = caps.ev_step || 0;
    if (step && evHi > evLo) {
      expRows.push(k.row('Brightness', 'Exposure compensation.', slider('cam-img-ev', 'Exposure compensation in EV', evLo, evHi, 1, clamp(cur.exposure_comp, evLo, evHi),
        (p) => fmtEv(p, step), (p) => set({ exposure_comp: Math.round(p) }, false))));
    }
    if (caps.ae_lock) expRows.push(k.row('AE lock', 'Freeze the current brightness.', k.switchBtn('cam-img-ae', cur.ae_lock, () => set({ ae_lock: !cur.ae_lock }))));
  }

  // ---- white balance
  const wbs = caps.white_balances?.length ? caps.white_balances : ['auto'];
  const wbRows = [
    k.row('White balance', 'Colour temperature of the picture.', h('select', {
      id: 'cam-img-wb', 'aria-label': 'White balance', disabled: wbs.length < 2, onchange: (e) => set({ white_balance: e.target.value }),
    }, wbs.map((v) => h('option', { value: v, selected: v === cur.white_balance }, WB_NAMES[v] ?? v)))),
  ];
  if (cur.white_balance === 'auto' && caps.awb_lock) {
    wbRows.push(k.row('AWB lock', 'Freeze the current colour balance.', k.switchBtn('cam-img-awb', cur.awb_lock, () => set({ awb_lock: !cur.awb_lock }))));
  }

  // ---- anti-flicker, stabilization
  const abs = AB_NAMES.filter(([v]) => v === 'auto' || (caps.antibandings ?? []).includes(v));
  const rest = [];
  if (abs.length > 1) {
    rest.push(k.row('Anti-flicker', 'Match your mains frequency (50 Hz in most of the world, 60 Hz in the Americas) to stop banding under lamps.',
      seg('Anti-flicker', abs, cur.antibanding, (v) => set({ antibanding: v }))));
  }
  if (caps.stabilization) rest.push(k.row('Stabilization', 'Steadies the picture, may crop a little.', k.switchBtn('cam-img-stab', cur.stabilization, () => set({ stabilization: !cur.stabilization }))));

  const note = activeCaps(c, k.lens) ? null : h('p', { class: 'muted small cam-img-note' }, c.phone
    ? 'This phone did not report what it supports. It applies what it can and ignores the rest.'
    : 'No phone connected. Settings are saved and sent when the camera starts; the phone applies what it supports.');
  const readout = h('p', { id: 'cam-img-readout', class: 'muted small cam-img-readout', 'aria-live': 'off', hidden: !c.readout }, readoutText(c.readout));

  return h('section', { class: 'card sect', id: 'cam-image' },
    h('h2', null, 'Image'), note, readout,
    ...focusRows, ...expRows, ...wbRows, ...rest,
    k.row('Defaults', 'Back to automatic focus, exposure and white balance.',
      h('button', { id: 'cam-img-reset', type: 'button', class: 'btn sm', onclick: () => { S.settings = { ...S.settings, camera_image: { ...DEFAULTS } }; k.rerender(); save(k.patch); } }, 'Reset to auto')));
}
