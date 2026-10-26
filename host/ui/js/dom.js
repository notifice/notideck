// SPDX-License-Identifier: GPL-3.0-or-later
// Tiny DOM helpers. All text goes through textContent / text nodes, never innerHTML.

const LATE = new Set(['value', 'checked', 'selected']);
const PROPS = new Set(['value', 'checked', 'disabled', 'selected', 'readOnly']);

export function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  const late = [];
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style') el.style.cssText = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (LATE.has(k)) late.push([k, v]);
    else if (PROPS.has(k)) el[k] = v;
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  append(el, kids);
  for (const [k, v] of late) el[k] = v;
  return el;
}

function append(el, kids) {
  for (const k of kids.flat(Infinity)) {
    if (k == null || k === false) continue;
    el.append(k.nodeType ? k : document.createTextNode(String(k)));
  }
}

const SVG_NS = 'http://www.w3.org/2000/svg';
export function svg(tag, attrs, ...kids) {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) el.setAttribute(k, String(v));
  append(el, kids);
  return el;
}

export const $ = (sel, root = document) => root.querySelector(sel);

export function toast(msg, kind = 'info') {
  const box = $('#toasts');
  const t = h('p', { class: `toast ${kind}` }, msg);
  box.append(t);
  setTimeout(() => {
    t.classList.add('out');
    setTimeout(() => t.remove(), 300);
  }, 3600);
}

export function debounce(fn, ms) {
  let id = 0;
  return (...a) => {
    clearTimeout(id);
    id = setTimeout(() => fn(...a), ms);
  };
}

export const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
