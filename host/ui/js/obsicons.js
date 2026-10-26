// SPDX-License-Identifier: GPL-3.0-or-later
// Rasterise icons.js glyphs to 144x144 PNGs and upload them as key icons (browser only).

import { api } from './api.js';
import { iconSvgMarkup } from './icons.js';

const SIZE = 144;
const PAD = 24;

function loadImage(src) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('could not draw the glyph'));
    img.src = src;
  });
}

async function glyphPng(name) {
  const img = await loadImage(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(iconSvgMarkup(name))}`);
  const canvas = document.createElement('canvas');
  canvas.width = SIZE;
  canvas.height = SIZE;
  canvas.getContext('2d').drawImage(img, PAD, PAD, SIZE - 2 * PAD, SIZE - 2 * PAD);
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('PNG export failed'))), 'image/png'));
}

/** Upload each distinct glyph once; returns { glyphName: icon_hash }. Missing ones are left out (keys then show text only). */
export async function uploadGlyphs(names) {
  const out = {};
  await Promise.all([...new Set(names)].map(async (n) => {
    try {
      const png = await glyphPng(n);
      const { hash } = await api.uploadIcon(png);
      if (hash) out[n] = hash;
    } catch { /* leave this glyph out */ }
  }));
  return out;
}
