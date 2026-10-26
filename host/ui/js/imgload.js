// SPDX-License-Identifier: GPL-3.0-or-later
// Turn any image the browser can decode into PNG/WebP/JPEG bytes for POST /api/icons, and wire up drops.

import { api } from './api.js';
import { toast } from './dom.js';
import { S, select, touch } from './store.js';

const MAX_SIDE = 288;
const MAX_BYTES = 2 * 1024 * 1024;
const NATIVE = ['image/png', 'image/webp', 'image/jpeg'];

const isSvg = (f) => f.type === 'image/svg+xml' || /\.svg$/i.test(f.name || '');
const extType = (f) => ({ png: 'image/png', webp: 'image/webp', jpg: 'image/jpeg', jpeg: 'image/jpeg' })[(f.name || '').split('.').pop().toLowerCase()] || '';

export const looksLikeImage = (f) => !!f && (/^image\//.test(f.type) || isSvg(f) || /\.(png|webp|jpe?g|gif|bmp|avif)$/i.test(f.name || ''));

function loadImage(src) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('decode failed'));
    img.src = src;
  });
}

/** Intrinsic size of an SVG source: width/height attributes, else the viewBox, else 0 (unknown). */
function svgSize(text) {
  const root = new DOMParser().parseFromString(text, 'image/svg+xml').documentElement;
  if (!root || root.localName !== 'svg') throw new Error('not an SVG');
  const num = (v) => { const m = /^\s*([\d.]+)\s*(px)?\s*$/.exec(v || ''); return m ? parseFloat(m[1]) : 0; };
  let w = num(root.getAttribute('width'));
  let h = num(root.getAttribute('height'));
  if (!w || !h) {
    const vb = (root.getAttribute('viewBox') || '').trim().split(/[\s,]+/).map(parseFloat);
    if (vb.length === 4 && vb[2] > 0 && vb[3] > 0) { w = vb[2]; h = vb[3]; }
  }
  return { root, w, h };
}

// "Fit" logos keep a transparent margin: with >= 20 % transparent pixels the phone and the preview classify them as
// CUTOUT (drawn fitted above the title) instead of ARTWORK (full bleed with the title over the image). Round 24.
const FIT_SIDE = Math.round(MAX_SIDE * 0.76);

/** Draw `img` (natural size w x h) fitted and centred on a transparent 288x288 canvas and export a PNG. */
function toPng(img, w, h, upscale, box = MAX_SIDE) {
  const canvas = document.createElement('canvas');
  canvas.width = MAX_SIDE;
  canvas.height = MAX_SIDE;
  const k = Math.min(box / w, box / h, upscale ? Infinity : 1);
  const sw = Math.max(1, Math.round(w * k));
  const sh = Math.max(1, Math.round(h * k));
  canvas.getContext('2d').drawImage(img, Math.round((MAX_SIDE - sw) / 2), Math.round((MAX_SIDE - sh) / 2), sw, sh);
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('PNG export failed'))), 'image/png'));
}

async function rasteriseSvg(file, box = MAX_SIDE) {
  const text = await file.text();
  const { root, w, h } = svgSize(text);
  let src = text;
  // Chromium/WebKit refuse to draw an SVG <img> with no intrinsic size: give it one.
  if (!root.getAttribute('width') || !root.getAttribute('height') || /%/.test(root.getAttribute('width') + root.getAttribute('height'))) {
    const bw = w || 288;
    const bh = h || 288;
    root.setAttribute('width', String(bw));
    root.setAttribute('height', String(bh));
    if (!root.getAttribute('viewBox')) root.setAttribute('viewBox', `0 0 ${bw} ${bh}`);
    src = new XMLSerializer().serializeToString(root);
  }
  const url = URL.createObjectURL(new Blob([src], { type: 'image/svg+xml' }));
  try {
    const img = await loadImage(url);
    return await toPng(img, w || img.naturalWidth || 288, h || img.naturalHeight || 288, true, box);
  } finally {
    URL.revokeObjectURL(url);
  }
}

