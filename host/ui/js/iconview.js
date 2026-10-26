// SPDX-License-Identifier: GPL-3.0-or-later
// Icon rendering by class (docs/PLAN.md round 24): GLYPH icons are drawn as a CSS mask filled with currentColor so
// they follow the surrounding ink; CUTOUT and ARTWORK stay plain <img>s. No DOM access at module top level.

import { h } from './dom.js';

const SIZE = 48;
const classes = new Map(); // url -> 'glyph' | 'cutout' | 'artwork'

/** Classify RGBA bytes (canvas order) as 'glyph' | 'cutout' | 'artwork'. */
export function classifyPixels(rgba) {
  const total = rgba.length >> 2;
  let transparent = 0, opaque = 0, coloured = 0, light = 0, dark = 0;
  for (let i = 0; i < rgba.length; i += 4) {
    const a = rgba[i + 3];
    if (a < 16) { transparent++; continue; }
    if (a < 128) continue;
    opaque++;
    const r = rgba[i], g = rgba[i + 1], b = rgba[i + 2];
    if (Math.max(r, g, b) - Math.min(r, g, b) > 40) coloured++;
    const luma = (299 * r + 587 * g + 114 * b) / 1000;
    if (luma >= 160) light++;
    if (luma <= 96) dark++;
  }
  if (total === 0 || transparent < total * 0.2) return 'artwork';
  if (opaque > 0 && coloured <= opaque * 0.05 && (light >= opaque * 0.95 || dark >= opaque * 0.95)) return 'glyph';
  return 'cutout';
}

function classifyImage(img) {
  try {
    const cv = document.createElement('canvas');
    cv.width = cv.height = SIZE;
    const ctx = cv.getContext('2d', { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, SIZE, SIZE);
    return classifyPixels(ctx.getImageData(0, 0, SIZE, SIZE).data);
  } catch {
    return 'artwork';
  }
}

function maskEl(url, cls, alt) {
  const u = `url("${url.replace(/["\\\n]/g, encodeURIComponent)}")`;
  return h('span', {
    class: `${cls || ''} ico-mask`.trim(),
    style: `-webkit-mask-image:${u};mask-image:${u}`,
    role: alt ? 'img' : null,
    'aria-label': alt || null,
    'aria-hidden': alt ? null : 'true',
  });
}

/**
 * An element for an icon URL, returned immediately. Glyphs become a masked <span> (colour = currentColor); anything
 * else an <img>. The class is learned from the pixels after load and cached, and a placeholder <img> is swapped in
 * place when it turns out to be a glyph.
 */
export function iconView(url, cls = '', alt = '') {
  if (classes.get(url) === 'glyph') return maskEl(url, cls, alt);
  const img = h('img', { class: cls || null, src: url, alt, draggable: 'false' });
  if (!classes.has(url)) {
    const learn = () => {
      let c = classes.get(url);
      if (!c) { c = classifyImage(img); classes.set(url, c); }
      if (c === 'glyph' && img.isConnected !== false) img.replaceWith(maskEl(url, cls, alt));
    };
    if (img.complete && img.naturalWidth > 0) learn();
    else img.addEventListener('load', learn, { once: true });
  }
  return img;
}

/** Relative luminance (0..1) of a #rrggbb colour. */
export function luminance(hex) {
  const m = /^#?([0-9a-f]{6})/i.exec(hex || '');
  if (!m) return 0;
  const lin = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
  const n = parseInt(m[1], 16);
  return 0.2126 * lin((n >> 16) & 255) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255);
}

/** Ink for a tinted key face: dark above the contract threshold (0.40), else light. */
export function tintInk(bgHex) {
  return luminance(bgHex) > 0.4 ? '#16181B' : '#F7F5F2';
}