async function rasteriseBitmap(file, fit = false) {
  const url = URL.createObjectURL(file);
  try {
    const img = await loadImage(url);
    return await toPng(img, img.naturalWidth, img.naturalHeight, fit, fit ? FIT_SIDE : MAX_SIDE);
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** Natural pixel size of a raster file (for the "> 512 px" check). */
async function rasterSize(file) {
  const url = URL.createObjectURL(file);
  try {
    const img = await loadImage(url);
    return Math.max(img.naturalWidth, img.naturalHeight);
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** Returns a Blob/File the server accepts (PNG/WebP/JPEG), converting where needed. Throws on decode failure.
 *  `fit`: a logo (app/site icon, "Fit above title"): always re-drawn with a transparent margin, small ones scaled up. */
export async function prepareIcon(file, { fit = false } = {}) {
  if (isSvg(file)) return rasteriseSvg(file, fit ? FIT_SIDE : MAX_SIDE);
  if (fit) return rasteriseBitmap(file, true);
  const type = NATIVE.includes(file.type) ? file.type : extType(file);
  if (NATIVE.includes(type)) {
    // Fits as-is unless it is large; the sniff on the server decides by bytes, so keep the original bytes then.
    if (file.size <= MAX_BYTES && (await rasterSize(file)) <= 512) return file;
  }
  return rasteriseBitmap(file);
}

/** Upload `file` as an icon and return its hash, toasting a clear message on failure ('' on failure). */
export async function uploadIconFile(file) {
  let blob;
  try {
    blob = await prepareIcon(file);
  } catch {
    toast(`Could not read this image: ${file.name || 'pasted image'}`, 'error');
    return '';
  }
  try {
    return (await api.uploadIcon(blob)).hash;
  } catch (err) {
    toast(`Upload failed: ${err.message}`, 'error');
    return '';
  }
}

/** Set the icon of control `c` (page control or rail key) from a file and select it, saving like the inspector does. */
export async function setIconFromFile(c, file, isRail = false) {
  const hash = await uploadIconFile(file);
  if (!hash) return false;
  select(isRail ? { railKey: c.id } : { id: c.id });
  c.icon_hash = hash;
  touch('all');
  return true;
}

// WebKitGTK (the Linux window) reports a file dragged from a file manager as text/uri-list with no File
// objects; the shell then delivers the bytes through window.__ndNativeDrop instead of the drop event.
const hasFiles = (e) => [...(e.dataTransfer?.types || [])].some((t) => t === 'Files' || t === 'text/uri-list');
const firstImage = (e) => [...(e.dataTransfer?.files || [])].find(looksLikeImage);

/** Make `el` accept dropped image files; `onFile(file)` runs on drop. Adds .dz-over while dragging over it. */
export function dropTarget(el, onFile) {
  let depth = 0;
  const off = () => { depth = 0; el.classList.remove('dz-over'); };
  el.classList.add('nd-drop');
  el.__ndDrop = onFile;
  el.addEventListener('dragenter', (e) => { if (!hasFiles(e)) return; e.preventDefault(); depth++; el.classList.add('dz-over'); });
  el.addEventListener('dragover', (e) => { if (!hasFiles(e)) return; e.preventDefault(); e.stopPropagation(); e.dataTransfer.dropEffect = 'copy'; el.classList.add('dz-over'); });
  el.addEventListener('dragleave', () => { if (--depth <= 0) off(); });
  el.addEventListener('drop', (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    e.stopPropagation();
    off();
    const f = firstImage(e) || e.dataTransfer.files[0];
    if (!f) return; // uri-list only (WebKitGTK): the shell's native drop handler delivers the file
    onFile(f);
  });
}

/** Called by the Linux window (wry drag-drop handler) with a dropped file read by the agent:
 *  `{ x, y, name, mime, b64 }` or `{ x, y, error }`; x/y are CSS pixels in the viewport. */
window.__ndNativeDrop = (d) => {
  document.querySelectorAll('.dz-over').forEach((el) => el.classList.remove('dz-over'));
  if (d.error) { toast(d.error, 'error'); return; }
  const target = document.elementFromPoint(d.x, d.y)?.closest('.nd-drop');
  if (!target?.__ndDrop) { toast('Drop the image on a key or on the icon box.', 'info'); return; }
  const bin = atob(d.b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  target.__ndDrop(new File([bytes], d.name, { type: d.mime }));
};

/** Image from a paste event, if any. */
export const pastedImage = (e) => [...(e.clipboardData?.files || [])].find(looksLikeImage);

let guarded = false;
/** Stop the browser/WebView navigating to a file dropped outside a drop target. */
export function installDropGuard() {
  if (guarded) return;
  guarded = true;
  for (const t of ['dragover', 'drop']) window.addEventListener(t, (e) => { if (hasFiles(e)) e.preventDefault(); });
}
